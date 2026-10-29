use super::*;
use skylive_game_capture_protocol::{self as wire,field as f,state as phase,mapping::Mapping};
mod target;mod inject;mod gpu;
use target::Process;
static PINNED:Mutex<Vec<(u32,u64)>>=Mutex::new(Vec::new());
const PIXEL_BYTES:usize=128*1024*1024;
static NATIVE_BYTES:std::sync::atomic::AtomicUsize=std::sync::atomic::AtomicUsize::new(0);
struct NativeCredit(usize);
impl NativeCredit {fn acquire()->Result<Self,GameError>{let bytes=3*wire::MAX_BYTES;
    NATIVE_BYTES.fetch_update(Ordering::AcqRel,Ordering::Acquire,|n|n.checked_add(bytes).filter(|n|*n<=256*1024*1024)).map_err(|_|GameError::Limit("game native logical copy budget256MiB"))?;Ok(Self(bytes))}}
impl Drop for NativeCredit {fn drop(&mut self){NATIVE_BYTES.fetch_sub(self.0,Ordering::AcqRel);}}
struct Pin {key:(u32,u64),inserted:bool,persistent:bool}
impl Pin {
    fn reserve(target:&GameCaptureTarget)->Result<Self,GameError>{
        let mut pinned=PINNED.lock().map_err(|_|GameError::Closed)?;
        pinned.retain(|(pid,created)|{
            let handle=unsafe{target::OpenProcess(0x00101000,0,*pid)};
            if handle.is_null(){return std::io::Error::last_os_error().raw_os_error()!=Some(87);}
            let alive=unsafe{target::WaitForSingleObject(handle,0)}==258&&target::creation(handle).is_ok_and(|v|v==*created);
            unsafe{target::CloseHandle(handle);}alive
        });
        let key=(target.pid,target.creation_time);let inserted=!pinned.contains(&key);
        if inserted {if pinned.len()>=wire::MAX_PINNED_TARGETS{return Err(GameError::Limit("two process-lifetime game hook targets already pinned; close a previous target process"));}pinned.push(key);}
        Ok(Self {key,inserted,persistent:!inserted})
    }
}
impl Drop for Pin {fn drop(&mut self){if self.inserted&&!self.persistent{if let Ok(mut pins)=PINNED.lock(){pins.retain(|key|*key!=self.key);}}}}
struct Stream {process:Process,map:Arc<Mapping>,binding:Arc<GameCaptureBinding>,readback:Option<gpu::Readback>,initialized:bool,
    last:u64,began:Instant,last_frame:Option<(Arc<RgbaFrame>,gpu::Descriptor,MediaTime)>,_pin:Pin,_native_credit:NativeCredit}
#[link(name="ole32")]
unsafe extern "system" {fn CoCreateGuid(value:*mut [u64;2])->i32;}
impl Stream {
    fn start(binding:Arc<GameCaptureBinding>,fresh:&impl Fn()->bool)->Result<Self,GameError>{
        let credit=NativeCredit::acquire()?;
        let process=Process::open(binding.target(),true)?;let mut pin=Pin::reserve(binding.target())?;
        let mut init=wire::Init::default();let mut token=[0u64;2];
        let hr=unsafe{CoCreateGuid(&mut token)};if hr<0||token==[0;2]{return Err(GameError::Native(format!("Game IPC GUID HRESULT {hr:#x}")));}
        let name=format!("Local\\SkyLive.Game.{:016x}{:016x}",token[0],token[1]);
        for (slot,value) in init.mapping.iter_mut().zip(name.encode_utf16()){*slot=value;}
        let executable:Vec<u16>=binding.target().executable.encode_utf16().collect();if executable.len()>=init.executable.len(){return Err(GameError::Limit("game executable4095 UTF16 units"));}
        init.executable[..executable.len()].copy_from_slice(&executable);
        init.pid=binding.target().pid;init.creation=binding.target().creation_time;init.hwnd=binding.target().hwnd;
        init.host_pid=unsafe{target::GetCurrentProcessId()};init.host_creation=target::creation(unsafe{target::GetCurrentProcess()})?;
        init.fps=binding.max_fps();init.token=token;
        let map=Arc::new(Mapping::create(&init.mapping).map_err(|e|GameError::Native(e.to_string()))?);let h=map.header();
        for (field,value) in [(f::MAGIC,wire::MAGIC),(f::ABI,wire::ABI),(f::HOST_PID,u64::from(init.host_pid)),(f::PID,u64::from(init.pid)),
            (f::CREATION,init.creation),(f::HWND,init.hwnd),(f::TOKEN0,token[0]),(f::TOKEN1,token[1]),(f::FPS,u64::from(init.fps)),(f::HOST_CREATION,init.host_creation)] {h.store(field,value);}
        let current=||{let valid=fresh();if !valid{h.store(f::STOP,1);}valid};
        let mut possibly_loaded=false;let mut start_may_run=false;
        let result=inject::inject(&process,&init,&current,&mut possibly_loaded,&mut start_may_run);
        pin.persistent|=possibly_loaded;
        let mut stream=Self {process,map,binding,readback:None,initialized:start_may_run,last:0,began:Instant::now(),last_frame:None,_pin:pin,_native_credit:credit};
        if let Err(error)=result {
            // A failed receipt after remote entry may still have started the
            // owned DLL worker. Revoke it before releasing mapping/copy credits.
            stream.map.header().store(f::STOP,1);stream.stop();return Err(error);
        }
        Ok(stream)
    }
    fn matches(&self,binding:&GameCaptureBinding)->bool{self.binding.target().same_process_window(binding.target())&&self.binding.max_fps()==binding.max_fps()}
    fn poll(&mut self,free:usize)->Result<Option<(RgbaFrame,gpu::Descriptor)>,GameError>{
        self.process.verify()?;let h=self.map.header();
        if h.load(f::ERROR)!=0{return Err(GameError::AfterInjection(format!("Game hook status {} code {:#x}, PID {} HWND {}",h.load(f::STATE),h.load(f::ERROR),self.binding.target().pid,self.binding.target().hwnd)));}
        if matches!(h.load(f::STATE),phase::RETIRED|phase::FAILED){return Err(GameError::AfterInjection("Game hook retired before its source was stopped".into()));}
        let Some(descriptor)=gpu::Descriptor::read(h)? else{
            if self.began.elapsed()>Duration::from_secs(10){return Err(GameError::Unsupported("No matching D3D11 Present/Present1 SDR RGBA/BGRA8 backbuffer arrived; other graphics APIs/custom swapchain implementations are not supported"));}return Ok(None);
        };
        if descriptor.sequence==self.last&&self.readback.as_ref().is_none_or(|v|v.generation()==descriptor.generation){return Ok(None);}
        if self.readback.as_ref().is_none_or(|v|v.generation()!=descriptor.generation){
            // Retire/unmap/release old keyed generation before allocating new.
            self.readback=None;self.readback=Some(gpu::Readback::open(descriptor)?);h.store(f::CONSUMER_GENERATION,descriptor.generation);
        }
        match self.readback.as_mut().ok_or(GameError::Invalid("game readback owner"))?.poll(descriptor,h,free)? {
            Some((image,actual))=>{self.last=actual.sequence;Ok(Some((image,actual)))},None=>Ok(None),
        }
    }
    fn stop(&mut self){
        self.map.header().store(f::STOP,1);self.readback=None;self.last_frame=None;
        if self.initialized {while self.process.alive()&&!matches!(self.map.header().load(f::STATE),phase::RETIRED|phase::FAILED){thread::sleep(Duration::from_millis(5));}}
        self.initialized=false;
        if self.map.header().load(f::STATE)==phase::FAILED {eprintln!("Game hook retirement: PID {} creation {} HWND {} executable {} status {} code {:#x}",self.binding.target().pid,self.binding.target().creation_time,self.binding.target().hwnd,self.binding.target().executable,self.map.header().load(f::STATE),self.map.header().load(f::ERROR));}
    }
}
impl Drop for Stream{fn drop(&mut self){self.stop();}}
#[link(name="kernel32")]
unsafe extern "system" {fn QueryPerformanceCounter(value:*mut i64)->i32;fn QueryPerformanceFrequency(value:*mut i64)->i32;}
fn presented_at(now:MediaTime,qpc:u64)->Option<MediaTime>{
    let(mut current,mut frequency)=(0i64,0i64);
    if unsafe{QueryPerformanceCounter(&mut current)}==0||unsafe{QueryPerformanceFrequency(&mut frequency)}==0||frequency<=0||current<0||qpc>current as u64{return None;}
    let age=(u128::from(current as u64-qpc)*1_000_000_000/u128::from(frequency as u64)).min(u128::from(u64::MAX)) as u64;
    Some(MediaTime(now.0.saturating_sub(age)))
}
pub(super) fn run(instance:u64,desired:Arc<Mutex<Desired>>,closed:Arc<AtomicBool>,jobs:mpsc::Receiver<GameRequestKey>,reply:mpsc::SyncSender<GameEvent>,publish:Arc<Mutex<Arc<GameSnapshot>>>){
    let mut streams:Vec<Stream>=Vec::new();let mut retained:Vec<Arc<RgbaFrame>>=Vec::new();let mut failed:Vec<(GameCaptureTarget,u32,u64,GameError)>=Vec::new();
    let mut generation=0u64;let mut previous=None;let mut dirty=false;let mut latest=Arc::new(GameSnapshot::default());
    while !closed.load(Ordering::Acquire) {
        retained.retain(|image|Arc::strong_count(image)>1);
        if let Ok(key)=jobs.try_recv(){let _=reply.try_send(GameEvent::Targets {key,result:target::targets()});}
        let Ok(wanted)=desired.lock().map(|state|state.clone())else{break;};
        let fresh=||{!closed.load(Ordering::Acquire)&&desired.lock().is_ok_and(|now|now.session==wanted.session&&now.revision==wanted.revision&&now.epoch==wanted.epoch)};
        let mut problems=Vec::new();let mut changed=previous!=Some((wanted.session,wanted.epoch));
        streams.retain(|stream|wanted.bindings.iter().any(|(_,binding)|stream.matches(binding))&&wanted.authorized.iter().any(|target|stream.binding.target().same_process_window(target)));
        failed.retain(|(_,_,generation,_)|*generation==wanted.authorization_generation);
        for (source,binding) in &wanted.bindings {
            if !wanted.authorized.iter().any(|target|target.same_process_window(binding.target())) {problems.push(GameProblem {source:*source,error:GameError::AuthorizationRequired});continue;}
            if let Some((_,_,_,error))=failed.iter().find(|(target,fps,epoch,_)|*epoch==wanted.authorization_generation&&*fps==binding.max_fps()&&target.same_process_window(binding.target())){problems.push(GameProblem {source:*source,error:error.clone()});continue;}
            if streams.iter().any(|stream|stream.matches(binding)){continue;}
            let result=if streams.len()>=2 {Err(GameError::Limit("two simultaneous game processes"))}
                else if streams.iter().any(|stream|stream.binding.target().pid==binding.target().pid){Err(GameError::Invalid("one selected game window/frame-rate per process"))}
                else {Stream::start(binding.clone(),&fresh)};
            match result {Ok(stream)=>{streams.push(stream);changed=true;},Err(error)=>{failed.push((binding.target().clone(),binding.max_fps(),wanted.authorization_generation,error.clone()));problems.push(GameProblem {source:*source,error});changed=true;}}
        }
        let mut remove=Vec::new();
        for (index,stream) in streams.iter_mut().enumerate(){
            let bytes=retained.iter().map(|image|image.byte_len()).sum::<usize>();let free=if retained.len()<64 {PIXEL_BYTES.saturating_sub(bytes)}else{0};
            match stream.poll(free){
                Ok(Some((image,descriptor)))=>{let image=Arc::new(image);retained.push(image.clone());
                    let at=MediaTime(wanted.at.0.saturating_add(u64::try_from(wanted.sent.elapsed().as_nanos()).unwrap_or(u64::MAX)));
                    let Some(presented)=presented_at(at,descriptor.qpc) else {
                        for (source,binding) in &wanted.bindings {if stream.matches(binding){problems.push(GameProblem {source:*source,error:GameError::Invalid("game QPC timestamp is invalid")});}}
                        remove.push(index);changed=true;continue;
                    };
                    stream.last_frame=Some((image,descriptor,presented));changed=true;},
                Ok(None)=>{},Err(error)=>{
                    for (source,binding) in &wanted.bindings {if stream.matches(binding){problems.push(GameProblem {source:*source,error:error.clone()});}}
                    failed.push((stream.binding.target().clone(),stream.binding.max_fps(),wanted.authorization_generation,error));remove.push(index);changed=true;
                }
            }
        }
        let mut frames=Vec::new();
        for (source,binding) in &wanted.bindings {if let Some(stream)=streams.iter().find(|stream|stream.matches(binding)) {if let Some((image,desc,presented))=&stream.last_frame {
            frames.push((*source,GameFrame {binding:binding.clone(),image:image.clone(),sequence:desc.sequence,generation:desc.generation,qpc:desc.qpc,
                requested_at:*presented,
                dropped:stream.map.header().load(f::DROPPED),throttled:stream.map.header().load(f::THROTTLED)}));
        }}}
        let statuses:Vec<_>=streams.iter().map(|stream|{let h=stream.map.header();GameStreamStatus {target:stream.binding.target().clone(),state:h.load(f::STATE),copies_submitted:h.load(f::SEQUENCE),callbacks:h.load(f::CALLBACKS),dropped:h.load(f::DROPPED),throttled:h.load(f::THROTTLED)}}).collect();
        changed|=latest.streams!=statuses;
        if changed {generation=match generation.checked_add(1){Some(v)=>v,None=>break};latest=Arc::new(GameSnapshot {instance,session:wanted.session,epoch:wanted.epoch,generation,frames,problems,streams:statuses});dirty=true;previous=Some((wanted.session,wanted.epoch));}
        if dirty&&fresh(){if let Ok(mut published)=publish.lock(){*published=latest.clone();dirty=false;}}
        // Error receipt is published before potentially unbounded native
        // retirement. Stop is not falsely reported as completed.
        for index in remove.into_iter().rev(){streams.remove(index);}
        thread::sleep(Duration::from_millis(5));
    }
    drop(streams);drop(latest);drop(publish);drop(reply);drop(jobs);
    while retained.iter().any(|image|Arc::strong_count(image)>1){retained.retain(|image|Arc::strong_count(image)>1);thread::sleep(Duration::from_millis(20));}
}

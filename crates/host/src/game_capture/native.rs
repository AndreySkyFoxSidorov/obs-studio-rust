use super::*;
use skylive_game_capture_protocol::{self as wire,field as f,state as phase,mapping::Mapping};
mod target;mod inject;mod gpu;mod cursor;
use target::Process;
static PINNED:Mutex<Vec<(u32,u64)>>=Mutex::new(Vec::new());
static NEXT_CAPTURE_GENERATION:AtomicU64=AtomicU64::new(1);
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
struct Stream {process:Process,map:Arc<Mapping>,binding:Arc<GameCaptureBinding>,readback:Option<gpu::Readback>,initialized:bool,capture_generation:u64,
    last:u64,began:Instant,last_frame:Option<(Arc<RgbaFrame>,gpu::Descriptor,MediaTime)>,opaque_frame:Option<(u64,u64,Arc<RgbaFrame>)>,cursor_frame:Option<(u64,u64,cursor::Stamp,u64,MediaTime,Result<Arc<RgbaFrame>,GameError>)>,alpha_cursor_frame:Option<(u64,u64,cursor::Stamp,u64,MediaTime,Result<Arc<RgbaFrame>,GameError>)>,cursor_animation:cursor::Animation,cursor_serial:u64,cursor_last:Option<Instant>,_pin:Pin,_native_credit:NativeCredit}
#[link(name="ole32")]
unsafe extern "system" {fn CoCreateGuid(value:*mut [u64;2])->i32;}
impl Stream {
    fn start(binding:Arc<GameCaptureBinding>,fresh:&impl Fn()->bool)->Result<Self,GameError>{
        let credit=NativeCredit::acquire()?;
        let process=Process::open(binding.target().expect("resolved game target"),true)?;let mut pin=Pin::reserve(binding.target().expect("resolved game target"))?;
        let mut init=wire::Init::default();let mut token=[0u64;2];
        let hr=unsafe{CoCreateGuid(&mut token)};if hr<0||token==[0;2]{return Err(GameError::Native(format!("Game IPC GUID HRESULT {hr:#x}")));}
        let name=format!("Local\\SkyLive.Game.{:016x}{:016x}",token[0],token[1]);
        for (slot,value) in init.mapping.iter_mut().zip(name.encode_utf16()){*slot=value;}
        let executable:Vec<u16>=binding.target().expect("resolved game target").executable.encode_utf16().collect();if executable.len()>=init.executable.len(){return Err(GameError::Limit("game executable4095 UTF16 units"));}
        init.executable[..executable.len()].copy_from_slice(&executable);
        init.pid=binding.target().expect("resolved game target").pid;init.creation=binding.target().expect("resolved game target").creation_time;init.hwnd=binding.target().expect("resolved game target").hwnd;
        init.host_pid=unsafe{target::GetCurrentProcessId()};init.host_creation=target::creation(unsafe{target::GetCurrentProcess()})?;
        init.fps=binding.max_fps();init.token=token;
        let map=Arc::new(Mapping::create(&init.mapping).map_err(|e|GameError::Native(e.to_string()))?);let h=map.header();
        for (field,value) in [(f::MAGIC,wire::MAGIC),(f::ABI,wire::ABI),(f::HOST_PID,u64::from(init.host_pid)),(f::PID,u64::from(init.pid)),
            (f::CREATION,init.creation),(f::HWND,init.hwnd),(f::TOKEN0,token[0]),(f::TOKEN1,token[1]),(f::FPS,u64::from(init.fps)),(f::HOST_CREATION,init.host_creation)] {h.store(field,value);}
        let current=||{let valid=fresh();if !valid{h.store(f::STOP,1);}valid};
        let mut possibly_loaded=false;let mut start_may_run=false;
        let result=inject::inject(&process,&init,&current,&mut possibly_loaded,&mut start_may_run);
        pin.persistent|=possibly_loaded;
        let mut stream=Self {process,map,binding,readback:None,initialized:start_may_run,capture_generation:0,last:0,began:Instant::now(),last_frame:None,opaque_frame:None,cursor_frame:None,alpha_cursor_frame:None,cursor_animation:cursor::Animation::default(),cursor_serial:0,cursor_last:None,_pin:pin,_native_credit:credit};
        if let Err(error)=result {
            // A failed receipt after remote entry may still have started the
            // owned DLL worker. Revoke it before releasing mapping/copy credits.
            stream.map.header().store(f::STOP,1);stream.stop();return Err(error);
        }
        Ok(stream)
    }
    fn matches(&self,binding:&GameCaptureBinding)->bool{self.binding.target().zip(binding.target()).is_some_and(|(a,b)|a.same_process_window(b))&&self.binding.max_fps()==binding.max_fps()}
    fn poll(&mut self,free:usize)->Result<Option<(RgbaFrame,gpu::Descriptor)>,GameError>{
        self.process.verify()?;let h=self.map.header();
        if h.load(f::ERROR)!=0{return Err(GameError::AfterInjection(format!("Game hook status {} code {:#x}, PID {} HWND {}",h.load(f::STATE),h.load(f::ERROR),self.binding.target().expect("resolved game target").pid,self.binding.target().expect("resolved game target").hwnd)));}
        if matches!(h.load(f::STATE),phase::RETIRED|phase::FAILED){return Err(GameError::AfterInjection("Game hook retired before its source was stopped".into()));}
        let Some(descriptor)=gpu::Descriptor::read(h)? else{
            if self.began.elapsed()>Duration::from_secs(10){return Err(GameError::Unsupported("No matching D3D11 Present/Present1 SDR RGBA/BGRA8 backbuffer arrived; other graphics APIs/custom swapchain implementations are not supported"));}return Ok(None);
        };
        if descriptor.sequence==self.last&&self.readback.as_ref().is_none_or(|v|v.generation()==descriptor.generation){return Ok(None);}
        if self.readback.as_ref().is_none_or(|v|v.generation()!=descriptor.generation){
            // Retire/unmap/release old keyed generation before allocating new.
            self.readback=None;self.readback=Some(gpu::Readback::open(descriptor)?);self.capture_generation=NEXT_CAPTURE_GENERATION.fetch_update(Ordering::AcqRel,Ordering::Acquire,|v|v.checked_add(1)).map_err(|_|GameError::Limit("game capture generation"))?;h.store(f::CONSUMER_GENERATION,descriptor.generation);
        }
        match self.readback.as_mut().ok_or(GameError::Invalid("game readback owner"))?.poll(descriptor,h,free)? {
            Some((image,actual))=>{self.last=actual.sequence;Ok(Some((image,actual)))},None=>Ok(None),
        }
    }
    fn stop(&mut self){
        self.map.header().store(f::STOP,1);self.readback=None;self.last_frame=None;self.opaque_frame=None;self.cursor_frame=None;self.alpha_cursor_frame=None;
        if self.initialized {while self.process.alive()&&!matches!(self.map.header().load(f::STATE),phase::RETIRED|phase::FAILED){thread::sleep(Duration::from_millis(5));}}
        self.initialized=false;
        if self.map.header().load(f::STATE)==phase::FAILED {eprintln!("Game hook retirement: PID {} creation {} HWND {} executable {} status {} code {:#x}",self.binding.target().expect("resolved game target").pid,self.binding.target().expect("resolved game target").creation_time,self.binding.target().expect("resolved game target").hwnd,self.binding.target().expect("resolved game target").executable,self.map.header().load(f::STATE),self.map.header().load(f::ERROR));}
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
fn resolve_selector(selector:&skylive_core::ObsGameSelector, candidates:&[GameTargetInfo])->Option<GameCaptureTarget>{
    if selector.any_fullscreen {
        let hwnd=target::fullscreen_foreground()?;
        // Match OBS's exclusions: a fullscreen browser, shell or launcher is
        // not a game hook target. Explicit exact-target capture is unchanged.
        const EXCLUDED:&[&str]=&["explorer.exe","steam.exe","battle.net.exe","galaxyclient.exe","skype.exe","uplay.exe","origin.exe","devenv.exe","taskmgr.exe","chrome.exe","discord.exe","firefox.exe","systemsettings.exe","applicationframehost.exe","cmd.exe","shellexperiencehost.exe","winstore.app.exe","searchui.exe","lockapp.exe","windowsinternal.composableshell.experiences.textinput.inputapp.exe"];
        return candidates.iter().find(|value|value.target.hwnd==hwnd &&
            std::path::Path::new(&value.target.executable).file_name().and_then(|name|name.to_str()).is_some_and(|name|!EXCLUDED.iter().any(|excluded|name.eq_ignore_ascii_case(excluded))))
            .map(|value|value.target.clone());
    }
    use skylive_core::{WindowMatchPriority as Priority,WindowTarget};
    let mut parts=selector.window.split(':');
    let decode=|part:&str|part.replace("#3A",":").replace("#22","#");
    let (title,class,executable)=(decode(parts.next()?),decode(parts.next()?),decode(parts.next()?));
    if parts.next().is_some(){return None;}
    let target=WindowTarget::Windows{title,class,executable};
    let priority=match selector.priority {0=>Priority::Class,1=>Priority::Title,2=>Priority::Executable,_=>return None};
    candidates.iter().filter_map(|candidate|{
        let observed=WindowTarget::Windows{title:candidate.target.title.clone(),class:candidate.target.class.clone(),executable:candidate.target.executable.rsplit(['\\','/']).next().unwrap_or("").to_owned()};
        crate::window_capture::matching::windows_rating(&target,&observed,priority).map(|score|(score,&candidate.target))
    }).min_by_key(|(score,_)|*score).map(|(_,target)|target.clone())
}
pub(super) fn run(instance:u64,desired:Arc<Mutex<Desired>>,closed:Arc<AtomicBool>,jobs:mpsc::Receiver<GameRequestKey>,reply:mpsc::SyncSender<GameEvent>,publish:Arc<Mutex<Arc<GameSnapshot>>>){
    let mut streams:Vec<Stream>=Vec::new();let mut retained:Vec<Arc<RgbaFrame>>=Vec::new();let mut failed:Vec<(GameCaptureTarget,u32,u64,GameError)>=Vec::new();
    let mut generation=0u64;let mut previous=None;let mut dirty=false;let mut latest=Arc::new(GameSnapshot::default());
    let mut inventory=Vec::new();let mut last_discovery:Option<Instant>=None;
    let mut previous_resolved:Vec<(Id,Option<GameCaptureTarget>)>=Vec::new();
    while !closed.load(Ordering::Acquire) {
        retained.retain(|image|Arc::strong_count(image)>1);
        if let Ok(key)=jobs.try_recv(){let _=reply.try_send(GameEvent::Targets {key,result:target::targets()});}
        let Ok(mut wanted)=desired.lock().map(|state|state.clone())else{break;};
        let authored=wanted.bindings.clone();
        if authored.iter().any(|(source,binding)|binding.target().is_none()&&binding.obs_selector().is_some()&&!wanted.suppressed_selectors.contains(source))
            && last_discovery.is_none_or(|last|last.elapsed()>=Duration::from_secs(1)) {
            // A transient enumeration error must not retire a still-valid
            // hooked stream. Process::verify independently checks its exact
            // lifetime, executable and HWND on every poll.
            if let Ok(found)=target::targets(){inventory=found;}last_discovery=Some(Instant::now());
        }
        for (source,binding) in &mut wanted.bindings {
            if binding.target().is_some()||wanted.suppressed_selectors.contains(source){continue;}
            if let Some(target)=binding.obs_selector().and_then(|selector|if selector.hotkey_mode {
                wanted.hotkey_targets.iter().find(|(id,_)|id==source).and_then(|(_,identity)|inventory.iter().find(|candidate|candidate.target.hwnd==identity.hwnd&&candidate.target.pid==identity.pid&&candidate.target.creation_time==identity.creation_time).map(|candidate|candidate.target.clone()))
            } else {resolve_selector(selector,&inventory)}) {
                if let Ok(resolved)=binding.as_ref().clone().with_runtime_target(target.clone()) {
                    *binding=Arc::new(resolved);
                    if !wanted.authorized.iter().any(|old|old.same_process_window(&target)){wanted.authorized.push(target);}
                }
            }
        }
        let resolved:Vec<_>=wanted.bindings.iter().map(|(source,binding)|(*source,binding.target().cloned())).collect();
        let resolution_changed=resolved!=previous_resolved;previous_resolved=resolved;
        let fresh=||{!closed.load(Ordering::Acquire)&&desired.lock().is_ok_and(|now|now.session==wanted.session&&now.revision==wanted.revision&&now.epoch==wanted.epoch)};
        let mut problems=Vec::new();let mut changed=resolution_changed||previous!=Some((wanted.session,wanted.epoch));
        streams.retain(|stream|wanted.bindings.iter().any(|(_,binding)|stream.matches(binding))&&wanted.authorized.iter().any(|target|stream.binding.target().expect("resolved game target").same_process_window(target)));
        // Automatic selectors may follow many process lifetimes. Retain
        // failures only while that exact target/FPS is still requested.
        failed.retain(|(target,fps,generation,_)|*generation==wanted.authorization_generation&&wanted.bindings.iter().any(|(_,binding)|binding.max_fps()==*fps&&binding.target().is_some_and(|current|current.same_process_window(target))));
        for (source,binding) in &wanted.bindings {
            if binding.target().is_none(){problems.push(GameProblem {source:*source,error:if wanted.suppressed_selectors.contains(source){GameError::AuthorizationRequired}else{GameError::Invalid("Waiting for the OBS game window")}});continue;}
            if !wanted.authorized.iter().any(|target|target.same_process_window(binding.target().expect("resolved game target"))) {problems.push(GameProblem {source:*source,error:GameError::AuthorizationRequired});continue;}
            if let Some((_,_,_,error))=failed.iter().find(|(target,fps,epoch,_)|*epoch==wanted.authorization_generation&&*fps==binding.max_fps()&&target.same_process_window(binding.target().expect("resolved game target"))){problems.push(GameProblem {source:*source,error:error.clone()});continue;}
            if streams.iter().any(|stream|stream.matches(binding)){continue;}
            let result=if streams.len()>=2 {Err(GameError::Limit("two simultaneous game processes"))}
                else if streams.iter().any(|stream|stream.binding.target().expect("resolved game target").pid==binding.target().expect("resolved game target").pid){Err(GameError::Invalid("one selected game window/frame-rate per process"))}
                else {Stream::start(binding.clone(),&fresh)};
            match result {Ok(stream)=>{streams.push(stream);changed=true;},Err(error)=>{failed.push((binding.target().expect("resolved game target").clone(),binding.max_fps(),wanted.authorization_generation,error.clone()));problems.push(GameProblem {source:*source,error});changed=true;}}
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
                    failed.push((stream.binding.target().expect("resolved game target").clone(),stream.binding.max_fps(),wanted.authorization_generation,error));remove.push(index);changed=true;
                }
            }
        }
        // A single hooked process can feed sources with different alpha
        // settings. Keep the original backbuffer untouched and derive an
        // opaque variant only if a source needs one. The extra allocation and
        // escaped snapshots share the existing resident-pixel budget.
        for stream in &mut streams {
            if !wanted.bindings.iter().any(|(_,binding)|stream.matches(binding)&&!binding.allow_transparency()){stream.opaque_frame=None;continue;}
            let Some((raw,desc,_))=&stream.last_frame else{stream.opaque_frame=None;continue;};
            if stream.opaque_frame.as_ref().is_some_and(|(generation,sequence,_)|*generation==desc.generation&&*sequence==desc.sequence){continue;}
            stream.opaque_frame=None;
            retained.retain(|old|Arc::strong_count(old)>1);
            let opaque=if raw.pixels().chunks_exact(4).all(|pixel|pixel[3]==255){Some(raw.clone())}else{
                let used=retained.iter().map(|old|old.byte_len()).sum::<usize>();
                let free=if retained.len()<64{PIXEL_BYTES.saturating_sub(used)}else{0};
                if raw.byte_len()>free{None}else{
                    let mut pixels=Vec::new();
                    if pixels.try_reserve_exact(raw.byte_len()).is_err(){None}else{
                        pixels.extend_from_slice(raw.pixels());
                        for pixel in pixels.chunks_exact_mut(4){pixel[3]=255;}
                        RgbaFrame::new(raw.width(),raw.height(),pixels).ok().map(|frame|{let frame=Arc::new(frame);retained.push(frame.clone());frame})
                    }
                }
            };
            if let Some(opaque)=opaque{stream.opaque_frame=Some((desc.generation,desc.sequence,opaque));changed=true;}
            else{stream.cursor_frame=None;}
        }
        // A process may feed both cursor-on and cursor-off sources. Compose a
        // separate bounded image per game frame or cursor change, never mutate
        // the shared clean image, and account escaped frames in the same budget.
        for stream in &mut streams {
          for transparent in [false,true] {
            if !wanted.bindings.iter().any(|(_,binding)|stream.matches(binding)&&binding.capture_cursor()&&binding.allow_transparency()==transparent){if transparent{stream.alpha_cursor_frame=None;}else{stream.cursor_frame=None;}continue;}
            let Some((raw,desc,_))=&stream.last_frame else{continue;};
            let image=if transparent {raw} else {let Some((generation,sequence,image))=&stream.opaque_frame else{stream.cursor_frame=None;continue;};if *generation!=desc.generation||*sequence!=desc.sequence{stream.cursor_frame=None;continue;}image};
            let mut stamp=match cursor::stamp(stream.binding.target().expect("resolved game target")) {
                Ok(stamp)=>stamp,
                Err(error)=>{for (source,binding) in &wanted.bindings {if stream.matches(binding)&&binding.capture_cursor()&&binding.allow_transparency()==transparent{problems.push(GameProblem{source:*source,error:error.clone()});}} changed|=if transparent{stream.alpha_cursor_frame.take().is_some()}else{stream.cursor_frame.take().is_some()};continue;}
            };
            stream.cursor_animation.update(&mut stamp,Instant::now());
            // A budget failure can be transient: the previous published
            // snapshot may still own both clean and cursor-on 4K frames until
            // the next publication. Retry the same sequence after those
            // leases are released instead of caching the error forever.
            let cached=if transparent{&mut stream.alpha_cursor_frame}else{&mut stream.cursor_frame};
            let retrying=cached.as_ref().is_some_and(|(generation,sequence,old,_,_,result)|*generation==desc.generation&&*sequence==desc.sequence&&*old==stamp&&matches!(result,Err(GameError::Limit("game cursor overlay copy budget"))));
            if !retrying&&cached.as_ref().is_some_and(|(generation,sequence,old,_,_,_)|*generation==desc.generation&&*sequence==desc.sequence&&*old==stamp){continue;}
            let same_game_frame=cached.as_ref().is_some_and(|(generation,sequence,_,_,_,_)|*generation==desc.generation&&*sequence==desc.sequence);
            // Do not copy a full 4K frame at the 5ms worker polling rate when
            // only the pointer moves; honor the source's capture FPS limit.
            let interval=Duration::from_nanos(1_000_000_000/u64::from(stream.binding.max_fps().max(1)));
            if same_game_frame&&!retrying&&stream.cursor_last.is_some_and(|last|last.elapsed()<interval){continue;}
            *cached=None;
            retained.retain(|old|Arc::strong_count(old)>1);
            let used=retained.iter().map(|old|old.byte_len()).sum::<usize>();
            let free=if retained.len()<64 {PIXEL_BYTES.saturating_sub(used)} else {0};
            let result=cursor::overlay(stream.binding.target().expect("resolved game target"),image,free,stamp,transparent).map(|result|match result {
                Some(frame)=>{let frame=Arc::new(frame);retained.push(frame.clone());frame},None=>image.clone()
            });
            let Some(serial)=stream.cursor_serial.checked_add(1) else {break;};stream.cursor_serial=serial;
            let at=MediaTime(wanted.at.0.saturating_add(u64::try_from(wanted.sent.elapsed().as_nanos()).unwrap_or(u64::MAX)));
            stream.cursor_last=Some(Instant::now());
            changed|=!retrying||result.is_ok();
            *cached=Some((desc.generation,desc.sequence,stamp,serial,at,result));
          }
        }
        let mut frames=Vec::new();
        for (source,binding) in &wanted.bindings {if let Some(stream)=streams.iter().find(|stream|stream.matches(binding)) {if let Some((image,desc,presented))=&stream.last_frame {
            let (image,sequence,at)=if binding.capture_cursor(){let cached=if binding.allow_transparency(){&stream.alpha_cursor_frame}else{&stream.cursor_frame};match cached.as_ref(){Some((_,_,_,serial,at,Ok(image)))=>(image.clone(),*serial,*at),Some((_,_,_,_,_,Err(error)))=>{problems.push(GameProblem{source:*source,error:error.clone()});continue;},None=>{if !binding.allow_transparency()&&stream.opaque_frame.is_none(){problems.push(GameProblem{source:*source,error:GameError::Limit("game opaque frame copy budget")});}continue;}}}else if binding.allow_transparency(){(image.clone(),desc.sequence,*presented)}else{match stream.opaque_frame.as_ref(){Some((generation,sequence,opaque)) if *generation==desc.generation&&*sequence==desc.sequence=>(opaque.clone(),desc.sequence,*presented),_=>{problems.push(GameProblem{source:*source,error:GameError::Limit("game opaque frame copy budget")});continue;}}};
            let authored_binding=authored.iter().find(|(id,_)|id==source).map(|(_,binding)|binding.clone()).unwrap_or_else(||binding.clone());
            frames.push((*source,GameFrame {binding:authored_binding,image,sequence,generation:stream.capture_generation,qpc:desc.qpc,
                requested_at:at,
                dropped:stream.map.header().load(f::DROPPED),throttled:stream.map.header().load(f::THROTTLED)}));
        }}}
        let statuses:Vec<_>=streams.iter().map(|stream|{let h=stream.map.header();GameStreamStatus {target:stream.binding.target().expect("resolved game target").clone(),state:h.load(f::STATE),copies_submitted:h.load(f::SEQUENCE),callbacks:h.load(f::CALLBACKS),dropped:h.load(f::DROPPED),throttled:h.load(f::THROTTLED)}}).collect();
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

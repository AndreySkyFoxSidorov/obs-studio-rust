//! A single receive owner; no SDK pointer or native borrowed buffer is sent.
use super::*;
use super::{receiver::{Finder,Receiver,Packet},runtime_windows::Api};
use std::sync::Arc;
const PIXELS:usize=256*1024*1024;
const PCM:usize=4*1024*1024;
const RECEIVE_RETRY:Duration=Duration::from_secs(1);
fn receive_qpc()->Option<u64>{
    use windows::Win32::System::Performance::{QueryPerformanceCounter,QueryPerformanceFrequency};
    let (mut ticks,mut frequency)=(0i64,0i64);
    unsafe {QueryPerformanceFrequency(&mut frequency)}.ok()?;
    unsafe {QueryPerformanceCounter(&mut ticks)}.ok()?;
    if ticks<0||frequency<=0{return None;}
    u64::try_from((ticks as u128)*1_000_000_000/(frequency as u128)).ok()
}
enum Pending {Find {key:NdiRequestKey,finder:Finder,until:Instant},Probe {key:NdiRequestKey,connection:NdiConnection,receiver:Receiver,until:Instant}}
struct Stream {id:Id,ndi:Arc<NdiSource>,receiver:Receiver,video_sequence:u64,audio_sequence:u64,generation:u64,live_generation:Arc<AtomicU64>,clock:Option<(bool,i128,i128)>,format:Option<NativeAudioFormat>,last_video_clock:Option<(NdiVideoClock,i128)>,video_clock_missing:bool,video_clock_epoch:u64,last_audio_clock:Option<(NdiVideoClock,i128)>,audio_clock_missing:bool,audio_clock_epoch:u64,live_audio_clock:Arc<AtomicU64>,delay:super::async_delay::NdiDelay}
impl Drop for Stream {fn drop(&mut self){self.live_generation.store(0,Ordering::Release);self.live_audio_clock.store(0,Ordering::Release);}}
impl Stream {
    fn time(&mut self,timestamp:Option<i64>,timecode:i64,received:i128)->(i128,bool){
        let code=(timecode!=i64::MAX).then_some(timecode);
        // Never mix SDK timestamps and user timecodes in a single A/V clock.
        // If the established kind is absent, arrival timing is marked estimated.
        let raw=match self.clock {Some((true,..))=>timestamp.map(|v|(true,v)),Some((false,..))=>code.map(|v|(false,v)),None=>timestamp.map(|v|(true,v)).or_else(||code.map(|v|(false,v)))};
        let Some((kind,raw))=raw else{return (received,true);};let raw=i128::from(raw)*100;
        // The sender clock is anchored to local packet arrival, so even a
        // stable sender timestamp does not prove capture time or network
        // latency. Keep this mapping marked estimated for downstream audio.
        if let Some((_,origin,local))=self.clock {let mapped=local+raw-origin;if (mapped-received).abs()<=2_000_000_000{return (mapped,true);}self.generation=self.generation.checked_add(1).unwrap_or(u64::MAX);}
        self.clock=Some((kind,raw,received));(received,true)
    }
}
pub(super) fn run(instance:u64,desired:Arc<Mutex<Desired>>,stop:Arc<AtomicBool>,jobs:mpsc::Receiver<Query>,reply:mpsc::SyncSender<NdiEvent>,publish:mpsc::SyncSender<Arc<NdiSnapshot>>,audio:NdiAudioSink,delay_epoch:Arc<AtomicU64>){
    let mut api:Option<Arc<Api>>=None;let mut scope=Scope {session:0,epoch:0,sources:Vec::new(),delays:Vec::new(),delay_upstream:Vec::new()};let mut streams:Vec<Stream>=Vec::new();let mut pending=None;
    let delay_budget=skylive_media::AsyncDelayBudget::new(512*1024*1024,4096).expect("valid NDI delay budget");
    let mut pixels:Vec<Arc<RgbaFrame>>=Vec::new();let mut pcm:Vec<Arc<NdiPcmPacket>>=Vec::new();
    let mut latest=NdiSnapshot {instance,..Default::default()};let mut generation=0u64;let mut attempted:Option<(u64,u64)>=None;let mut retry_at:Option<Instant>=None;let mut next_generation=0u64;let mut dirty=false;
    while !stop.load(Ordering::Acquire){
        pixels.retain(|v|Arc::strong_count(v)>1);pcm.retain(|v|Arc::strong_count(v)>1);
        let Ok(wanted)=desired.lock().map(|v|v.clone()) else {break;};
        let received=i128::from(wanted.at.0)+i128::try_from(wanted.sent.elapsed().as_nanos()).unwrap_or(i128::MAX-i128::from(wanted.at.0));
        let mut changed=false;
        if scope!=wanted.scope {
            // Source IDs are scoped to one document. A new session can reuse
            // an ID and identical settings but must never inherit its native
            // receiver, clock, frame sequence or audio stream identity.
            for stream in &mut streams {stream.delay.reset();stream.audio_clock_missing=true;}
            if scope.session!=wanted.scope.session {streams.clear();}
            else {streams.retain(|s|wanted.scope.sources.iter().any(|(id,ndi)|*id==s.id&&**ndi==*s.ndi));}
            scope=wanted.scope.clone();attempted=None;retry_at=None;latest=NdiSnapshot {runtime_version:api.as_ref().map(|a|a.version.clone()),instance,session:scope.session,epoch:scope.epoch,..Default::default()};for stream in &streams {if let Some(format)=stream.format {latest.formats.insert(stream.id,format);}}changed=true;
        }
        if pending.is_none(){match jobs.try_recv(){
            Ok(query)=>{
                let key=match &query {Query::Discover{key,..}|Query::Prepare{key,..}=>*key};
                let result=if scope.session!=0&&key.session!=scope.session {Err(NdiError::Stale)}else{match &api {Some(value)=>Ok(value.clone()),None=>runtime_windows::load()}};
                if api.is_none(){if let Ok(value)=&result {api=Some(value.clone());attempted=None;retry_at=None;latest.runtime_version=Some(value.version.clone());changed=true;}}
                match (query,result){
                    (Query::Discover{key,local},Ok(api))=>match Finder::new(api,local){Ok(finder)=>pending=Some(Pending::Find {key,finder,until:Instant::now()+Duration::from_secs(1)}),Err(e)=>{let _=reply.try_send(NdiEvent::Discovered {key,result:Err(e)});}},
                    (Query::Prepare{key,connection},Ok(api))=>{if streams.len()>=MAX_RECEIVERS {let _=reply.try_send(NdiEvent::Prepared {key,result:Err(NdiError::Limit("Stop one of four active NDI receivers before probing another"))});}else{match Receiver::new(api,&connection){Ok(receiver)=>pending=Some(Pending::Probe {key,connection,receiver,until:Instant::now()+Duration::from_secs(3)}),Err(e)=>{let _=reply.try_send(NdiEvent::Prepared {key,result:Err(e)});}}}},
                    (Query::Discover{key,..},Err(e))=>{let _=reply.try_send(NdiEvent::Discovered {key,result:Err(e)});},
                    (Query::Prepare{key,..},Err(e))=>{let _=reply.try_send(NdiEvent::Prepared {key,result:Err(e)});},
                }
            },Err(mpsc::TryRecvError::Disconnected)=>break,Err(_)=>{}
        }}
        if attempted!=Some((scope.session,scope.epoch))&&!scope.sources.is_empty()
            &&retry_at.is_none_or(|deadline|Instant::now()>=deadline){
            attempted=Some((scope.session,scope.epoch));
            retry_at=None;
            let load=if let Some(api)=&api {Ok(api.clone())}else{runtime_windows::load()};
            match load {
                Ok(value)=>{latest.runtime_version=Some(value.version.clone());api=Some(value.clone());for (id,ndi) in &scope.sources {if streams.iter().any(|s|s.id==*id){continue;}if streams.len()+usize::from(matches!(pending,Some(Pending::Probe {..})))>=MAX_RECEIVERS {attempted=None;retry_at=Some(Instant::now()+RECEIVE_RETRY);problem(&mut latest,*id,NdiError::Limit("NDI receiver admission waits for the active source probe"));continue;}match Receiver::new(value.clone(),ndi.connection()){
                    Ok(receiver)=>{let Some(next)=next_generation.checked_add(1u64<<32)else{problem(&mut latest,*id,NdiError::Limit("NDI stream identity exhausted"));continue;};next_generation=next;streams.push(Stream {id:*id,ndi:ndi.clone(),receiver,video_sequence:0,audio_sequence:0,generation:next_generation,live_generation:Arc::new(AtomicU64::new(next_generation)),clock:None,format:None,last_video_clock:None,video_clock_missing:false,video_clock_epoch:1,last_audio_clock:None,audio_clock_missing:false,audio_clock_epoch:1,live_audio_clock:Arc::new(AtomicU64::new(0)),delay:super::async_delay::NdiDelay::new(delay_budget.clone())});},
                    Err(e)=>{attempted=None;retry_at=Some(Instant::now()+RECEIVE_RETRY);problem(&mut latest,*id,e);},
                }}},
                Err(e)=>{attempted=None;retry_at=Some(Instant::now()+RECEIVE_RETRY);for (id,_) in &scope.sources {problem(&mut latest,*id,e.clone());}},
            }changed=true;
        }
        if let Some(job)=pending.take(){pending=match job {
            Pending::Find {key,mut finder,until}=>match finder.sources(){
                // Discovery is asynchronous: the first sender can arrive well
                // before other network senders. Return the current finder list
                // only after the bounded discovery window has elapsed.
                Ok(sources) if Instant::now()>=until=>{let _=reply.try_send(NdiEvent::Discovered {key,result:Ok(sources)});None},
                Ok(_)=>Some(Pending::Find {key,finder,until}),Err(e)=>{let _=reply.try_send(NdiEvent::Discovered {key,result:Err(e)});None},
            },
            Pending::Probe {key,connection,mut receiver,until}=>match receiver.capture(0,true){
                Ok(Packet::Video {metadata,..})=>{let result=NdiSource::new(connection,metadata.width,metadata.height).map_err(|e|NdiError::Runtime(e.to_string()));let _=reply.try_send(NdiEvent::Prepared {key,result});None},
                Err(e)=>{let _=reply.try_send(NdiEvent::Prepared {key,result:Err(e)});None},
                Ok(_) if Instant::now()>=until=>{let _=reply.try_send(NdiEvent::Prepared {key,result:Err(NdiError::Unavailable("No actual progressive RGB video frame arrived during NDI source preparation"))});None},
                Ok(_)=>Some(Pending::Probe {key,connection,receiver,until}),
            },
        };}
        for stream in &mut streams {
            // At most eight nonblocking receives per source per fair worker pass.
            for _ in 0..8 {
                let retained=pixels.iter().map(|v|v.byte_len()).sum::<usize>();let available=if pixels.len()<64 {PIXELS.saturating_sub(retained)}else{0};
                let before_receive=receive_qpc();
                let packet=stream.receiver.capture(available,false);
                let after_receive=receive_qpc();
                match packet {
                    Ok(Packet::None)=>{if !stream.receiver.connected(){stream.delay.reset();stream.audio_clock_missing=true;stream.live_generation.store(0,Ordering::Release);if latest.frames.remove(&stream.id).is_some(){changed=true;}changed|=latest.draft_frames.remove(&stream.id).is_some();changed|=problem(&mut latest,stream.id,NdiError::Unavailable("NDI sender is disconnected; waiting to reconnect"));}break;},
                    Ok(Packet::Status)=>{},
                    Ok(Packet::Video {metadata,image:Some(image)})=>{
                        if stream.live_generation.load(Ordering::Acquire)==0 {
                            let Some(next)=stream.generation.checked_add(1) else {changed|=problem(&mut latest,stream.id,NdiError::Limit("NDI stream identity exhausted"));continue;};
                            stream.generation=next;
                        }
                        if available<image.byte_len(){stream.delay.reset();changed|=latest.frames.remove(&stream.id).is_some();changed|=problem(&mut latest,stream.id,NdiError::Limit("NDI resident pixel budget"));break;}
                        stream.video_sequence=stream.video_sequence.saturating_add(1);let image=Arc::new(image);pixels.push(image.clone());
                        let (mapped,estimated)=stream.time(metadata.timestamp_100ns,metadata.timecode_100ns,received);
                        stream.live_generation.store(stream.generation,Ordering::Release);
                        // Use only the established A/V clock kind. If this
                        // packet lacks that clock, preserve absence rather than
                        // labelling arrival or a different clock as source PTS.
                        let original=match stream.clock {
                            Some((true,..))=>metadata.timestamp_100ns.map(|v|(NdiVideoClock::SdkTimestamp,i128::from(v)*100)),
                            Some((false,..))=>(metadata.timecode_100ns!=i64::MAX).then_some((NdiVideoClock::SenderTimecode,i128::from(metadata.timecode_100ns)*100)),
                            None=>None,
                        };
                        if let Some((clock,pts))=original {
                            let jump=stream.last_video_clock.is_some_and(|(old_clock,old_pts)| stream.video_clock_missing || old_clock!=clock || pts.checked_sub(old_pts).is_none_or(|delta| !(0..=1_000_000_000).contains(&delta)));
                            if jump {
                                let Some(epoch)=stream.video_clock_epoch.checked_add(1) else {changed|=problem(&mut latest,stream.id,NdiError::Limit("NDI video clock identity exhausted"));continue;};
                                stream.video_clock_epoch=epoch;
                                // Audio history and video evidence must retire
                                // together when the sender video PTS jumps.
                                let Some(next)=stream.generation.checked_add(1) else {changed|=problem(&mut latest,stream.id,NdiError::Limit("NDI stream identity exhausted"));continue;};
                                stream.generation=next;stream.live_generation.store(next,Ordering::Release);
                                stream.delay.reset();
                                changed|=latest.frames.remove(&stream.id).is_some();
                            }
                            stream.last_video_clock=Some((clock,pts));
                            stream.video_clock_missing=false;
                        } else if stream.last_video_clock.is_some() {stream.video_clock_missing=true;}
                        let frame=NdiFrame {stream_generation:stream.generation,ndi:stream.ndi.clone(),sequence:stream.video_sequence,requested_at:MediaTime(u64::try_from(mapped).unwrap_or(wanted.at.0)),metadata,image:image.into(),native_timestamp_ns:original.map(|(_,pts)|pts),native_clock:original.map(|(clock,_)|clock),native_clock_epoch:stream.video_clock_epoch,mapped_media_ns:mapped,mapped_time_estimated:estimated,receive_qpc_ns:before_receive.zip(after_receive).filter(|(a,b)|a<=b),async_delay:None,draft_raw:None};
                        if scope.delays.iter().any(|(id,_)|*id==stream.id) {
                            latest.draft_frames.insert(stream.id,(frame.clone(),stream.live_generation.clone()));
                            changed=true;
                        } else {changed|=latest.draft_frames.remove(&stream.id).is_some();}
                        let staged=scope.delay_upstream.iter().find(|(id,_)|*id==stream.id).is_some_and(|(_,filters)|filters.iter().any(|filter|
                            matches!(filter.effect,skylive_core::VideoEffect::PersonBackgroundRemoval{..}|skylive_core::VideoEffect::Scroll{..})
                                || matches!(filter.effect,skylive_core::VideoEffect::ImageMask{..})&&filter.mask_image().is_some()
                                || matches!(filter.effect,skylive_core::VideoEffect::ApplyLut{..})&&filter.lut_asset().is_some()));
                        let result=if let Some((_,filters))=scope.delays.iter().find(|(id,_)|*id==stream.id) {
                            let upstream=scope.delay_upstream.iter().find(|(id,_)|*id==stream.id).map_or(&[][..],|(_,filters)|filters.as_slice());
                            stream.delay.push(instance,scope.session,scope.epoch,stream.id,filters,upstream,frame,&delay_epoch,wanted.mask.as_ref())
                        } else {stream.delay.reset();Ok(Some(frame))};
                        match result {
                            Ok(Some(frame))=>{latest.frames.insert(stream.id,frame);latest.problems.retain(|p|p.source!=stream.id);changed=true;},
                            // While the bounded staged worker runs, keep the
                            // last proven output; lack of a new result is not
                            // evidence that its current generation is stale.
                            Ok(None)=>{if !staged {changed|=latest.frames.remove(&stream.id).is_some();}},
                            Err(error)=>{stream.delay.reset();changed|=latest.frames.remove(&stream.id).is_some();changed|=problem(&mut latest,stream.id,error);},
                        }
                    },
                    Ok(Packet::Video {image:None,..})=>{stream.delay.reset();changed|=latest.frames.remove(&stream.id).is_some();changed|=latest.draft_frames.remove(&stream.id).is_some();},
                    Ok(Packet::Audio {rate,channels,timecode,timestamp,samples})=>{
                        if stream.live_generation.load(Ordering::Acquire)==0 {
                            let Some(next)=stream.generation.checked_add(1) else {changed|=problem(&mut latest,stream.id,NdiError::Limit("NDI stream identity exhausted"));continue;};
                            stream.generation=next;
                        }
                        let previous_generation=stream.generation;
                        let format=NativeAudioFormat {sample_rate:rate,channels,channel_mask:0};
                        if stream.format!=Some(format){stream.generation=stream.generation.saturating_add(1);stream.format=Some(format);latest.formats.insert(stream.id,format);changed=true;}
                        stream.audio_sequence=stream.audio_sequence.saturating_add(1);let (first_media_ns,estimated)=stream.time(timestamp,timecode,received);
                        stream.live_generation.store(stream.generation,Ordering::Release);
                        if stream.generation!=previous_generation {stream.delay.reset();changed|=latest.frames.remove(&stream.id).is_some();}
                        // Keep the unmodified sender PTS in the established
                        // A/V clock domain, independently of the arrival-anchored
                        // local estimate. Track audio discontinuity separately:
                        // this counter must not be compared with video epochs.
                        let original=match stream.clock {
                            Some((true,..))=>timestamp.map(|v|(NdiVideoClock::SdkTimestamp,i128::from(v)*100)),
                            Some((false,..))=>(timecode!=i64::MAX).then_some((NdiVideoClock::SenderTimecode,i128::from(timecode)*100)),
                            None=>None,
                        };
                        if let Some((clock,pts))=original {
                            let discontinuity=stream.generation!=previous_generation || stream.audio_clock_missing || stream.last_audio_clock.is_some_and(|(old_clock,old_pts)|
                                old_clock!=clock || pts.checked_sub(old_pts).is_none_or(|delta| !(0..=1_000_000_000).contains(&delta)));
                            if discontinuity {
                                let Some(epoch)=stream.audio_clock_epoch.checked_add(1) else {changed|=problem(&mut latest,stream.id,NdiError::Limit("NDI audio clock identity exhausted"));continue;};
                                stream.audio_clock_epoch=epoch;
                                // A changed sender audio epoch cannot be paired
                                // with video/PCM from the old stream. Rotate
                                // the shared stream identity when the format or
                                // clock mapping did not already do so.
                                if stream.generation==previous_generation {
                                    let Some(next)=stream.generation.checked_add(1) else {changed|=problem(&mut latest,stream.id,NdiError::Limit("NDI stream identity exhausted"));continue;};
                                    stream.generation=next;stream.live_generation.store(next,Ordering::Release);
                                    stream.delay.reset();
                                    changed|=latest.frames.remove(&stream.id).is_some();
                                }
                            }
                            stream.last_audio_clock=Some((clock,pts));stream.audio_clock_missing=false;
                        } else if stream.last_audio_clock.is_some() {stream.audio_clock_missing=true;}
                        let audio_epoch=if original.is_some(){stream.audio_clock_epoch}else{0};
                        stream.live_audio_clock.store(audio_epoch,Ordering::Release);
                        // The recording path must never correlate arrival time
                        // with a sender timestamp as if they shared a clock.
                        if original.is_none() && stream.ndi.audio() && scope.delays.iter().any(|(id,_)|*id==stream.id) {
                            stream.delay.reset();
                            changed|=latest.frames.remove(&stream.id).is_some();
                            changed|=problem(&mut latest,stream.id,NdiError::Unavailable("NDI delayed audio has no matching sender clock"));
                            continue;
                        }
                        if let Some(program)=wanted.program.filter(|_|wanted.audio_sources.contains(&stream.id)) {
                            if !desired.lock().is_ok_and(|current|current.scope==wanted.scope&&current.program==Some(program)&&current.audio_sources.contains(&stream.id)){continue;}
                            let resident=pcm.iter().map(|p|p.samples.len()*4).sum::<usize>();
                            if pcm.len()>=64||samples.len()*4>PCM.saturating_sub(resident){changed|=problem(&mut latest,stream.id,NdiError::Limit("NDI PCM resident4MiB/64 packet budget"));continue;}
                            let packet=Arc::new(NdiPcmPacket {program,source:stream.id,ndi:stream.ndi.clone(),stream_generation:stream.generation,sequence:stream.audio_sequence,first_media_ns,received_media_ns:received,format,samples,timestamp_estimated:estimated,sender_timecode_100ns:timecode,sender_timestamp_100ns:timestamp,native_timestamp_ns:original.map(|(_,pts)|pts),native_clock:original.map(|(clock,_)|clock),native_clock_epoch:audio_epoch,owner:instance,scope_epoch:scope.epoch,live_scope:delay_epoch.clone(),live_stream:stream.live_generation.clone(),live_audio_clock:stream.live_audio_clock.clone()});
                            pcm.push(packet.clone());if audio.sender.try_send(packet).is_err(){changed|=problem(&mut latest,stream.id,NdiError::Limit("NDI Program audio queue is full or closed"));}
                        }
                    },
                    Err(e)=>{stream.delay.reset();changed|=latest.frames.remove(&stream.id).is_some();changed|=problem(&mut latest,stream.id,e);break;},
                }
            }
        }
        if changed {let Some(next)=generation.checked_add(1)else{break;};generation=next;latest.generation=generation;dirty=true;}
        if dirty {
            // Recheck complete scope after native work; obsolete payloads never publish.
            if desired.lock().is_ok_and(|d|d.scope==scope){match publish.try_send(Arc::new(latest.clone())){Ok(())=>dirty=false,Err(mpsc::TrySendError::Disconnected(_))=>break,Err(mpsc::TrySendError::Full(_))=>{}}}
        }
        thread::sleep(Duration::from_millis(5));
    }
    drop(pending);drop(streams);latest.frames.clear();drop(latest);drop(publish);drop(reply);drop(jobs);
    // Externally pinned images and PCM remain charged and are finally dropped
    // here. Retain SDK lifetime until every payload/receiver is retired.
    loop {pixels.retain(|p|Arc::strong_count(p)>1);pcm.retain(|p|Arc::strong_count(p)>1);if pixels.is_empty()&&pcm.is_empty(){break;}thread::sleep(Duration::from_millis(50));}
    drop(api);
}
fn problem(snapshot:&mut NdiSnapshot,source:Id,error:NdiError)->bool{
    let code=error.code();let detail=error.to_string();
    if snapshot.problems.iter().any(|old|old.source==source&&old.code==code&&old.detail==detail){return false;}
    snapshot.problems.retain(|p|p.source!=source);
    // Inputs are bounded strings; never truncate device/runtime diagnostics.
    snapshot.problems.push(NdiProblem {source,code,detail});true
}

//! Native packet ports own codecs. This coordinator owns render/cache, PCM
//! conversion, encoded ring and pinned save lifetime; UI never handles pixels.
use super::*;
use super::ring::{Ring,Unit};
use skylive_media::{AudioConverter,AudioConvertLimits,AudioFormat};
use std::time::Duration;
fn detail(error:impl std::fmt::Display)->ReplayError{ReplayError::Detail(error.to_string())}
struct Closed(Arc<AtomicBool>);
impl Drop for Closed {fn drop(&mut self){self.0.store(true,Ordering::Release);}}
struct PendingSave {path:PathBuf,cancel:Arc<AtomicBool>,cutoff:i64}
struct Saver {handle:thread::JoinHandle<()>,reply:mpsc::Receiver<Result<ReplayReceipt,ReplayError>>}
#[allow(clippy::too_many_arguments)]
pub(super) fn run(key:OutputKey,config:ReplayConfig,gpu:RenderGpu,commands:mpsc::Receiver<Command>,frames:mpsc::Receiver<RecordingFrame>,events:mpsc::SyncSender<ReplayEvent>,status:Arc<Mutex<ReplayStatus>>,stop:Arc<AtomicBool>,closed:Arc<AtomicBool>,mut audio:Option<audio_output::StreamingAudioReceiver>,control:Option<StreamingAudioControl>,resources:Arc<AtomicU64>,permit:Permit){
    let _closed=Closed(closed);let _permit=permit;
    let mut video=None;let mut encoder_audio=None;let mut saver:Option<Saver>=None;let mut pending_save:Option<PendingSave>=None;
    let operation=(||->Result<(),ReplayError>{
        let vc=config.h264(key)?;let ac=config.audio_enabled.then(||config.aac(key)).transpose()?;
        video=Some(native_h264::EncoderWorker::native(vc.clone()).map_err(detail)?);
        if let Some(ac)=&ac {encoder_audio=Some(native_aac::EncoderWorker::native(ac.clone()).map_err(detail)?);}
        let mut ring=Ring::new(config.clone())?;let mut renderer=gpu.output_renderer();
        let mut converter=if let Some(ac)=&ac {Some(AudioConverter::new(AudioFormat::new(config.audio.sample_rate(),config.audio.speakers().channels(),config.audio.speakers().channel_mask()).map_err(detail)?,AudioFormat::new(48_000,ac.channels(),if ac.channels()==1{4}else{3}).map_err(detail)?,AudioConvertLimits{max_bytes:2*1024*1024,..Default::default()}).map_err(detail)?)}else{None};
        let mut scratch=vec![0f32;1024*ac.as_ref().map_or(1,|v|usize::from(v.channels()))];
        let mut video_ready=false;let mut audio_ready=ac.is_none();let mut announced=false;let mut origin=None;let mut last_tick=None;
        let mut audio_first=None;let mut audio_absolute_end=None;let mut converted=0u64;let mut degraded=0usize;
        let mut frames_closed=false;let mut video_eof=false;let mut audio_eof=ac.is_none();
        let mut video_drained=false;let mut audio_drained=ac.is_none();let mut converter_sealed=false;let mut cutoff_requested=false;
        loop {
            let stopping=stop.load(Ordering::Acquire);
            if stopping&&origin.is_none(){
                // No admitted media origin means no Program tail exists. Still
                // retire codecs on their owners; never fabricate frame0/EOS.
                if let Some(c)=&control{c.request_stop();}break;
            }
            let encoder=video.as_mut().unwrap();
            if let Some(event)=encoder.poll(){match event {
                native_h264::Event::Ready {generation,configuration}=>{if generation!=vc.generation(){return Err(ReplayError::Invalid("replay AVC readiness generation"));}video_ready=true;if let Some(header)=configuration{ring.configuration_video(header)?;}},
                native_h264::Event::Packets(batch)=>{video_drained=batch.drained;if batch.generation!=vc.generation(){return Err(ReplayError::Invalid("replay AVC packet generation"));}if let Some(header)=batch.configuration{ring.configuration_video(header)?;}for unit in batch.packets{ring.insert(Unit::Video(unit))?;}},
                native_h264::Event::Failed {error,..}=>return Err(detail(error)),native_h264::Event::Closed {..}=>return Err(ReplayError::Unavailable("replay AVC encoder closed unexpectedly")),
            }}
            if let (Some(encoder),Some(ac))=(&mut encoder_audio,&ac){if let Some(event)=encoder.poll(){match event {
                native_aac::Event::Ready {generation,configuration}=>{if generation!=ac.generation(){return Err(ReplayError::Invalid("replay AAC readiness generation"));}audio_ready=true;ring.configuration_audio(configuration)?;},
                native_aac::Event::Packets(batch)=>{audio_drained=batch.drained;if batch.generation!=ac.generation(){return Err(ReplayError::Invalid("replay AAC packet generation"));}ring.configuration_audio(batch.configuration)?;let offset=audio_first.ok_or(ReplayError::Invalid("AAC arrived before actual Program sample anchor"))?;for unit in batch.packets{unit.pts().checked_add(offset).and_then(|p|p.checked_add(unit.duration())).ok_or(ReplayError::Invalid("replay AAC anchor overflow"))?;ring.insert(Unit::Audio {unit,offset})?;}},
                native_aac::Event::Failed {error,..}=>return Err(detail(error)),native_aac::Event::Closed {..}=>return Err(ReplayError::Unavailable("replay AAC encoder closed unexpectedly")),
            }}}
            if video_ready&&audio_ready&&!announced {events.try_send(ReplayEvent::ReadyForInputs {key}).map_err(|_|ReplayError::Closed)?;announced=true;}
            match commands.try_recv(){
                Ok(Command::Activate(at))=>{if !announced||origin.is_some(){return Err(ReplayError::Invalid("replay activation phase"));}origin=Some(at);events.try_send(ReplayEvent::Started {key}).map_err(|_|ReplayError::Closed)?;},
                Ok(Command::Save {path,cancel})=>{
                    if saver.is_some()||pending_save.is_some(){return Err(ReplayError::Busy);}
                    if let Some(last)=last_tick {let (pts,duration)=vc.frame_timing(last).map_err(detail)?;
                        pending_save=Some(PendingSave {path,cancel,cutoff:pts+duration});
                    }else{if let Ok(mut s)=status.lock(){s.saving=false;}let _=events.try_send(ReplayEvent::Saved {key,result:Err(ReplayError::Unavailable("Replay has no actual video cutoff"))});}
                },Err(mpsc::TryRecvError::Disconnected)=>{if !stopping{stop.store(true,Ordering::Release);}},Err(mpsc::TryRecvError::Empty)=>{}
            }
            if saver.as_ref().is_some_and(|s|s.handle.is_finished()){let save=saver.take().unwrap();let result=save.reply.try_recv().unwrap_or_else(|_|Err(ReplayError::Unavailable("replay save worker ended without receipt")));let _=save.handle.join();if let Ok(mut s)=status.lock(){s.saving=false;}events.try_send(ReplayEvent::Saved {key,result}).map_err(|_|ReplayError::Closed)?;}
            if let Some(origin)=origin {
                if !encoder.is_busy()&&!video_eof {let next=match frames.try_recv(){Ok(frame)=>Some(frame),Err(mpsc::TryRecvError::Disconnected)=>{frames_closed=true;None},Err(mpsc::TryRecvError::Empty)=>None};if let Some(frame)=next{
                    if frame.session!=key.session||!frame.model.output_video().is_ok_and(|video|video==config.video)||!audio_output::same_format(frame.model.audio(),config.audio){return Err(ReplayError::Invalid("replay Program frame format or session changed"));}
                    let elapsed=frame.time.0.checked_sub(origin.0).ok_or(ReplayError::Invalid("replay Program time moved before origin"))?;
                    let tick=u64::try_from(u128::from(elapsed)*u128::from(config.video.fps_num())/(1_000_000_000u128*u128::from(config.video.fps_den()))).map_err(|_|ReplayError::Invalid("replay frame index overflow"))?;
                    if last_tick.is_none_or(|last|tick>last)&&frame.resource_generation==resources.load(Ordering::Acquire){
                        renderer.resource_generation(frame.resource_generation);
                        let rendered=renderer.render_profile_output(&frame.model,&frame.output,&frame.playback,&frame.capture).map_err(detail)?;degraded=rendered.diagnostics.len();
                        let nv12=skylive_media::recording_nv12(&rendered.image,32*1024*1024,18_000_000).map_err(detail)?;drop(rendered);
                        if frame.resource_generation!=resources.load(Ordering::Acquire){continue;}
                        let (pts,duration)=vc.frame_timing(tick).map_err(detail)?;let input=native_h264::Nv12Input::new(&vc,pts,duration,nv12.into_boxed_slice()).map_err(detail)?;
                        encoder.submit(input).map_err(|e|detail(e.error))?;last_tick=Some(tick);
                    }
                }}
                if let (Some(receiver),Some(converter),Some(aenc),Some(ac))=(&mut audio,&mut converter,&mut encoder_audio,&ac){
                    if control.as_ref().is_some_and(|c|c.stats().fault.is_some()){return Err(ReplayError::Unavailable("replay Program audio transport fault"));}
                    // Leave bounded backpressure with the existing PCM producer;
                    // never clone Program blocks or manufacture missing audio.
                    if converter.stats().queued_input_frames<2048 {if let Some(delivery)=receiver.poll(){
                        if delivery.output_key()!=key||delivery.anchor().media_origin_ns()!=origin.0||!audio_output::same_format(delivery.program_key().settings,config.audio)||delivery.gap_before_frames()!=0{return Err(ReplayError::Invalid("replay Program audio scope or gap"));}
                        if audio_absolute_end.is_some_and(|end|delivery.absolute_first_frame()!=end){return Err(ReplayError::Invalid("replay Program audio discontinuity"));}
                        audio_absolute_end=delivery.absolute_first_frame().checked_add(delivery.frames() as u64);if audio_first.is_none(){audio_first=Some(delivery.first_pts_100ns().map_err(detail)?);}
                        converter.push(delivery.samples()).map_err(detail)?;
                    }}
                    if receiver.drained()&&!converter_sealed {converter.finish().map_err(detail)?;converter_sealed=true;}
                    if !aenc.is_busy()&&!audio_eof {let frames=converter.pull(&mut scratch).map_err(detail)?;if frames!=0 {let samples=native_aac::pcm16_interleaved(&scratch[..frames*usize::from(ac.channels())],ac.channels()).map_err(detail)?;let input=native_aac::PcmInput::new(ac,converted,samples).map_err(detail)?;aenc.submit(input).map_err(|e|detail(e.error))?;converted=converted.checked_add(frames as u64).ok_or(ReplayError::Invalid("replay converted sample position"))?;}
                        else if converter_sealed&&converter.stats().drained {aenc.finish().map_err(detail)?;audio_eof=true;}
                    }
                }
            }
            if stopping&&frames_closed&&!encoder.is_busy()&&!video_eof {
                if last_tick.is_none(){if let Some(c)=&control{c.request_stop();}break;}
                encoder.finish().map_err(detail)?;video_eof=true;
            }
            if stopping&&video_eof&&!cutoff_requested {
                if let (Some(origin),Some(last),Some(control))=(origin,last_tick,control.as_ref()){
                    let end=(u128::from(last+1)*1_000_000_000*u128::from(config.video.fps_den())).div_ceil(u128::from(config.video.fps_num()));
                    let end=u64::try_from(end).ok().and_then(|end|origin.0.checked_add(end)).ok_or(ReplayError::Invalid("replay final Program cutoff overflow"))?;
                    control.request_stop_at(MediaTime(end)).map_err(detail)?;
                }cutoff_requested=true;
            }
            if let Some(pending)=pending_save.as_ref(){
                if pending.cancel.load(Ordering::Acquire){let pending=pending_save.take().unwrap();drop(pending);if let Ok(mut s)=status.lock(){s.saving=false;}let _=events.try_send(ReplayEvent::Saved {key,result:Err(ReplayError::Cancelled)});}
                else {match ring.clip_until(key,Some(pending.cutoff)){
                    Ok(clip)=>{let pending=pending_save.take().unwrap();let (send,receive)=mpsc::sync_channel(1);match thread::Builder::new().name("skylive-replay-save".into()).spawn(move||{let result=save::write(clip,pending.path,pending.cancel);let _=send.send(result);}){Ok(handle)=>saver=Some(Saver {handle,reply:receive}),Err(e)=>{if let Ok(mut s)=status.lock(){s.saving=false;}let _=events.try_send(ReplayEvent::Saved {key,result:Err(detail(e))});}}},
                    Err(ReplayError::Busy)|Err(ReplayError::Unavailable(_)) if !(stopping&&video_drained&&audio_drained)=>{},
                    Err(error)=>{pending_save.take();if let Ok(mut s)=status.lock(){s.saving=false;}let _=events.try_send(ReplayEvent::Saved {key,result:Err(error)});}
                }}
            }
            if stopping&&video_drained&&audio_drained&&pending_save.is_none(){break;}
            if stopping&&origin.is_none()&&frames_closed{if let Some(c)=&control{c.request_stop();}break;}
            ring.reap();if let Ok(mut s)=status.lock(){s.starting=origin.is_none();s.active=origin.is_some();s.degraded_sources=degraded;ring.status(&mut s);if stop.load(Ordering::Acquire){s.ready=false;s.active=false;}}
            thread::sleep(Duration::from_millis(1));
        }
        if let Some(pending)=pending_save.take(){pending.cancel.store(true,Ordering::Release);let _=events.try_send(ReplayEvent::Saved {key,result:Err(ReplayError::Unavailable("Replay stopped before a complete requested interval was available"))});}
        Ok(())
    })();
    if operation.is_err(){if let Some(pending)=pending_save.take(){pending.cancel.store(true,Ordering::Release);let _=events.try_send(ReplayEvent::Saved {key,result:Err(ReplayError::Unavailable("Replay failed before delayed Program audio reached the save cutoff"))});}}
    if operation.is_err(){if let Ok(Command::Save {cancel,..})=commands.try_recv(){cancel.store(true,Ordering::Release);let _=events.try_send(ReplayEvent::Saved {key,result:Err(ReplayError::Unavailable("Replay buffer failed before the save snapshot was prepared"))});}}
    if let Err(error)=operation {if let Ok(mut s)=status.lock(){s.problem=Some(error.to_string());}let _=events.try_send(ReplayEvent::Failed {key,error});}
    stop.store(true,Ordering::Release);if let Some(c)=&control{c.request_stop();}drop(audio);drop(frames);
    if let Some(video)=&mut video {video.request_close();}if let Some(audio)=&mut encoder_audio{audio.request_close();}
    if let Ok(mut s)=status.lock(){s.active=false;s.starting=false;s.stopping=true;s.ready=false;}
    // Retain the replay permit until native codecs and any independent save
    // really settle. No hard cancellation or GUI join promise is made.
    loop {if let Some(v)=&mut video {let _=v.poll();}if let Some(a)=&mut encoder_audio{let _=a.poll();}
        if video.as_ref().is_none_or(|v|v.is_closed())&&encoder_audio.as_ref().is_none_or(|a|a.is_closed()){break;}thread::sleep(Duration::from_millis(5));}
    if let Some(save)=saver {let _=save.handle.join();let result=save.reply.try_recv().unwrap_or_else(|_|Err(ReplayError::Unavailable("replay save worker ended without receipt")));let _=events.try_send(ReplayEvent::Saved {key,result});}
    if let Ok(mut s)=status.lock(){s.stopping=false;s.saving=false;s.ready=false;s.stored_bytes=0;s.duration_100ns=0;}
    let _=events.try_send(ReplayEvent::Stopped {key});
}

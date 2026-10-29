//! One actual SDK sender per owned worker. Program PCM is already post-DSP.
use super::*;
use std::time::{Duration,Instant};
struct Closed(Arc<AtomicBool>);impl Drop for Closed{fn drop(&mut self){self.0.store(true,Ordering::Release);}}
#[allow(clippy::too_many_arguments)]
pub(super) fn run(key:Key,config:Config,gpu:RenderGpu,commands:mpsc::Receiver<Command>,frames:mpsc::Receiver<RecordingFrame>,mut audio:Option<audio_output::StreamingAudioReceiver>,control:Option<StreamingAudioControl>,events:mpsc::SyncSender<Event>,status:Arc<Mutex<Status>>,stop:Arc<AtomicBool>,closed:Arc<AtomicBool>,revision:Arc<AtomicU64>,resources:Arc<AtomicU64>,lease:Lease){
    let _closed=Closed(closed);let _lease=lease;
    let result=(||->Result<(),NdiError>{
        #[cfg(not(all(target_os="windows",target_arch="x86_64")))] {let _=(&config,&gpu,&commands,&frames,&mut audio,&control,&revision,&resources);return Err(NdiError::Unavailable("NDI output requires Windows x64"));}
        #[cfg(all(target_os="windows",target_arch="x86_64"))] {
            let mut sender=native_windows::Sender::new(&config,&stop)?;
            if stop.load(Ordering::Acquire){drop(sender);return Ok(());}
            if let Ok(mut s)=status.lock(){s.backend_ready=true;s.starting=false;s.advertised_name=Some(sender.name.clone());s.advertised_url=sender.url.clone();s.runtime_version=Some(sender.version().into());}
            events.try_send(Event::Ready {key}).map_err(|_|NdiError::Closed)?;
            let mut renderer=gpu.output_renderer();let mut origin=None;let mut last_tick=None;let mut last_audio_end=None;let mut first=true;let mut scratch=Vec::with_capacity(4096*8);let mut connection_check:Option<Instant>=None;
            while !stop.load(Ordering::Acquire){
                if let Ok(Command::Activate(at))=commands.try_recv(){if origin.is_some(){return Err(NdiError::Invalid("NDI sender activated twice"));}origin=Some(at);}
                if let Some(origin)=origin {
                    if let Ok(frame)=frames.try_recv(){
                        if !config.matches(frame.session,&frame.model){return Err(NdiError::Stale);}
                        if frame.model.revision()!=revision.load(Ordering::Acquire)||frame.resource_generation!=resources.load(Ordering::Acquire){continue;}
                        let elapsed=frame.time.0.checked_sub(origin.0).ok_or(NdiError::Invalid("NDI output video clock moved before run origin"))?;
                        let tick=u64::try_from(u128::from(elapsed)*u128::from(config.video.fps_num())/(1_000_000_000u128*u128::from(config.video.fps_den()))).map_err(|_|NdiError::Limit("NDI video tick"))?;
                        if last_tick.is_none_or(|old|tick>old){
                            renderer.resource_generation(frame.resource_generation);
                            let rendered=match config.route {NdiOutputRoute::Program=>renderer.render_profile_output(&frame.model,&frame.output,&frame.playback,&frame.capture),NdiOutputRoute::Preview=>renderer.render_live(&frame.model,&frame.output,config.video.width(),config.video.height(),&frame.playback,&frame.capture)}.map_err(detail)?;
                            if frame.model.revision()!=revision.load(Ordering::Acquire)||frame.resource_generation!=resources.load(Ordering::Acquire)||stop.load(Ordering::Acquire){continue;}
                            let degraded=rendered.diagnostics.len();let pts=i64::try_from(elapsed/100).map_err(|_|NdiError::Limit("NDI video timecode"))?;
                            sender.video(rendered.image,pts,&config)?;
                            if let Ok(mut s)=status.lock(){s.video_frames=s.video_frames.saturating_add(1);s.skipped_ticks=s.skipped_ticks.saturating_add(last_tick.map_or(0,|old|tick.saturating_sub(old+1)));s.degraded_sources=degraded;s.sending=true;}
                            last_tick=Some(tick);if first{events.try_send(Event::Sending {key}).map_err(|_|NdiError::Closed)?;first=false;}
                        }
                    }
                    if let (Some(receiver),Some(format))=(&mut audio,config.audio){
                        if control.as_ref().is_some_and(|c|c.stats().fault.is_some()){return Err(NdiError::Runtime("NDI Program audio transport fault".into()));}
                        // Drain a bounded batch without starving the video/Stop loop.
                        for _ in 0..4{let Some(delivery)=receiver.poll()else{break;};
                            if delivery.output_key()!=key.audio()||delivery.anchor().media_origin_ns()!=origin.0||!audio_output::same_format(delivery.program_key().settings,format)||delivery.gap_before_frames()!=0||last_audio_end.is_some_and(|end|delivery.absolute_first_frame()!=end){return Err(NdiError::Invalid("NDI Program PCM scope, order or gap"));}
                            let pts=delivery.first_pts_100ns().map_err(detail)?;sender.audio(delivery.samples(),delivery.frames(),pts,format,&mut scratch)?;last_audio_end=delivery.absolute_first_frame().checked_add(delivery.frames() as u64);if let Ok(mut s)=status.lock(){s.audio_frames=s.audio_frames.saturating_add(delivery.frames() as u64);}
                        }
                    }
                }
                if connection_check.is_none_or(|at|at.elapsed()>=Duration::from_secs(1)){let connections=sender.connections()?;if let Ok(mut s)=status.lock(){s.connections=connections;}connection_check=Some(Instant::now());}
                thread::sleep(Duration::from_millis(2));
            }
            // sender Drop flushes its exact retained async frame and destroys the
            // network namespace before the native admission lease is released.
            drop(sender);Ok(())
        }
    })();
    let requested=stop.load(Ordering::Acquire);
    stop.store(true,Ordering::Release);if let Some(c)=&control{c.request_stop();}drop(audio);drop(frames);
    let result=if requested&&matches!(&result,Err(NdiError::Closed)){Ok(())}else{result};
    if let Err(error)=result{if let Ok(mut s)=status.lock(){s.problem=Some(error.to_string());}let _=events.try_send(Event::Failed {key,error});}
    if let Ok(mut s)=status.lock(){s.admitted=false;s.starting=false;s.backend_ready=false;s.sending=false;s.stopping=false;s.connections=0;s.advertised_name=None;s.advertised_url=None;}
    let _=events.try_send(Event::Stopped {key});
}

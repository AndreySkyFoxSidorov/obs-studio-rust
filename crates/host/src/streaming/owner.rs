#[path="reconnect.rs"]
mod reconnect;
use super::{
    audio::AudioPump,
    backend::{
        AudioPort, CodecEvent, Factory, RenderFactory, PublishEvent, PublisherPort, VideoPort,
    },
    timing::{Outbox, Track},
    *,
};
use crate::audio_output::{StreamingAudioControl, StreamingAudioReceiver};
use std::{
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
    },
    thread,
    time::Duration,
};
static NEXT_OUTPUT: AtomicU64 = AtomicU64::new(1);
static SERVICES: OnceLock<Arc<AtomicUsize>> = OnceLock::new();
struct ServicePermit(Arc<AtomicUsize>);
impl ServicePermit {
    fn acquire(pool: Arc<AtomicUsize>) -> Result<Self, Error> {
        pool.fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
            (n < 2).then_some(n + 1)
        })
        .map_err(|_| Error::Limit("two streaming services active/retiring"))?;
        Ok(Self(pool))
    }
}
impl Drop for ServicePermit {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}
struct VideoPermit(Arc<AtomicUsize>);
impl VideoPermit {
    fn acquire(pool: &Arc<AtomicUsize>) -> Result<Self, Error> {
        pool.fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
            (n < 2).then_some(n + 1)
        })
        .map_err(|_| Error::Limit("two Program video inputs in flight"))?;
        Ok(Self(pool.clone()))
    }
}
impl Drop for VideoPermit {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}
struct ClosedSignal(Arc<AtomicBool>);
impl Drop for ClosedSignal {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}
struct Controls {
    audio_enabled: bool,
    video_end: AtomicBool,
    audio_end: AtomicBool,
    stop: Arc<AtomicBool>,
    connection:AtomicU64,
    cancel: AtomicBool,
}
fn seal_admission(controls: &Controls, slots: &AtomicUsize) {
    slots.fetch_or(4, Ordering::AcqRel);
    controls.video_end.store(true, Ordering::Release);
    controls.audio_end.store(true, Ordering::Release);
    controls.stop.store(true, Ordering::Release);
}
struct AdmissionSeal<'a> {
    controls: &'a Controls,
    slots: &'a AtomicUsize,
}
impl Drop for AdmissionSeal<'_> {
    fn drop(&mut self) {
        seal_admission(self.controls, self.slots);
    }
}
enum Work {
    Activate(ProgramVideoFrame, VideoPermit,u64),
    Video(ProgramVideoFrame, VideoPermit,u64),
}
/// Single operation facade. No SDK, file IO, DNS, socket calls or joins here.
pub struct StreamingOwner {
    key: StreamKey,
    epoch: u64,
    commands: SyncSender<Work>,
    events: Receiver<Event>,
    controls: Arc<Controls>,
    closed: Arc<AtomicBool>,
    status: Arc<Mutex<Status>>,
    video_slots: Arc<AtomicUsize>,
    ready: bool,
    activated: bool,
    _thread: thread::JoinHandle<()>,
}
impl StreamingOwner {
    pub fn start(config: Config) -> Result<StartedOwner, Error> {
        Self::start_with_render_gpu(config,crate::render_gpu::RenderGpu::cpu())
    }
    pub fn start_with_render_gpu(config:Config,render_gpu:crate::render_gpu::RenderGpu)->Result<StartedOwner,Error> {
        if !cfg!(target_os = "windows") {
            return Err(Error::Invalid(
                "Windows packet encoders required; other platforms unavailable",
            ));
        }
        Self::spawn(
            config,
            Box::new(RenderFactory {render_gpu}),
            SERVICES
                .get_or_init(|| Arc::new(AtomicUsize::new(0)))
                .clone(),
            None,
        )
    }
    #[cfg(test)]
    pub(crate) fn with_factory(
        config: Config,
        factory: Box<dyn Factory>,
        admission: Arc<AtomicUsize>,
    ) -> Result<StartedOwner, Error> {
        Self::spawn(
            config,
            factory,
            admission,
            Some(Arc::new(AtomicUsize::new(0))),
        )
    }
    fn spawn(
        config: Config,
        factory: Box<dyn Factory>,
        admission: Arc<AtomicUsize>,
        audio_pool: Option<Arc<AtomicUsize>>,
    ) -> Result<StartedOwner, Error> {
        #[cfg(not(test))]
        let _ = audio_pool;
        let permit = ServicePermit::acquire(admission)?;
        let output = NEXT_OUTPUT
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |id| id.checked_add(1))
            .map_err(|_| Error::Limit("streaming identity exhausted"))?;
        let key = StreamKey {
            session: config.session,
            output,
        };
        let epoch = key.output;
        let (audio_sink, audio) = if let Some(settings) = config.audio {
            #[cfg(test)]
            let channel = match audio_pool {
                Some(pool) => {
                    crate::audio_output::channel_for_test_track(key.audio(), settings, None, pool,config.settings.audio_track())
                }
                None => crate::audio_output::streaming_channel_track(key.audio(), settings, None,config.settings.audio_track()),
            };
            #[cfg(not(test))]
            let channel = crate::audio_output::streaming_channel_track(key.audio(), settings, None,config.settings.audio_track());
            let (sink, receiver, control) = channel.map_err(|e| Error::Audio(e.to_string()))?;
            (Some(sink), Some((receiver, control)))
        } else {
            (None, None)
        };
        let (commands, work) = mpsc::sync_channel(2);
        let (reply, events) = mpsc::sync_channel(2);
        let controls = Arc::new(Controls {
            audio_enabled: audio_sink.is_some(),
            video_end: AtomicBool::new(false),
            audio_end: AtomicBool::new(false),
            stop: Arc::new(AtomicBool::new(false)),
            connection:AtomicU64::new(epoch),
            cancel: AtomicBool::new(false),
        });
        let closed = Arc::new(AtomicBool::new(false));
        let status = Arc::new(Mutex::new(Status::new(key)));
        let video_slots = Arc::new(AtomicUsize::new(0));
        let control = controls.clone();
        let done = closed.clone();
        let state = status.clone();
        let slots = video_slots.clone();
        let handle = thread::Builder::new()
            .name("skylive-streaming-owner".into())
            .spawn(move || {
                let _closed = ClosedSignal(done);
                let _permit = permit;
                let _render_services =
                    crate::preview::retain_render_services().inspect_err(|error| {
                        eprintln!("streaming render service {}: {}", error.code(), error);
                    });
                // Guard drops before the permit and ClosedSignal: completed
                // retirement is not published before pool destruction ends.
                run(Worker {
                    config,
                    key,
                    epoch,
                    factory,
                    audio,
                    work,
                    reply,
                    controls: control,
                    state,
                    video_slots: slots,
                });
            })
            .map_err(|e| Error::Worker(e.to_string()))?;
        Ok(StartedOwner {
            owner: Self {
                key,
                epoch,
                commands,
                events,
                controls,
                closed,
                status,
                video_slots,
                ready: false,
                activated: false,
                _thread: handle,
            },
            audio_sink,
        })
    }
    pub fn key(&self) -> StreamKey {
        self.key
    }
    pub fn epoch(&self) -> u64 {
        self.controls.connection.load(Ordering::Acquire)
    }
    pub fn status(&self) -> Option<Status> {
        self.status.try_lock().ok().map(|status| status.clone())
    }
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }
    pub fn poll(&mut self)->Option<Event>{
        // Readiness is latest-per-connection state, not an accumulating event
        // per retry. Two lifecycle slots remain reserved for Started/terminal.
        for _ in 0..2{if let Ok(event)=self.events.try_recv(){match &event{Event::Started{epoch,..} if *epoch!=self.epoch()=>continue,_=>return Some(event)}}break;}
        let status=self.status()?;let epoch=self.epoch();if status.connection_epoch==epoch&&status.ready_for_inputs&&(!self.ready||self.epoch!=epoch)&&(!self.activated||self.epoch!=epoch)&&!self.controls.stop.load(Ordering::Acquire){self.epoch=epoch;self.ready=true;self.activated=false;return Some(Event::ReadyForInputs{key:self.key,epoch});}None
    }
    pub fn activate(
        &mut self,
        key: StreamKey,
        epoch: u64,
        frame: ProgramVideoFrame,
    ) -> Result<(), RejectedFrame> {
        if key != self.key {
            return Err(RejectedFrame {
                error: Error::Scope,
                frame: Box::new(frame),
            });
        }
        if epoch!=self.epoch()||self.epoch!=epoch{return Err(RejectedFrame{error:Error::Reconnecting,frame:Box::new(frame)});}
        if !self.ready || self.activated {
            return Err(RejectedFrame {
                error: Error::State("fresh input activation requires consumed ReadyForInputs"),
                frame: Box::new(frame),
            });
        }
        self.submit(frame, true)?;
        self.ready = false;
        self.activated = true;
        Ok(())
    }
    pub fn video(
        &mut self,
        key: StreamKey,
        epoch: u64,
        frame: ProgramVideoFrame,
    ) -> Result<(), RejectedFrame> {
        if key != self.key {
            return Err(RejectedFrame {
                error: Error::Scope,
                frame: Box::new(frame),
            });
        }
        if epoch!=self.epoch()||self.epoch!=epoch{return Err(RejectedFrame{error:Error::Reconnecting,frame:Box::new(frame)});}
        if !self.activated {
            return Err(RejectedFrame {
                error: Error::State("stream not activated"),
                frame: Box::new(frame),
            });
        }
        self.submit(frame, false)
    }
    fn submit(&mut self, frame: ProgramVideoFrame, activate: bool) -> Result<(), RejectedFrame> {
        if self.epoch!=self.epoch(){return Err(RejectedFrame{error:Error::Reconnecting,frame:Box::new(frame)});}
        if frame.session != self.key.session {
            return Err(RejectedFrame {
                error: Error::Scope,
                frame: Box::new(frame),
            });
        }
        if self.controls.video_end.load(Ordering::Acquire)
            || self.controls.stop.load(Ordering::Acquire)
            || self.is_closed()
        {
            return Err(RejectedFrame {
                error: Error::State("stream stopping/closed"),
                frame: Box::new(frame),
            });
        }
        let permit = match VideoPermit::acquire(&self.video_slots) {
            Ok(p) => p,
            Err(error) => {
                return Err(RejectedFrame {
                    error,
                    frame: Box::new(frame),
                });
            }
        };
        let work = if activate {
            Work::Activate(frame, permit,self.epoch)
        } else {
            Work::Video(frame, permit,self.epoch)
        };
        self.commands.try_send(work).map_err(|e| {
            let (error, work) = match e {
                TrySendError::Full(w) => (Error::Limit("video ingress"), w),
                TrySendError::Disconnected(w) => (Error::Disconnected, w),
            };
            let frame = match work {
                Work::Activate(f, _,_) | Work::Video(f, _,_) => f,
            };
            RejectedFrame {
                error,
                frame: Box::new(frame),
            }
        })
    }
    /// Seal this finite video producer. Already-admitted frames still encode;
    /// only the real native drained event ends the watermark peer.
    pub fn finish_video(&mut self, key: StreamKey, epoch: u64) -> Result<(), Error> {
        self.finish_scope(key, epoch)?;
        if self.controls.video_end.load(Ordering::Acquire) {
            return Ok(());
        }
        self.video_slots.fetch_or(4, Ordering::AcqRel);
        self.controls.video_end.store(true, Ordering::Release);
        Ok(())
    }
    /// Seal only this streaming PCM endpoint. The audio actor's unique producer
    /// must acknowledge its existing end barrier; Recording remains independent.
    pub fn finish_audio(&mut self, key: StreamKey, epoch: u64) -> Result<(), Error> {
        self.finish_scope(key, epoch)?;
        if !self.controls.audio_enabled {
            return Err(Error::State("stream audio is disabled"));
        }
        self.controls.audio_end.store(true, Ordering::Release);
        Ok(())
    }
    fn finish_scope(&self, key: StreamKey, epoch: u64) -> Result<(), Error> {
        if key != self.key || epoch != self.epoch() {
            return Err(Error::Scope);
        }
        if self.epoch!=self.epoch(){return Err(Error::Reconnecting);}
        if !self.activated {
            return Err(Error::State("track EOF requires admitted activation"));
        }
        if self.is_closed() {
            return Err(Error::State("stream closed"));
        }
        Ok(())
    }
    pub fn stop(&self) {
        self.video_slots.fetch_or(4, Ordering::AcqRel);
        self.controls.video_end.store(true, Ordering::Release);
        self.controls.audio_end.store(true, Ordering::Release);
        self.controls.stop.store(true, Ordering::Release);
    }
    pub fn cancel(&self) {
        self.controls.cancel.store(true, Ordering::Release);
        self.stop();
    }
}
impl Drop for StreamingOwner {
    fn drop(&mut self) {
        self.cancel();
    }
}
struct Ports {
    video: Option<Box<dyn VideoPort>>,
    audio: Option<Box<dyn AudioPort>>,
    publisher: Option<Box<dyn PublisherPort>>,
}
impl Ports {
    fn new() -> Self {
        Self {
            video: None,
            audio: None,
            publisher: None,
        }
    }
    fn retire(
        &mut self,
        work: &Receiver<Work>,
        pump: &mut Option<AudioPump>,
        audio_control: Option<&StreamingAudioControl>,
    ) -> Result<(), Error> {
        if let Some(p) = &mut self.publisher {
            p.cancel();
        }
        if let Some(v) = &mut self.video {
            v.close();
        }
        if let Some(a) = &mut self.audio {
            a.close();
        }
        if let Some(p) = pump.as_ref() {
            p.stop();
        }
        loop {
            // Drop queued snapshots/PCM only on the owning worker.
            while work.try_recv().is_ok() {}
            let audio_done = pump.as_mut().is_none_or(AudioPump::retire_discard)
                && audio_control.is_none_or(|c| {
                    let s = c.stats();
                    s.producer_ended && s.queued_blocks == 0
                });
            let video_done = self.video.as_mut().is_none_or(|v| {
                let _ = v.poll();
                v.closed()
            });
            let aac_done = self.audio.as_mut().is_none_or(|a| {
                let _ = a.poll();
                a.closed()
            });
            let network_done = self.publisher.as_mut().is_none_or(|p| {
                let _ = p.poll();
                p.finished()
            });
            if audio_done && video_done && aac_done && network_done {
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        let result = match self.publisher.take() {
            Some(p) => p.join(),
            None => Ok(()),
        };
        // Destructors are inside the service lease, even a blocking destructor.
        self.video = None;
        self.audio = None;
        result
    }
}
fn publish(state: &Mutex<Status>, status: &Status) {
    if let Ok(mut s) = state.lock() {
        *s = status.clone();
    }
}
fn event(reply: &SyncSender<Event>, value: Event) -> Result<(), Error> {
    reply.try_send(value).map_err(|e| match e {
        TrySendError::Full(_) => Error::State("two-event lifecycle capacity invariant"),
        TrySendError::Disconnected(_) => Error::Disconnected,
    })
}
struct Worker {
    config: Config,
    key: StreamKey,
    epoch: u64,
    factory: Box<dyn Factory>,
    audio: Option<(StreamingAudioReceiver, StreamingAudioControl)>,
    work: Receiver<Work>,
    reply: SyncSender<Event>,
    controls: Arc<Controls>,
    state: Arc<Mutex<Status>>,
    video_slots: Arc<AtomicUsize>,
}
fn run(worker: Worker) {
    let Worker {
        mut config,
        key,
        mut epoch,
        mut factory,
        audio,
        work,
        reply,
        controls,
        state,
        video_slots,
    } = worker;
    config.stop=Some(controls.stop.clone());
    let mut ports = Ports::new();
    let mut status = Status::new(key);
    let mut pump = None;
    // Keep the receiver reachable through every partial-initialization failure.
    let audio_control = audio.as_ref().map(|(_, c)| c.clone());
    let mut pending_audio = audio;
    let result = (|| {
        if let Some((receiver, control)) = pending_audio.take() {
            pump = Some(AudioPump::new(
                key,
                config.audio.ok_or(Error::State("missing project audio"))?,
                receiver,
                control,
            )?);
        }
        let mut renderer=factory.renderer();
        let _seal=AdmissionSeal{controls:&controls,slots:&video_slots};
        let mut backoff=reconnect::Backoff::new(key);
        loop{
            if controls.cancel.load(Ordering::Acquire){return Err(Error::Cancelled);}if controls.stop.load(Ordering::Acquire){return Ok(());}
            status.connection_epoch=epoch;status.origin=None;status.audio_offset_100ns=None;status.ready_for_inputs=false;status.connection_started=false;status.reconnect_wait_ms=0;status.native_aac=Default::default();status.interleave=Default::default();status.ndi_video_receipts.clear();status.first_ndi_video_receipts.clear();status.camera_video_receipts.clear();status.first_camera_video_receipts.clear();
            let codec_key=StreamKey{session:key.session,output:epoch};
            ports.video=Some(factory.video(&config,codec_key)?);if config.audio.is_some(){ports.audio=Some(factory.audio(&config,codec_key)?);}
            let result=body(Body{config:&config,key,epoch,ports:&mut ports,pump:&mut pump,renderer:&mut *renderer,factory:&mut *factory,work:&work,reply:&reply,controls:&controls,state:&state,status:&mut status,video_slots:&video_slots});
            let error=match result{Ok(())=>return Ok(()),Err(Error::Cancelled) if controls.stop.load(Ordering::Acquire)&&!controls.cancel.load(Ordering::Acquire)=>return Ok(()),Err(error)=>error};
            if controls.stop.load(Ordering::Acquire)||controls.cancel.load(Ordering::Acquire)||controls.video_end.load(Ordering::Acquire)||controls.audio_end.load(Ordering::Acquire)||!matches!(error,Error::RetryableTransport(_))||!status.ever_published{return Err(error);}
            if status.connection_started{backoff.reset();}
            let Some(delay)=backoff.next(config.settings.reconnect())else{return Err(error);};
            if let Some(p)=pump.as_ref(){p.begin_reconnect();}
            status.reconnect_attempt=backoff.attempts;add(&mut status.reconnect_total,1)?;status.reconnect_last_error=Some(Arc::new(error));status.ready_for_inputs=false;status.phase=Phase::ReconnectRetiring;
            epoch=NEXT_OUTPUT.fetch_update(Ordering::AcqRel,Ordering::Acquire,|n|n.checked_add(1)).map_err(|_|Error::Limit("stream connection epoch exhausted"))?;
            controls.connection.store(epoch,Ordering::Release);status.connection_epoch=epoch;status.origin=None;status.audio_offset_100ns=None;status.connection_started=false;status.native_aac=Default::default();status.interleave=Default::default();status.ndi_video_receipts.clear();status.first_ndi_video_receipts.clear();status.camera_video_receipts.clear();status.first_camera_video_receipts.clear();publish(&state,&status);
            reconnect::retire(&mut ports,&work,&mut pump,&mut status,&state,&controls)?;
            status.phase=Phase::ReconnectWaiting;publish(&state,&status);
            if !reconnect::wait(delay,&work,&mut pump,&mut status,&state,&controls)?{return if controls.cancel.load(Ordering::Acquire){Err(Error::Cancelled)}else{Ok(())};}
            status.phase=Phase::Initializing;publish(&state,&status);
        }
    })();
    video_slots.fetch_or(4, Ordering::AcqRel);
    controls.video_end.store(true, Ordering::Release);
    controls.audio_end.store(true, Ordering::Release);
    controls.stop.store(true, Ordering::Release);
    if let Some(c) = &audio_control {
        c.request_stop();
    }
    if let Some(p) = pump.as_ref() {
        p.stop();
    }
    status.ready_for_inputs=false;
    status.phase = Phase::Retiring;
    if let Err(error) = result {
        status.failure = Some(Arc::new(error));
    }
    publish(&state, &status);
    if let Err(error) = ports.retire(&work, &mut pump, audio_control.as_ref()) {
        if status.failure.is_none() {
            status.failure = Some(Arc::new(error));
        }
    }
    if let Some(p)=pump.as_mut(){if let Err(error)=p.account_offline(&mut status){if status.failure.is_none(){status.failure=Some(Arc::new(error));}}}
    status.phase = Phase::Closed;
    publish(&state, &status);
    let terminal = if status.failure.is_some() {
        Event::Failed(status)
    } else {
        Event::Stopped(status)
    };
    let _ = event(&reply, terminal);
}
struct Body<'a> {
    config: &'a Config,
    key: StreamKey,
    epoch: u64,
    ports: &'a mut Ports,
    pump: &'a mut Option<AudioPump>,
    renderer: &'a mut dyn super::video::Renderer,
    factory: &'a mut dyn Factory,
    work: &'a Receiver<Work>,
    reply: &'a SyncSender<Event>,
    controls: &'a Controls,
    state: &'a Mutex<Status>,
    status: &'a mut Status,
    video_slots: &'a AtomicUsize,
}
fn body(body: Body<'_>) -> Result<(), Error> {
    let Body {
        config,
        key,
        epoch,
        ports,
        pump,
        renderer,
        factory,
        work,
        reply,
        controls,
        state,
        status,
        video_slots,
    } = body;
    let mut video_header = None;
    let mut audio_header = None;
    let codec_key=StreamKey{session:key.session,output:epoch};
    let published_base=status.packets_published;
    let mut outbox = Outbox::new(codec_key, config.audio.is_some());
    let mut publishing = false;
    let mut ready = false;
    let mut started = false;
    let mut origin: Option<MediaTime> = None;
    let mut last_video = None;
    let ndi_lifetime=super::ndi_receipt::Lifetime::new();
    // Metadata only, bounded independently of codec and outbox pixel budgets.
    // A candidate is not a receipt until the matching AVC access unit returns.
    let mut ndi_candidates:std::collections::VecDeque<(i64,Vec<super::ndi_receipt::NdiVideoReceipt>)>=std::collections::VecDeque::new();
    let mut camera_candidates:std::collections::VecDeque<(i64,Vec<super::camera_receipt::CameraVideoReceipt>)>=std::collections::VecDeque::new();
    let mut pending: Option<(ProgramVideoFrame, VideoPermit, bool)> = None;
    let mut video_finish = false;
    let mut audio_finish = false;
    // One existing AudioPump output allocation, prepared only while AAC is idle.
    // <=1024frames*2channels*2bytes; no clone/FIFO. Drops on this service owner.
    let mut pending_audio: Option<(u64, Box<[i16]>)> = None;
    let mut audio_cut_requested=false;
    let mut audio_offset_fixed=config.audio.is_none();
    let mut video_drained = false;
    let mut audio_drained = config.audio.is_none();
    let mut publisher_stop = false;
    loop {
        if controls.cancel.load(Ordering::Acquire) {
            return Err(Error::Cancelled);
        }
        // Observe producer control faults even while one PCM chunk awaits mux
        // room; holding data cannot hide the endpoint's real terminal fault.
        if let Some(fault) = pump.as_ref().and_then(|p| p.control.stats().fault) {
            return Err(Error::Audio(format!("Program streaming sink: {fault:?}")));
        }
        let stopping = controls.stop.load(Ordering::Acquire);
        let video_ending = controls.video_end.load(Ordering::Acquire);
        let audio_ending = controls.audio_end.load(Ordering::Acquire);
        status.video_sealed = video_ending;
        let mut network_backpressure = false;
        // A request is not EOS. While the actual producer is up to2s behind,
        // retain its sink and continue encoding packets on their original PTS.
        if stopping {status.phase=Phase::Finishing;}
        if audio_ending && !stopping && !audio_cut_requested {
            // Explicit track EOF seals at the currently admitted common video
            // clock, too; a delayed Program producer is not an empty EOS.
            if let (Some(origin),Some(last),Some(p))=(origin,last_video,pump.as_ref()){
                let end=(u128::from(last+1)*1_000_000_000*u128::from(config.video.fps_den())).div_ceil(u128::from(config.video.fps_num()));
                let end=u64::try_from(end).ok().and_then(|end|origin.0.checked_add(end)).ok_or(Error::Limit("stream finite audio cutoff"))?;
                p.stop_at(MediaTime(end))?;audio_cut_requested=true;
            }
        }
        // Every submitted operation reserved its worst-case reply. Poll both
        // tracks even while data queues are full: do not mask native faults/EOS.
        if let Some(v) = ports.video.as_mut() {
            if let Some(e) = v.poll()? {
                let mut emitted=[0i64;4];let mut count=0usize;
                if let CodecEvent::Packets {packets,..}=&e {
                    for packet in packets.iter().take(emitted.len()) {emitted[count]=packet.pts;count+=1;}
                }
                codec_event(
                    e,
                    Track::Video,
                    &mut video_header,
                    &mut outbox,
                    &mut video_drained,
                    status,
                    video_finish,
                )?;
                for pts in &emitted[..count] {super::ndi_receipt::accept(status,&mut ndi_candidates,*pts);super::camera_receipt::accept(status,&mut camera_candidates,*pts);}
            }
        }
        if let Some(a) = ports.audio.as_mut() {
            if let Some(e) = a.poll()? {
                codec_event(
                    e,
                    Track::Audio,
                    &mut audio_header,
                    &mut outbox,
                    &mut audio_drained,
                    status,
                    audio_finish,
                )?;
            }
        }
        if ports.publisher.is_none()
            && video_header.is_some()
            && (config.audio.is_none() || audio_header.is_some())
            && !stopping
        {
            status.phase = Phase::Connecting;
            publish(state, status);
            ports.publisher = Some(factory.publisher(
                config,
                codec_key,
                video_header.clone().unwrap(),
                audio_header.clone(),
            )?);
        }
        if let Some(p) = ports.publisher.as_mut() {
            if let Some(e) = p.poll()? {
                match e {
                    PublishEvent::Publishing => {
                        if publishing {
                            return Err(Error::State("duplicate Publishing"));
                        }
                        publishing = true;status.ever_published=true;
                    }
                    PublishEvent::Progress { packets } => {
                        status.packets_published = status.packets_published.max(published_base.checked_add(packets).ok_or(Error::Limit("published packet total"))?)
                    }
                    PublishEvent::Cancelled => {
                        if stopping && !started {
                            return Ok(());
                        }
                        return Err(Error::Transport("publisher cancelled unexpectedly".into()));
                    }
                    PublishEvent::Stopped { packets } => {
                        status.packets_published = status.packets_published.max(published_base.checked_add(packets).ok_or(Error::Limit("published packet total"))?);
                        if !publisher_stop {
                            return Err(Error::State("publisher stopped unexpectedly"));
                        }
                        return Ok(());
                    }
                }
            }
            if let Some(packet) = outbox.front()? {
                if p.packet(&packet)? {
                    add(&mut status.packets_admitted, 1)?;
                    outbox.pop()?;
                } else {
                    network_backpressure = true;
                }
            }
        }
        if publishing && !ready && !stopping {
            status.phase = Phase::ReadyAwaitingInput;
            publish(state, status);
            status.ready_for_inputs=true;publish(state,status);
            ready = true;
        }
        if epoch!=key.output&&origin.is_none(){if let Some(p)=pump.as_mut(){p.discard_reconnecting(status)?;}}
        let mut work_empty = false;
        if pending.is_none() {
            match work.try_recv() {
                Ok(Work::Activate(frame, permit,generation)) if generation==epoch => {
                    if !ready || started || origin.is_some() {
                        return Err(Error::State("activation outside ReadyForInputs"));
                    }
                    pending = Some((frame, permit, true));
                }
                Ok(Work::Video(frame, permit,generation)) if generation==epoch => {
                    if origin.is_none() {
                        return Err(Error::State("video before frame0"));
                    }
                    pending = Some((frame, permit, false));
                }
                Ok(Work::Activate(_,_,_)|Work::Video(_,_,_))=>{add(&mut status.reconnect_video_discarded,1)?;},
                Err(TryRecvError::Empty) => {
                    work_empty = true;
                }
                Err(TryRecvError::Disconnected) => return Err(Error::Disconnected),
            }
        }
        // A stop before activation has no media clock and no fabricated frame.
        if stopping
            && origin.is_none()
            && pending.is_none()
            && video_slots.load(Ordering::Acquire) & 3 == 0
        {
            pending = None;
            while work.try_recv().is_ok() {}
            if let Some(p) = ports.publisher.as_mut() {
                if !publisher_stop {
                    p.cancel();
                    publisher_stop = true;
                }
                if p.finished() {
                    return Ok(());
                }
            } else {
                return Ok(());
            }
        }
        if let Some((frame, _, activation)) = pending.as_ref() {
            // Validate even a superseded/same-tick frame: stale data is never a hit.
            let index = config.validate_frame(frame, origin)?;
            if last_video.is_some_and(|last| index <= last) {
                pending = None;
            } else if ports.video.as_ref().is_some_and(|v| !v.busy())
                && outbox.can_reserve_video_encode()?
                && outbox.video_time_admitted(i128::from(index)*10_000_000*i128::from(config.video.fps_den())/i128::from(config.video.fps_num()),((10_000_000*i128::from(config.video.fps_den())+i128::from(config.video.fps_num())-1)/i128::from(config.video.fps_num())))?
                // Actual first audio may be2s late. Future reply byte credits
                // and the bounded encoded outbox, not a raw frame FIFO, gate us.
            {
                let prepared = match renderer.render(config, frame){
                    Ok(prepared)=>prepared,
                    Err(Error::AutoTextRetry)=>{thread::sleep(Duration::from_millis(5));continue;},
                    Err(error)=>return Err(error),
                };
                let trace=renderer.take_delay_trace();
                if !prepared.auto_text_current(){thread::sleep(Duration::from_millis(5));continue;}
                if *activation {
                    if index != 0 {return Err(Error::State("initial frame must be0"));}
                    origin = Some(frame.time);
                    status.origin = origin;
                    if let Some(p) = pump.as_mut() {
                        p.activate(frame.time)?;if let Some(offset)=p.current_offset(){outbox.audio_offset(offset)?;status.audio_offset_100ns=Some(offset);audio_offset_fixed=true;}
                    }
                }
                if !outbox.reserve_video_encode()? {
                    return Err(Error::State("video reply reservation changed"));
                }
                if prepared.warnings.is_some() {
                    add(&mut status.degraded_frames, 1)?;
                }
                status.warnings = prepared.warnings;
                ports
                    .video
                    .as_mut()
                    .ok_or(Error::State("video encoder missing"))?
                    .encode(index, prepared.nv12)?;
                let start_ns=(u128::from(index)*1_000_000_000*u128::from(config.video.fps_den()))/u128::from(config.video.fps_num());
                let end_ns=((u128::from(index)+1)*1_000_000_000*u128::from(config.video.fps_den()))/u128::from(config.video.fps_num());
                let pts_100ns=i64::try_from(start_ns/100).map_err(|_|Error::Limit("stream video proof PTS"))?;
                let start=u64::try_from(start_ns).ok().and_then(|n|origin?.0.checked_add(n)).map(MediaTime);
                let end=u64::try_from(end_ns).ok().and_then(|n|origin?.0.checked_add(n)).map(MediaTime);
                let receipts=match (start,end){(Some(start),Some(end))=>super::ndi_receipt::candidates(frame,&trace,key,epoch,index,start,end,&ndi_lifetime.0),_=>Vec::new()};
                if ndi_candidates.len()>=32 {ndi_candidates.pop_front();}
                if ndi_candidates.try_reserve(1).is_ok(){ndi_candidates.push_back((pts_100ns,receipts));}
                let camera_receipts=match (start,end){(Some(start),Some(end))=>super::camera_receipt::candidates(frame,&trace,key,epoch,index,start,end,&ndi_lifetime.0),_=>Vec::new()};
                if camera_candidates.len()>=32 {camera_candidates.pop_front();}
                if camera_candidates.try_reserve(1).is_ok(){camera_candidates.push_back((pts_100ns,camera_receipts));}
                let expected = last_video.map_or(0, |last: u64| last.saturating_add(1));
                add(
                    &mut status.skipped_video_frames,
                    index.saturating_sub(expected),
                )?;
                add(&mut status.video_frames, 1)?;
                last_video = Some(index);
                if !started {
                    status.phase = Phase::Publishing;status.ready_for_inputs=false;status.connection_started=true;status.reconnect_attempt=0;
                    started = true;
                    publish(state, status);
                    event(
                        reply,
                        Event::Started {
                            key,
                            epoch,
                            origin: frame.time,
                        },
                    )?;
                }
                pending = None;
            }
        }
        if origin.is_some() {
            if let (Some(pump), Some(aac)) = (pump.as_mut(), ports.audio.as_mut()) {
                if !aac.busy() && !audio_finish {
                    if pending_audio.is_none() {
                        pending_audio = pump.step(status)?;
                    }
                    if let Some((first, samples)) = pending_audio.as_ref() {
                        let channels = if config
                            .audio
                            .ok_or(Error::State("AAC settings missing"))?
                            .channels()
                            == 1
                        {
                            1
                        } else {
                            2
                        };
                        if samples.is_empty()
                            || samples.len() % channels != 0
                            || samples.len() > 2048
                            || samples.len() * std::mem::size_of::<i16>() > 4096
                        {
                            return Err(Error::Limit("pending streaming PCM chunk"));
                        }
                        if !audio_offset_fixed{let offset=pump.current_offset().ok_or(Error::State("AAC PCM before observed reconnect audio origin"))?;outbox.audio_offset(offset)?;status.audio_offset_100ns=Some(offset);audio_offset_fixed=true;}
                        let frames = samples.len() / channels;
                        if outbox.reserve_audio_encode(*first, frames)? {
                            let (first, samples) = pending_audio
                                .take()
                                .ok_or(Error::State("reserved AAC input absent"))?;
                            aac.encode(first, samples)?;
                        }
                    } else if pump.done() {
                        if !audio_ending {
                            return Err(Error::Audio(
                                "Program producer ended before streaming Stop".into(),
                            ));
                        }
                        if !outbox.reserve_audio_finish()? {
                            return Err(Error::State("audio future drain credit lost"));
                        }
                        aac.finish()?;
                        audio_finish = true;
                    }
                }
            }
        }
        if video_ending
            && origin.is_some()
            && pending.is_none()
            && work_empty
            && video_slots.load(Ordering::Acquire) & 3 == 0
            && !video_finish
            && ports.video.as_ref().is_some_and(|v| !v.busy())
        {
            if !outbox.reserve_video_finish()? {
                return Err(Error::State("video drain reservation changed"));
            }
            ports.video.as_mut().unwrap().finish()?;
            video_finish = true;
        }
        if stopping && video_finish && !audio_cut_requested {
            if let (Some(origin),Some(last),Some(p))=(origin,last_video,pump.as_ref()){
                let end=(u128::from(last+1)*1_000_000_000*u128::from(config.video.fps_den())).div_ceil(u128::from(config.video.fps_num()));
                let end=u64::try_from(end).ok().and_then(|end|origin.0.checked_add(end)).ok_or(Error::Limit("stream final Program cutoff"))?;
                p.stop_at(MediaTime(end))?;
            }else if let Some(p)=pump.as_ref(){p.stop();}
            audio_cut_requested=true;
        }
        if video_drained && outbox.gated() && status.video_packets == 0 {
            return Err(Error::State("no AVC packet before EOS"));
        }
        if audio_drained && config.audio.is_some() && outbox.gated() && status.audio_packets == 0 {
            return Err(Error::State("no AAC packet before EOS"));
        }
        if stopping && origin.is_some() && video_drained && audio_drained {
            // Enabled audio without one actual AU cannot choose a shared shift.
            if outbox.gated() {
                return Err(Error::State("no actual audio/video packet before EOS"));
            }
            if outbox.is_empty() && !publisher_stop {
                ports
                    .publisher
                    .as_mut()
                    .ok_or(Error::State("publisher missing"))?
                    .stop();
                publisher_stop = true;
            }
        }
        status.audio_sealed =
            audio_ending && (origin.is_some() || video_slots.load(Ordering::Acquire) & 3 == 0);
        status.interleave = outbox.state();
        if network_backpressure {
            status.interleave.waiting = InterleaveWait::Network;
        }
        publish(state, status);
        thread::sleep(Duration::from_millis(2));
    }
}
fn codec_event(
    event: CodecEvent,
    track: Track,
    header: &mut Option<Arc<[u8]>>,
    outbox: &mut Outbox,
    drained: &mut bool,
    status: &mut Status,
    finish_sent: bool,
) -> Result<(), Error> {
    match event {
        CodecEvent::Ready(value) => {
            if header.is_some() {
                return Err(Error::State("duplicate codec Ready"));
            }
            if value.is_empty() || value.len() > if track == Track::Video { 32768 } else { 64 } {
                return Err(Error::Limit("codec header"));
            }
            *header = Some(value);
        }
        CodecEvent::Packets {
            header: value,
            packets,
            drained: eos,
            aac,
        } => {
            if header
                .as_ref()
                .is_none_or(|old| old.as_ref() != value.as_ref())
            {
                return Err(Error::State("codec configuration changed during output"));
            }
            if packets.len() > 4 {
                return Err(Error::Limit("codec packet batch"));
            }
            if eos && !finish_sent {
                return Err(Error::State("unexpected codec EOS"));
            }
            if let Some(stats) = aac {
                status.native_aac = stats;
            }
            let stats = outbox.admit_batch(track, packets, eos)?;
            add(&mut status.encoded_bytes, stats.bytes)?;
            match track {
                Track::Video => add(&mut status.video_packets, stats.packets)?,
                Track::Audio => add(&mut status.audio_packets, stats.packets)?,
            };
            *drained = stats.drained;
        }
        CodecEvent::Closed => return Err(Error::State("encoder closed before drain")),
    }
    Ok(())
}

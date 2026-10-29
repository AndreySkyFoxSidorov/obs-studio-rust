//! APP TEST ONLY metadata service. It does not emulate RTMP or encode pixels.
use super::*;
use skylive_host::transition::CompositionKey;
use std::{
    collections::VecDeque,
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

#[derive(Clone, Debug)]
pub(in crate::app) struct FrameSeen {
    pub(in crate::app) initial: bool,
    pub(in crate::app) time: MediaTime,
    pub(in crate::app) playback_time: MediaTime,
    pub(in crate::app) session: u64,
    pub(in crate::app) revision: u64,
    pub(in crate::app) composition: CompositionKey,
}
pub(in crate::app) struct State {
    pub(in crate::app) starts: AtomicUsize,
    pub(in crate::app) stops: AtomicUsize,
    pub(in crate::app) cancels: AtomicUsize,
    pub(in crate::app) closed: AtomicBool,
    key: Mutex<Option<StreamKey>>,
    phase: Mutex<Phase>,
    consumed_ready: AtomicBool,
    pub(in crate::app) events: Mutex<VecDeque<Event>>,
    pub(in crate::app) frames: Mutex<Vec<FrameSeen>>,
}
impl Default for State {
    fn default() -> Self {
        Self {
            starts: AtomicUsize::new(0),
            stops: AtomicUsize::new(0),
            cancels: AtomicUsize::new(0),
            closed: AtomicBool::new(false),
            key: Mutex::new(None),
            phase: Mutex::new(Phase::Initializing),
            consumed_ready: AtomicBool::new(false),
            events: Mutex::new(VecDeque::new()),
            frames: Mutex::new(Vec::new()),
        }
    }
}
impl State {
    pub(in crate::app) fn key(&self) -> StreamKey {
        self.key.lock().unwrap().unwrap()
    }
    pub(in crate::app) fn ready(&self) {
        *self.phase.lock().unwrap() = Phase::ReadyAwaitingInput;
        self.events
            .lock()
            .unwrap()
            .push_back(Event::ReadyForInputs {
                key: self.key(),
                epoch: 11,
            });
    }
}
struct Fake(Arc<State>);
pub(in crate::app) fn factory(state: Arc<State>) -> ServiceFactory {
    Box::new(move |config| {
        let output = state.starts.fetch_add(1, Ordering::SeqCst) + 1;
        *state.key.lock().unwrap() = Some(StreamKey {
            session: config.session(),
            output: output as u64,
        });
        state.closed.store(false, Ordering::SeqCst);
        assert!(
            config.audio().is_none(),
            "this metadata-only fixture must not silently skip an audio sink"
        );
        Ok(StartedService {
            owner: Box::new(Fake(state.clone())),
            audio_sink: None,
        })
    })
}
impl Service for Fake {
    fn key(&self) -> StreamKey {
        self.0.key()
    }
    fn epoch(&self) -> u64 {
        11
    }
    fn status(&self) -> Option<Status> {
        Some(Status {
            key: self.key(),
            phase: *self.0.phase.lock().unwrap(),
            origin: None,
            video_frames: 0,
            skipped_video_frames: 0,
            audio_input_frames: 0,
            audio_converted_frames: 0,
            audio_gap_frames: 0,
            video_packets: 0,
            audio_packets: 0,
            packets_admitted: 0,
            packets_published: 0,
            encoded_bytes: 0,
            degraded_frames: 0,
            warnings: None,
            failure: None,
            native_aac: Default::default(),
            video_sealed: false,
            audio_sealed: false,
            interleave: Default::default(),
        })
    }
    fn poll(&mut self) -> Option<Event> {
        let event = self.0.events.lock().unwrap().pop_front()?;
        if matches!(event, Event::ReadyForInputs {key,epoch} if key==self.key() && epoch==self.epoch())
        {
            self.0.consumed_ready.store(true, Ordering::SeqCst);
        }
        Some(event)
    }
    fn activate(
        &mut self,
        key: StreamKey,
        epoch: u64,
        frame: ProgramVideoFrame,
    ) -> Result<(), RejectedFrame> {
        assert_eq!(key, self.key());
        assert_eq!(epoch, self.epoch());
        assert!(
            self.0.consumed_ready.swap(false, Ordering::SeqCst),
            "activation requires consumed matching Ready"
        );
        *self.0.phase.lock().unwrap() = Phase::Publishing;
        self.observe(true, frame);
        Ok(())
    }
    fn video(
        &mut self,
        key: StreamKey,
        epoch: u64,
        frame: ProgramVideoFrame,
    ) -> Result<(), RejectedFrame> {
        assert_eq!(key, self.key());
        assert_eq!(epoch, self.epoch());
        self.observe(false, frame);
        Ok(())
    }
    fn stop(&self) {
        self.0.stops.fetch_add(1, Ordering::SeqCst);
    }
    fn cancel(&self) {
        self.0.cancels.fetch_add(1, Ordering::SeqCst);
    }
    fn is_closed(&self) -> bool {
        self.0.closed.load(Ordering::SeqCst)
    }
}
impl Fake {
    fn observe(&self, initial: bool, frame: ProgramVideoFrame) {
        self.0.frames.lock().unwrap().push(FrameSeen {
            initial,
            time: frame.time,
            playback_time: frame.playback.now(),
            session: frame.session,
            revision: frame.model.revision(),
            composition: frame.output.sample_key(),
        });
    }
}

pub(in crate::app) fn configure(app: &mut StudioApp) {
    let id = app.session.snapshot().profiles().selected_id();
    app.apply(Command::SetVideo {
        width: 64,
        height: 48,
        fps_num: 30,
        fps_den: 1,
    });
    app.apply(Command::Profile {
        command: skylive_core::ProfileCommand::SetStreaming {
            id,
            settings: skylive_core::StreamingSettings::new(
                "rtmp://127.0.0.1/live".into(),
                "never-connected-test".into(),
                1000,
                2,
                false,
                160,
            )
            .unwrap(),
        },
    });
}
pub(in crate::app) fn frame(
    app: &mut StudioApp,
    ctx: &egui::Context,
    now: u64,
    events: Vec<egui::Event>,
) -> egui::FullOutput {
    ctx.run(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1280.0, 800.0),
            )),
            events,
            ..Default::default()
        },
        |ctx| app.frame_at(ctx, MediaTime(now)),
    )
}
fn point(shape: &egui::epaint::Shape, label: &str) -> Option<egui::Pos2> {
    match shape {
        egui::epaint::Shape::Text(text) if text.galley.text() == label => {
            Some(text.pos + text.galley.size() * 0.5)
        }
        egui::epaint::Shape::Vec(items) => items.iter().find_map(|shape| point(shape, label)),
        _ => None,
    }
}
pub(in crate::app) fn click(app: &mut StudioApp, ctx: &egui::Context, english: &str) {
    let now = app.frame_now.0;
    let _ = frame(app, ctx, now, vec![]);
    let output = frame(app, ctx, now, vec![]);
    let label = crate::i18n::tr_context(ctx, english);
    let pos = output
        .shapes
        .iter()
        .find_map(|shape| point(&shape.shape, &label).filter(|p| shape.clip_rect.contains(*p)))
        .unwrap_or_else(|| panic!("actual painted {label}"));
    for pressed in [true, false] {
        let _ = frame(
            app,
            ctx,
            now,
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
    }
}

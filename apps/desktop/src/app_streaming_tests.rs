use super::{test_support::*, *};
use std::sync::atomic::Ordering;

fn fixture() -> (egui::Context, StudioApp, Arc<State>) {
    let ctx = egui::Context::default();
    crate::theme::install(&ctx);
    let mut app = StudioApp::headless(&ctx).unwrap();
    let state = Arc::new(State::default());
    app.streaming = Runtime::with_factory(factory(state.clone()));
    (ctx, app, state)
}
fn source(app: &mut StudioApp, name: &str, rgba: [u8; 4]) {
    app.apply(Command::AddColorSource {
        scene: app.session.preview(),
        name: name.into(),
        rgba,
    });
}
fn activate(app: &mut StudioApp, ctx: &egui::Context, state: &State, now: u64) {
    state.ready();
    let _ = frame(app, ctx, now, vec![]);
    assert!(app.streaming.origin.is_some());
}

#[test]
fn unconfigured_button_and_busy_document_intent_do_not_construct_service() {
    let (ctx, mut app, state) = fixture();
    click(&mut app, &ctx, "Start Streaming");
    assert_eq!(state.starts.load(Ordering::SeqCst), 0);
    app.intent(Intent::StartStreaming, &ctx);
    assert_eq!(state.starts.load(Ordering::SeqCst), 0);
    configure(&mut app);
    let missing = std::env::temp_dir().join(format!(
        "streaming-seam-missing-{}-never-created",
        std::process::id()
    ));
    app.io
        .submit(app.session.identity(), Job::Load(missing))
        .unwrap();
    // I/O remains admitted until its completion is consumed, even if already finished.
    assert!(app.io.busy());
    app.intent(Intent::StartStreaming, &ctx);
    assert_eq!(state.starts.load(Ordering::SeqCst), 0);
    assert!(!app.streaming.active());
}

#[test]
fn busy_shared_audio_control_does_not_construct_stream_owner() {
    let (ctx, mut app, state) = fixture();
    configure(&mut app);
    let id = app.session.snapshot().profiles().selected_id();
    app.apply(Command::Profile {
        command: skylive_core::ProfileCommand::SetStreaming {
            id,
            settings: skylive_core::StreamingSettings::new(
                "rtmp://127.0.0.1/live".into(),
                "never-connected-test".into(),
                1000,
                2,
                true,
                160,
            )
            .unwrap(),
        },
    });
    let output = app.sync_transition().unwrap().output;
    app.audio
        .synchronize(&app.session, &output.scenes())
        .unwrap();
    app.audio
        .request_program(&app.session, &output, MediaTime(0))
        .unwrap();
    assert!(app.audio.is_busy());
    app.intent(Intent::StartStreaming, &ctx);
    assert_eq!(state.starts.load(Ordering::SeqCst), 0);
    assert!(!app.streaming.active());
}

#[test]
fn configured_actual_start_click_resets_previous_warning_projection() {
    let (ctx, mut app, state) = fixture();
    configure(&mut app);
    let output = app.sync_transition().unwrap().output;
    app.streaming.view.warnings = Some(Arc::new(skylive_host::recording::RecordingWarnings {
        session: app.session.identity(),
        revision: app.session.snapshot().revision(),
        time_ns: 0,
        resource_generation: 0,
        capture_epoch: 0,
        capture_generation: 0,
        output: output.identity(),
        composition: output.sample_key(),
        diagnostics: vec![],
        source_names: vec![],
        omitted: 3,
    }));
    app.streaming.view.degraded_video_frames = 17;
    // Actual Controls press/release, not a manually fabricated Start intent.
    click(&mut app, &ctx, "Start Streaming");
    assert_eq!(state.starts.load(Ordering::SeqCst), 1);
    assert!(app.streaming.view.warnings.is_none());
    assert_eq!(app.streaming.view.degraded_video_frames, 0);
    assert_eq!(app.streaming.view.phase, Some(Phase::Initializing));
    assert!(state.frames.lock().unwrap().is_empty());
    state.closed.store(true, Ordering::SeqCst);
}

#[test]
fn unread_and_stale_ready_cannot_activate_then_matching_ready_samples_current_manual_output() {
    let (ctx, mut app, state) = fixture();
    configure(&mut app);
    source(&mut app, "Program red", [255, 0, 0, 255]);
    app.intent(Intent::StudioMode(true), &ctx);
    app.apply(Command::AddScene {
        name: "Unsent blue".into(),
    });
    source(&mut app, "Preview blue", [0, 0, 255, 255]);
    app.intent(Intent::ManualPosition(512), &ctx);
    let old = app.sync_transition().unwrap().output;
    assert!(old.is_manual());
    click(&mut app, &ctx, "Start Streaming");
    // Even a fresh valid frame cannot activate without a consumed Ready event.
    let sample = skylive_host::playback::PlaybackSnapshot::at_time(&app.session, MediaTime(0));
    let capture = Arc::new(CaptureSnapshot::empty(app.session.identity()));
    state.ready(); // Matching Ready exists but has NOT been consumed by the app.
    app.stream_program_frame(&ctx, &sample, &old, &capture);
    assert!(state.frames.lock().unwrap().is_empty());
    state.events.lock().unwrap().clear();
    let key = state.key();
    state.events.lock().unwrap().extend([
        Event::ReadyForInputs {
            key: StreamKey {
                session: key.session,
                output: key.output + 1,
            },
            epoch: 11,
        },
        Event::ReadyForInputs { key, epoch: 12 },
        Event::ReadyForInputs {
            key: StreamKey {
                session: key.session + 1,
                output: key.output,
            },
            epoch: 11,
        },
    ]);
    let _ = frame(&mut app, &ctx, 100_000_000, vec![]);
    assert!(state.frames.lock().unwrap().is_empty());
    assert!(app.streaming.origin.is_none());
    activate(&mut app, &ctx, &state, 500_000_000);
    let current = app.sync_transition().unwrap().output.sample_key();
    assert_ne!(
        old.sample_key(),
        current,
        "Manual output really advanced during connection"
    );
    let frames = state.frames.lock().unwrap();
    assert_eq!(frames.len(), 1);
    let seen = &frames[0];
    assert!(seen.initial);
    assert_eq!(seen.time, MediaTime(500_000_000));
    assert_eq!(seen.playback_time, seen.time);
    assert_eq!(seen.composition, current);
    assert_eq!(seen.session, app.session.identity());
    assert_eq!(seen.revision, app.session.snapshot().revision());
    assert_eq!(app.streaming.origin, Some(seen.time));
    state.closed.store(true, Ordering::SeqCst);
}

#[test]
fn starting_cancel_and_publishing_stop_use_distinct_service_controls() {
    let (ctx, mut app, state) = fixture();
    configure(&mut app);
    click(&mut app, &ctx, "Start Streaming");
    click(&mut app, &ctx, "Cancel Streaming Start");
    assert_eq!(state.cancels.load(Ordering::SeqCst), 1);
    assert_eq!(state.stops.load(Ordering::SeqCst), 0);
    assert!(app.streaming.view.finishing);
    assert!(
        app.streaming.active(),
        "Cancel is not an SDK-retirement acknowledgement"
    );
    state.closed.store(true, Ordering::SeqCst);

    let (ctx, mut app, state) = fixture();
    configure(&mut app);
    click(&mut app, &ctx, "Start Streaming");
    activate(&mut app, &ctx, &state, 1_000_000);
    click(&mut app, &ctx, "Stop Streaming");
    assert_eq!(state.stops.load(Ordering::SeqCst), 1);
    assert_eq!(state.cancels.load(Ordering::SeqCst), 0);
    assert!(app.streaming.active());
    assert!(app.streaming.view.finishing);
    state.closed.store(true, Ordering::SeqCst);
}

#[test]
fn activation_rejects_old_playback_time_revision_and_foreign_session_before_service_call() {
    let (ctx, mut app, state) = fixture();
    configure(&mut app);
    click(&mut app, &ctx, "Start Streaming");
    state.ready();
    app.poll_streaming(&ctx);
    let stale = skylive_host::playback::PlaybackSnapshot::at_time(&app.session, MediaTime(0));
    let capture = Arc::new(CaptureSnapshot::empty(app.session.identity()));
    let output = app.sync_transition().unwrap().output;
    app.frame_now = MediaTime(100);
    app.stream_program_frame(&ctx, &stale, &output, &capture);
    assert!(state.frames.lock().unwrap().is_empty());
    app.frame_now = MediaTime(0);
    source(&mut app, "New authored source", [1, 2, 3, 255]);
    app.stream_program_frame(&ctx, &stale, &output, &capture);
    assert!(state.frames.lock().unwrap().is_empty());
    let foreign = skylive_core::Session::new();
    let sample = skylive_host::playback::PlaybackSnapshot::at_time(&foreign, MediaTime(0));
    app.stream_program_frame(&ctx, &sample, &output, &capture);
    assert!(state.frames.lock().unwrap().is_empty());
    let current = app.sync_transition().unwrap().output;
    let sample = skylive_host::playback::PlaybackSnapshot::at_time(&app.session, MediaTime(0));
    app.stream_program_frame(&ctx, &sample, &current, &capture);
    assert_eq!(state.frames.lock().unwrap().len(), 1);
    state.closed.store(true, Ordering::SeqCst);
}

#[test]
fn stop_intent_bypasses_real_admitted_document_job_but_does_not_publish_edits() {
    let (ctx, mut app, state) = fixture();
    configure(&mut app);
    click(&mut app, &ctx, "Start Streaming");
    activate(&mut app, &ctx, &state, 1_000_000);
    let model = app.session.snapshot();
    let missing = std::env::temp_dir().join(format!(
        "streaming-stop-seam-{}-missing",
        std::process::id()
    ));
    app.io
        .submit(app.session.identity(), Job::Load(missing))
        .unwrap();
    assert!(app.io.busy());
    app.intent(Intent::StopStreaming, &ctx);
    assert_eq!(state.stops.load(Ordering::SeqCst), 1);
    assert!(Arc::ptr_eq(&model, &app.session.snapshot()));
    assert!(
        app.io.busy(),
        "Stop must not consume or replace document completion"
    );
    state.closed.store(true, Ordering::SeqCst);
}

#[test]
fn shutdown_retains_session_through_streaming_retirement() {
    let (ctx, mut app, state) = fixture();
    configure(&mut app);
    click(&mut app, &ctx, "Start Streaming");
    app.shutdown(&ctx);
    assert!(!app.allow_close);
    assert!(!app.session.is_closed());
    assert_eq!(state.cancels.load(Ordering::SeqCst), 1);
    // Multiple polls/frames do not substitute for an actual closed acknowledgement.
    let _ = frame(&mut app, &ctx, 1, vec![]);
    assert!(!app.allow_close);
    assert!(!app.session.is_closed());
    state.closed.store(true, Ordering::SeqCst);
    app.poll_streaming(&ctx);
    assert!(app.allow_close);
    assert!(app.session.is_closed());
}

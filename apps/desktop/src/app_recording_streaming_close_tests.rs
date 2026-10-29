// Include inside app_recording::tests to reuse its existing fake writer.
use crate::app::streaming::{Runtime as StreamRuntime, test_support as streaming_test};

#[test]
fn cancel_dirty_exit_preserves_both_outputs_then_discard_waits_for_both_actual_retirements() {
    for stream_retires_first in [true, false] {
        let directory = Directory::new();
        let writer = Arc::new(State::default());
        let _release = Release(writer.clone());
        writer.hold_finalize.store(true, Ordering::SeqCst);
        let ctx = egui::Context::default();
        let mut app = fixture(&ctx, &directory, writer.clone());
        let stream = Arc::new(streaming_test::State::default());
        app.streaming = StreamRuntime::with_factory(streaming_test::factory(stream.clone()));
        streaming_test::configure(&mut app);
        click(&mut app, &ctx, "Start Recording");
        settle(&mut app, &ctx, |_| {
            writer
                .calls
                .lock()
                .unwrap()
                .iter()
                .any(|c| matches!(c, Call::Video(..)))
        });
        click(&mut app, &ctx, "Start Streaming");
        stream.ready();
        let now = app.frame_now.0;
        let _ = frame(&mut app, &ctx, now, vec![]);
        assert!(app.streaming.active() && app.recording.active());

        let before = app.session.snapshot();
        click(&mut app, &ctx, "Exit");
        assert!(app.close_dialog);
        click(&mut app, &ctx, "Cancel");
        assert!(!app.close_dialog && !app.allow_close && !app.session.is_closed());
        assert!(Arc::ptr_eq(&before, &app.session.snapshot()));
        assert!(app.recording.active() && app.streaming.active());
        assert_eq!(stream.stops.load(Ordering::SeqCst), 0);
        assert_eq!(stream.cancels.load(Ordering::SeqCst), 0);
        assert!(
            !writer
                .calls
                .lock()
                .unwrap()
                .iter()
                .any(|c| matches!(c, Call::Finalize(..)))
        );

        click(&mut app, &ctx, "Exit");
        click(&mut app, &ctx, "Discard and close");
        assert!(!app.allow_close && !app.session.is_closed());
        settle(&mut app, &ctx, |_| {
            writer
                .calls
                .lock()
                .unwrap()
                .iter()
                .any(|c| matches!(c, Call::Finalize(..)))
        });
        assert!(stream.stops.load(Ordering::SeqCst) > 0);
        if stream_retires_first {
            stream.closed.store(true, Ordering::SeqCst);
            app.poll_streaming(&ctx);
            assert!(
                !app.allow_close && !app.session.is_closed(),
                "Recording still owns blocked finalization"
            );
            writer.hold_finalize.store(false, Ordering::SeqCst);
        } else {
            writer.hold_finalize.store(false, Ordering::SeqCst);
            settle(&mut app, &ctx, |app| !app.recording.active());
            assert!(
                !app.allow_close && !app.session.is_closed(),
                "Streaming has not acknowledged retirement"
            );
            stream.closed.store(true, Ordering::SeqCst);
        }
        settle(&mut app, &ctx, |app| app.allow_close);
        assert!(app.session.is_closed());
        app.recording.owner.join().unwrap();
    }
}

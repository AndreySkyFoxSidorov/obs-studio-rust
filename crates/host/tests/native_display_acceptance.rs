//! Explicit opt-in native capture acceptance. Never run by the default test suite.
//! Retains no screenshot files and prints no captured pixels.
#[cfg(target_os = "windows")]
mod windows {
    use skylive_core::{Command, Session};
    use skylive_host::{
        capture::{self, CaptureEvent, CaptureWorker},
        playback::PlaybackSnapshot,
        preview,
        transition::{FrameComposition, FrameScene},
    };
    use skylive_media::MediaTime;
    use std::time::{Duration, Instant};

    #[test]
    #[ignore = "explicit user-authorized live Windows display capture; no screenshot files"]
    fn native_display_worker_captures_renders_and_releases_actual_sdr_frames() {
        let mut owner = Session::new();
        let mut worker = CaptureWorker::new(capture::native::backend()).unwrap();
        assert!(worker.capabilities().available);
        worker.request_displays(&owner).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let displays = loop {
            if let Some(CaptureEvent::Displays { result, .. }) =
                worker.poll(&owner, MediaTime(0)).unwrap()
            {
                break result.unwrap();
            }
            assert!(
                Instant::now() < deadline,
                "native display enumeration timed out"
            );
            std::thread::sleep(Duration::from_millis(5));
        };
        let display = displays
            .iter()
            .find(|display| display.primary)
            .or_else(|| displays.first())
            .expect("a live display must exist");
        let binding = display.binding(false).unwrap();
        let scene = owner.preview();
        owner
            .apply(Command::AddDisplayCaptureSource {
                scene,
                name: "Native display acceptance".into(),
                binding,
            })
            .unwrap();
        worker
            .synchronize(&owner, &[FrameScene::Live(scene)])
            .unwrap();
        for time in [0, 1_000_000_000] {
            let now = MediaTime(time);
            assert!(worker.request_frame(&owner, now).unwrap());
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                if matches!(
                    worker.poll(&owner, now).unwrap(),
                    Some(CaptureEvent::Frames { .. })
                ) {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "native frame acquisition timed out"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
            let captured = worker.snapshot();
            assert!(captured.problems().is_empty(), "{:?}", captured.problems());
            assert_eq!(captured.frame_count(), 1);
            for (_, frame) in captured.frames() {
                assert_eq!(
                    (frame.image.width(), frame.image.height()),
                    (display.width, display.height)
                );
                assert_eq!(
                    frame.image.pixels().len(),
                    display.width as usize * display.height as usize * 4
                );
                assert!(
                    frame
                        .image
                        .pixels()
                        .chunks_exact(4)
                        .all(|pixel| pixel[3] == 255)
                );
            }
            let rendered = preview::render_output_with_capture(
                &owner.snapshot(),
                &FrameComposition::Scene(FrameScene::Live(scene)),
                160,
                90,
                &PlaybackSnapshot::at_time(&owner, now),
                &captured,
            )
            .unwrap();
            assert_eq!((rendered.width(), rendered.height()), (160, 90));
            assert!(
                rendered
                    .pixels()
                    .chunks_exact(4)
                    .all(|pixel| pixel[3] == 255)
            );
            println!(
                "native SDR frame+CPU composition passed: {}x{} ->160x90; no captured pixels persisted",
                display.width, display.height
            );
            drop(rendered);
            drop(captured);
        }
        worker.join().unwrap();
        assert_eq!(worker.active_source_count(), 0);
        assert_eq!(worker.snapshot().frame_count(), 0);
    }
}

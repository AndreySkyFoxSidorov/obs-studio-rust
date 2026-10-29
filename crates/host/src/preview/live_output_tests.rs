//! Actual layer rendering with injected/absent resources; no native SDK.
use super::*;
use crate::resource_issue::ResourceIssueKind;
use skylive_core::{Command, TextSource, TextStyle, Transform};

fn base() -> Session {
    let mut session = Session::new();
    session
        .apply(Command::SetVideo {
            width: 16,
            height: 16,
            fps_num: 30,
            fps_den: 1,
        })
        .unwrap();
    session
        .apply(Command::AddColorSource {
            scene: session.preview(),
            name: "Healthy green".into(),
            rgba: [0, 255, 0, 255],
        })
        .unwrap();
    session
}
fn text(content: &str) -> TextSource {
    TextSource::new(content.into(), 64, 48, 24, TextStyle::default()).unwrap()
}
fn live(
    renderer: &mut OutputRenderer,
    session: &Session,
    capture: &CaptureSnapshot,
) -> Result<RenderedScene, PreviewError> {
    renderer.render_live(
        &session.snapshot(),
        &FrameComposition::Scene(FrameScene::Live(session.preview())),
        16,
        16,
        &PlaybackSnapshot::at_time(session, MediaTime(0)),
        capture,
    )
}
fn pending_commands(scene: Id) -> Vec<Command> {
    use skylive_core::*;
    vec![
        Command::AddDisplayCaptureSource {
            scene,
            name: "Pending display".into(),
            binding: DisplayCaptureBinding::new(
                DisplayTarget::Windows {
                    device_id: "injected-display".into(),
                },
                false,
                16,
                16,
            )
            .unwrap(),
        },
        Command::AddWindowCaptureSource {
            scene,
            name: "Pending window".into(),
            binding: WindowCaptureBinding::new(
                WindowTarget::Windows {
                    title: "Injected".into(),
                    class: "Fixture".into(),
                    executable: "fixture.exe".into(),
                },
                WindowMatchPriority::Title,
                false,
                true,
                16,
                16,
            )
            .unwrap(),
        },
        Command::AddVideoCaptureSource {
            scene,
            name: "Pending camera".into(),
            binding: VideoCaptureBinding::new(
                VideoCaptureTarget::Windows {
                    symbolic_link: "injected-camera".into(),
                },
                VideoCaptureMode::new(16, 16, 30, 1, VideoPixelFormat::Bgra8).unwrap(),
                true,
            )
            .unwrap(),
        },
        Command::AddMediaSource {
            scene,
            name: "Pending movie".into(),
            media: MediaSource::new("injected-missing.mp4".into(), 16, 16, false).unwrap(),
        },
        Command::AddImageSource {
            scene,
            name: "Missing image".into(),
            asset: ImageAsset::new("injected-missing.png".into(), [0; 32], 1, 1).unwrap(),
        },
        Command::AddTextSource {
            scene,
            name: "Unsupported glyph".into(),
            settings: text("漢"),
        },
    ]
}

#[test]
fn pending_native_file_and_glyph_layers_keep_literal_healthy_pixels_but_exports_stay_strict() {
    let scene = base().preview();
    for command in pending_commands(scene) {
        // Id allocation is session-local: every fresh session's initial scene is the same ID.
        let mut session = base();
        session.apply(command).unwrap();
        let capture = CaptureSnapshot::empty(session.identity());
        let mut renderer = OutputRenderer::default();
        let actual = live(&mut renderer, &session, &capture).unwrap();
        assert_eq!(actual.diagnostics.len(), 1);
        assert_eq!(actual.diagnostics[0].kind, ResourceIssueKind::Unavailable);
        assert!(
            actual
                .image
                .pixels()
                .chunks_exact(4)
                .all(|p| p == [0, 255, 0, 255])
        );
        assert!(matches!(
            renderer.render(
                &session.snapshot(),
                &FrameComposition::Scene(FrameScene::Live(session.preview())),
                16,
                16,
                &PlaybackSnapshot::at_time(&session, MediaTime(0)),
                &capture
            ),
            Err(PreviewError::ResourcesUnavailable(_))
        ));
    }
}

#[test]
fn retained_matching_capture_pixels_survive_transient_error_and_rebind_never_reuses_them() {
    use super::capture_tests::{acquire, fixture, worker};
    use crate::capture::CaptureLimits;
    let (mut session, scene, _, source) = fixture((2, 2), Transform::full(2, 2));
    let (mut owner, _, _) = worker(
        vec![RgbaFrame::solid(2, 2, [255, 0, 0, 255]).unwrap()],
        CaptureLimits::default(),
    );
    let views = [FrameScene::Live(scene)];
    let first = acquire(&mut owner, &session, &views, 0);
    let failed = acquire(&mut owner, &session, &views, 1_000_000_000);
    assert!(failed.problem(source).is_some());
    assert_eq!(
        first.frame(source).unwrap().sequence,
        failed.frame(source).unwrap().sequence
    );
    let mut renderer = OutputRenderer::default();
    let rendered = live(&mut renderer, &session, &failed).unwrap();
    assert_eq!(&rendered.image.pixels()[..4], &[255, 0, 0, 255]);
    assert_eq!(rendered.diagnostics.len(), 1);
    session
        .apply(Command::RebindDisplayCapture {
            source,
            binding: skylive_core::DisplayCaptureBinding::new(
                skylive_core::DisplayTarget::Windows {
                    device_id: "other-display".into(),
                },
                true,
                2,
                2,
            )
            .unwrap(),
        })
        .unwrap();
    let rebound = live(&mut renderer, &session, &failed).unwrap();
    assert!(
        rebound
            .image
            .pixels()
            .chunks_exact(4)
            .all(|p| p == [0, 0, 0, 255])
    );
    assert!(!rebound.diagnostics.is_empty());
    drop((rendered, rebound, failed, first));
    owner.close();
    owner.join().unwrap();
}

#[test]
fn real_text_admission_and_whole_frame_freshness_remain_fatal() {
    let mut session = base();
    let old = PlaybackSnapshot::at_time(&session, MediaTime(0));
    session
        .apply(Command::AddTextSource {
            scene: session.preview(),
            name: "Text".into(),
            settings: text("A"),
        })
        .unwrap();
    let model = session.snapshot();
    let output = FrameComposition::Scene(FrameScene::Live(session.preview()));
    let capture = CaptureSnapshot::empty(session.identity());
    let mut renderer = OutputRenderer::default();
    renderer.cache.scopes(
        session.identity(),
        model.revision(),
        vec![SceneKey::Live(session.preview())],
    );
    // Exhaust the admitted cache metadata, without allocating a 128MiB raster.
    renderer.cache.text.bytes = crate::text::MAX_TEXT_CACHE_BYTES;
    assert!(
        matches!(live(&mut renderer,&session,&capture),Err(PreviewError::ResourcesUnavailable(issues)) if issues.iter().any(|issue|issue.kind==ResourceIssueKind::Limit))
    );
    assert!(matches!(
        renderer.render_live(&model, &output, 16, 16, &old, &capture),
        Err(PreviewError::StalePlayback)
    ));
    let foreign = CaptureSnapshot::empty(session.identity() + 1);
    assert!(matches!(
        live(&mut renderer, &session, &foreign),
        Err(PreviewError::StaleCapture)
    ));
}

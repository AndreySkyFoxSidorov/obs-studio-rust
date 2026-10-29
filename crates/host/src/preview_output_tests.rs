use super::*;
use skylive_core::{Command, Session};
#[test]
fn full_resolution_program_and_fade_use_output_budget_while_preview_stays_bounded() {
    let mut session = Session::new();
    let red = session.preview();
    session
        .apply(Command::AddColorSource {
            scene: red,
            name: "Red".into(),
            rgba: [255, 0, 0, 255],
        })
        .unwrap();
    let blue = session
        .apply(Command::AddScene {
            name: "Blue".into(),
        })
        .unwrap()
        .created
        .unwrap();
    session
        .apply(Command::AddColorSource {
            scene: blue,
            name: "Blue color".into(),
            rgba: [0, 0, 255, 255],
        })
        .unwrap();
    let model = session.snapshot();
    let sample = PlaybackSnapshot::at_time(&session, MediaTime(0));
    let capture = CaptureSnapshot::empty(session.identity());
    let mut renderer = OutputRenderer::default();
    let red_output = FrameComposition::Scene(FrameScene::Live(red));
    assert!(matches!(
        render_output_with_capture(&model, &red_output, 1920, 1080, &sample, &capture),
        Err(PreviewError::InvalidSize)
    ));
    let actual = renderer
        .render(&model, &red_output, 1920, 1080, &sample, &capture)
        .unwrap();
    assert_eq!((actual.width(), actual.height()), (1920, 1080));
    assert!(
        actual
            .pixels()
            .chunks_exact(4)
            .all(|pixel| pixel == [255, 0, 0, 255])
    );
    drop(actual);
    let fade = FrameComposition::Fade {
        from: FrameScene::Live(red),
        to: FrameScene::Live(blue),
        elapsed_ns: 500,
        duration_ns: 1000,
        epoch: 1,
    };
    let actual = renderer
        .render(&model, &fade, 1920, 1080, &sample, &capture)
        .unwrap();
    // OBS transition-fade.c deliberately uses nonlinear sRGB Fade for two
    // endpoints ("users want nonlinear fade"). Its shader lerp is 128/128.
    assert!(
        actual
            .pixels()
            .chunks_exact(4)
            .all(|pixel| pixel == [128, 0, 128, 255])
    );
    drop(actual);
    let single = FrameComposition::SingleFade {
        scene: FrameScene::Live(red),
        logical_scene: red,
        incoming: true,
        elapsed_ns: 500,
        duration_ns: 1000,
        epoch: 2,
    };
    let actual = renderer
        .render(&model, &single, 1920, 1080, &sample, &capture)
        .unwrap();
    // FadeSingle samples sRGB as linear and multiplies before framebuffer
    // encoding, so the independent null-to-red half is188 rather than128.
    assert!(
        actual
            .pixels()
            .chunks_exact(4)
            .all(|pixel| pixel == [188, 0, 0, 255])
    );
    assert!(matches!(
        renderer.render(&model, &red_output, 4096, 4096, &sample, &capture),
        Err(PreviewError::InvalidSize)
    ));
}

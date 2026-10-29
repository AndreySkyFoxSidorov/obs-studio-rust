use super::*;
use skylive_core::{Session, VideoPixelFormat};

fn info(width: u32, height: u32, fps: u32) -> CameraModeInfo {
    CameraModeInfo {
        width,
        height,
        fps_num: fps,
        fps_den: 1,
        native_subtype: "RGB32".into(),
        mode: Some(VideoCaptureMode::new(width, height, fps, 1, VideoPixelFormat::Rgb32).unwrap()),
        unsupported_reason: None,
    }
}
fn fixture(session: &Session, original: Option<VideoCaptureBinding>) -> CameraDraft {
    let target = VideoCaptureTarget::Windows {
        symbolic_link: "camera-reported-fullhd".into(),
    };
    let mut draft = CameraDraft::new(
        17,
        session.identity(),
        session.snapshot().revision(),
        CameraEdit::Add {
            scene: session.preview(),
        },
        "Camera".into(),
        original,
        CameraCapabilities {
            available: true,
            backend: "injected metadata",
            video: true,
            audio: false,
            note: None,
        },
    )
    .on_platform(crate::platform::CapturePlatform::Windows);
    draft.selected = Some(target.clone());
    draft.devices = vec![CameraDeviceInfo {
        target,
        name: "Reported native Full HD camera".into(),
    }];
    draft.loading_devices = false;
    draft
}
fn modes(draft: &mut CameraDraft, session: &Session, found: Vec<CameraModeInfo>) {
    let key = CameraRequestKey {
        instance: 1,
        session: session.identity(),
        revision: session.snapshot().revision(),
        epoch: 0,
        serial: 9,
    };
    draft.modes(
        Some((draft.token, key)),
        key,
        session.identity(),
        session.snapshot().revision(),
        draft.selected.clone().unwrap(),
        Ok(found),
    );
}
#[test]
fn new_camera_default_is_reported_fullhd_not_the_first_vga_driver_mode_and_commit_uses_exact_identity()
 {
    let session = Session::new();
    let before = session.snapshot();
    let mut draft = fixture(&session, None);
    modes(
        &mut draft,
        &session,
        vec![info(160, 120, 5), info(640, 480, 30), info(1920, 1080, 30)],
    );
    assert_eq!(draft.selected_mode, info(1920, 1080, 30).mode);
    let Intent::CommitCamera { binding, .. } = draft.intent().unwrap() else {
        panic!()
    };
    assert_eq!(binding.mode(), info(1920, 1080, 30).mode.unwrap());
    assert!(std::sync::Arc::ptr_eq(&before, &session.snapshot()));
}
#[test]
fn saved_choice_survives_sort_and_refresh_and_a_missing_mode_requires_explicit_actual_selection() {
    let session = Session::new();
    let target = VideoCaptureTarget::Windows {
        symbolic_link: "camera-reported-fullhd".into(),
    };
    let old = info(640, 480, 30).mode.unwrap();
    let mut draft = fixture(
        &session,
        Some(VideoCaptureBinding::new(target, old, false).unwrap()),
    );
    modes(
        &mut draft,
        &session,
        vec![info(1920, 1080, 30), info(640, 480, 30)],
    );
    assert_eq!(draft.selected_mode, Some(old));
    modes(&mut draft, &session, vec![info(1920, 1080, 30)]);
    assert_eq!(
        draft.selected_mode,
        Some(old),
        "a query must not silently downgrade or replace an existing authored choice"
    );
    assert!(draft.intent().is_err());
    mode_selectors::select_resolution(&mut draft, 1920, 1080);
    assert!(draft.intent().is_ok());
    let selected = draft.selected_mode;
    mode_selectors::select_resolution(&mut draft, 4096, 2160);
    mode_selectors::select_rate(&mut draft, 120, 1);
    assert_eq!(
        draft.selected_mode, selected,
        "unreported resolution/rate cannot be manufactured"
    );
}
#[test]
fn a_foreign_or_stale_query_never_changes_selection_and_sorting_keeps_exact_fps_format() {
    let mut session = Session::new();
    let mut draft = fixture(&session, None);
    let key = CameraRequestKey {
        instance: 1,
        session: session.identity(),
        revision: 0,
        epoch: 0,
        serial: 1,
    };
    let other = VideoCaptureTarget::Windows {
        symbolic_link: "different-camera".into(),
    };
    draft.modes(
        Some((17, key)),
        key,
        session.identity(),
        0,
        other,
        Ok(vec![info(1920, 1080, 30)]),
    );
    assert!(draft.modes.is_empty() && draft.selected_mode.is_none());
    session
        .apply(skylive_core::Command::RenameCollection {
            name: "Changed".into(),
        })
        .unwrap();
    let target = draft.selected.clone().unwrap();
    draft.modes(
        Some((17, key)),
        key,
        session.identity(),
        session.snapshot().revision(),
        target,
        Ok(vec![info(1920, 1080, 30)]),
    );
    assert!(draft.modes.is_empty() && draft.selected_mode.is_none());
    assert!(draft.error.is_some());
}
#[test]
fn real_fullhd_shortcut_changes_only_draft_and_reports_the_actual_slow_frame_rate() {
    let session = Session::new();
    let ctx = egui::Context::default();
    crate::theme::install(&ctx);
    crate::i18n::install(
        &ctx,
        crate::i18n::english().unwrap(),
        std::path::Path::new("metadata-only-camera.csv"),
    );
    let mut draft = fixture(&session, None);
    modes(
        &mut draft,
        &session,
        vec![info(640, 480, 30), info(1920, 1080, 5)],
    );
    assert_eq!(draft.selected_mode, info(640, 480, 30).mode);
    fn point(shape: &egui::epaint::Shape, text: &str) -> Option<egui::Pos2> {
        match shape {
            egui::epaint::Shape::Text(t) if t.galley.text() == text => {
                Some(t.pos + t.galley.size() * 0.5)
            }
            egui::epaint::Shape::Vec(v) => v.iter().find_map(|s| point(s, text)),
            _ => None,
        }
    }
    {
        let mut render = |events| {
            ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(600.0, 900.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default()
                        .show(ctx, |ui| mode_selectors::show(ui, &mut draft));
                },
            )
        };
        let _ = render(vec![]);
        let output = render(vec![]);
        let p = output
            .shapes
            .iter()
            .find_map(|s| point(&s.shape, "Use Full HD").filter(|p| s.clip_rect.contains(*p)))
            .unwrap();
        for pressed in [true, false] {
            let _ = render(vec![
                egui::Event::PointerMoved(p),
                egui::Event::PointerButton {
                    pos: p,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ]);
        }
    }
    assert_eq!(draft.selected_mode, info(1920, 1080, 5).mode);
    assert_eq!(draft.selected_mode.unwrap().fps_num(), 5);
    assert!(draft.intent().is_ok());
    assert_eq!(
        session.snapshot().sources().len(),
        0,
        "mode shortcut does not create a source or start a camera"
    );
}

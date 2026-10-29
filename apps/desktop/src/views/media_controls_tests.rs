//! Real headless toolbar pointer events backed by a local CPU file owner.
use super::*;
use skylive_core::{Command, MediaSource, Session};
use skylive_host::{
    localization::Localization,
    media_source::MediaOwner,
    transition::{FrameComposition, FrameScene},
};
use skylive_media::MediaTime;
use std::{
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};
fn ready() -> (Session, Id, MediaOwner) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../crates/host/tests/fixtures/media/no_pns/cavlc.mp4")
        .canonicalize()
        .unwrap();
    let media = MediaSource::new(path.to_str().unwrap().into(), 64, 48, true)
        .unwrap()
        .with_playback(true, true, false, true);
    let mut session = Session::new();
    let scene = session.preview();
    session
        .apply(Command::AddMediaSource {
            scene,
            name: "Local test fixture".into(),
            media: media.clone(),
        })
        .unwrap();
    let source = session.snapshot().sources()[0].id();
    let mut owner = MediaOwner::new().unwrap();
    owner
        .synchronize(
            &session,
            &FrameComposition::Scene(FrameScene::Live(scene)),
            MediaTime(0),
            None,
        )
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        owner.poll(&session).unwrap();
        if !owner.is_busy()
            && owner
                .snapshot()
                .transport_status(session.identity(), source, &media)
                .is_some()
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "local decoder did not publish source metadata"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    (session, source, owner)
}
#[test]
fn five_locale_selected_media_controls_emit_exact_targets_and_disable_while_pending() {
    let (session, source, mut owner) = ready();
    let before = session.snapshot();
    for language in ["en-US", "es-ES", "ru-RU", "uk-UA", "de-DE"] {
        for size in [egui::vec2(130.0, 180.0), egui::vec2(220.0, 180.0)] {
            let ctx = egui::Context::default();
            crate::theme::install(&ctx);
            crate::i18n::install(
                &ctx,
                Arc::new(Localization::embedded(language).unwrap()),
                Path::new("headless-media-controls.csv"),
            );
            let snapshot = owner.snapshot();
            let draw = |events: Vec<egui::Event>, busy| {
                let mut intents = Vec::new();
                let output = ctx.run(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
                        events,
                        ..Default::default()
                    },
                    |ctx| {
                        egui::CentralPanel::default().show(ctx, |ui| {
                            show(
                                ui,
                                session.identity(),
                                &before,
                                source,
                                MediaTransportView {
                                    snapshot: &snapshot,
                                    busy,
                                },
                                &mut intents,
                            )
                        });
                    },
                );
                (output, intents)
            };
            draw(vec![], false);
            draw(vec![], false);
            for (label, expected) in [
                ("Pause", MediaControl::Pause),
                ("Restart", MediaControl::Restart),
                ("Stop", MediaControl::Stop),
            ] {
                let rect = ctx
                    .data(|d| d.get_temp::<egui::Rect>(egui::Id::new(("media-transport", label))))
                    .unwrap();
                assert!(egui::Rect::from_min_size(egui::Pos2::ZERO, size).contains_rect(rect));
                let mut emitted = Vec::new();
                for pressed in [true, false] {
                    emitted.extend(
                        draw(
                            vec![
                                egui::Event::PointerMoved(rect.center()),
                                egui::Event::PointerButton {
                                    pos: rect.center(),
                                    button: egui::PointerButton::Primary,
                                    pressed,
                                    modifiers: egui::Modifiers::NONE,
                                },
                            ],
                            false,
                        )
                        .1,
                    );
                }
                assert_eq!(emitted.len(), 1);
                let super::super::Intent::ControlMedia { target, operation } =
                    emitted.pop().unwrap()
                else {
                    panic!("real transport intent")
                };
                assert_eq!(operation, expected);
                assert_eq!(target.source(), source);
                assert!(target.matches(&session));
                for pressed in [true, false] {
                    assert!(
                        draw(
                            vec![
                                egui::Event::PointerMoved(rect.center()),
                                egui::Event::PointerButton {
                                    pos: rect.center(),
                                    button: egui::PointerButton::Primary,
                                    pressed,
                                    modifiers: egui::Modifiers::NONE
                                }
                            ],
                            true
                        )
                        .1
                        .is_empty()
                    );
                }
                draw(vec![], false);
            }
            assert!(Arc::ptr_eq(&before, &session.snapshot()));
        }
    }
    // View snapshots own pixels. Let all readers retire before the test-only join.
    drop(before);
    owner.join().unwrap();
}
#[test]
fn public_control_target_rejects_foreign_session_and_a_rebound_file_before_queueing() {
    let (mut session, source, mut owner) = ready();
    let target = owner.control_target(&session, source).unwrap();
    let foreign = Session::new();
    assert!(
        owner
            .control(&foreign, target.clone(), MediaControl::Stop, MediaTime(0))
            .is_err()
    );
    assert!(!owner.is_busy());
    let old = session.snapshot();
    let SourceKind::MediaSource { media } = old.source(source).unwrap().kind() else {
        panic!()
    };
    let replacement = MediaSource::new(
        format!("{}-changed", media.path()),
        media.width(),
        media.height(),
        media.has_audio(),
    )
    .unwrap();
    session
        .apply(Command::RebindMediaSource {
            source,
            media: replacement,
        })
        .unwrap();
    let changed = session.snapshot();
    assert!(
        owner
            .control(&session, target, MediaControl::Pause, MediaTime(0))
            .is_err()
    );
    assert!(!owner.is_busy());
    assert!(Arc::ptr_eq(&changed, &session.snapshot()));
    drop(old);
    drop(changed);
    owner.join().unwrap();
}
#[test]
fn explicit_clock_reset_invalidates_control_receipts_even_without_a_canonical_revision_change() {
    let (session, source, mut owner) = ready();
    let model = session.snapshot();
    let target = owner.control_target(&session, source).unwrap();
    assert!(target.matches(&session) && owner.target_is_current(&target));
    owner.reset_timing(&session).unwrap();
    assert!(
        target.matches(&session),
        "canonical source identity is unchanged"
    );
    assert!(!owner.target_is_current(&target));
    assert!(
        owner
            .control(&session, target, MediaControl::Stop, MediaTime(0))
            .is_err()
    );
    assert!(!owner.is_busy());
    assert!(Arc::ptr_eq(&model, &session.snapshot()));
    drop(model);
    owner.join().unwrap();
}

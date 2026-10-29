//! Actual Source Actions pointer routing; no viewport/device/backend execution.
use super::*;
use skylive_core::{
    AudioCaptureBinding, AudioFilter, AudioTarget, Session, VideoEffect, VideoFilter,
};
use std::sync::Arc;
fn point(shape: &egui::epaint::Shape, label: &str) -> Option<egui::Pos2> {
    match shape {
        egui::epaint::Shape::Text(text) if text.galley.text() == label => {
            Some(text.pos + text.galley.size() * 0.5)
        }
        egui::epaint::Shape::Vec(shapes) => shapes.iter().find_map(|s| point(s, label)),
        _ => None,
    }
}
#[test]
fn source_actions_use_real_obs_provider_and_keep_nonmatching_audio_video_entries_absent() {
    for audio in [true, false] {
        for retained in [false, true] {
            let mut session = Session::new();
            let scene = session.preview();
            if audio {
                session
                    .apply(Command::AddAudioCaptureSource {
                        scene,
                        name: "Audio only".into(),
                        binding: AudioCaptureBinding::new(
                            AudioFlow::Input,
                            AudioTarget::Default,
                            false,
                            false,
                        )
                        .unwrap(),
                    })
                    .unwrap();
            } else {
                session
                    .apply(Command::AddColorSource {
                        scene,
                        name: "Video only".into(),
                        rgba: [10, 20, 30, 255],
                    })
                    .unwrap();
            }
            let source = session.snapshot().sources()[0].id();
            let item = session.snapshot().scene(scene).unwrap().items()[0].id();
            if retained {
                session
                    .apply(if audio {
                        Command::SetSourceAudioFilters {
                            source,
                            filters: vec![AudioFilter::ObsGain {
                                db: 3.0,
                                enabled: true,
                            }],
                        }
                    } else {
                        Command::SetSourceVideoFilters {
                            source,
                            filters: vec![
                                VideoFilter::new(VideoEffect::color_correction()).unwrap(),
                            ],
                        }
                    })
                    .unwrap();
            }
            for plugin in [
                skylive_core::CRAFT_AUDIO_PLUGIN,
                skylive_core::CRAFT_VIDEO_PLUGIN,
                skylive_core::AUDIO_PAN_PLUGIN,
            ] {
                session
                    .apply(Command::SetPluginEnabled {
                        plugin: plugin.into(),
                        enabled: false,
                    })
                    .unwrap();
            }
            let before = session.snapshot();
            assert!(
                before
                    .preferences()
                    .plugin_enabled(skylive_core::OBS_VIDEO_FILTERS_PLUGIN)
            );
            assert!(
                session
                    .apply(Command::SetPluginEnabled {
                        plugin: skylive_core::OBS_VIDEO_FILTERS_PLUGIN.into(),
                        enabled: false
                    })
                    .is_err()
            );
            assert!(Arc::ptr_eq(&before, &session.snapshot()));
            let context = egui::Context::default();
            crate::theme::install(&context);
            let mut intents = Vec::new();
            let mut draw = |events| {
                context.run(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(500.0, 600.0),
                        )),
                        events,
                        ..Default::default()
                    },
                    |ctx| {
                        egui::CentralPanel::default().show(ctx, |ui| {
                            source_actions(
                                ui,
                                &before,
                                scene,
                                Some(item),
                                session.identity(),
                                &mut intents,
                            )
                        });
                    },
                )
            };
            let _ = draw(vec![]);
            let out = draw(vec![]);
            let label = if audio { "Audio Filters" } else { "Filters" };
            let absent = if audio { "Filters" } else { "Audio Filters" };
            assert!(
                out.shapes.iter().all(|s| point(&s.shape, absent).is_none()),
                "nonmatching source provider must not be offered"
            );
            let pos = out
                .shapes
                .iter()
                .find_map(|s| point(&s.shape, label).filter(|p| s.clip_rect.contains(*p)))
                .expect("matching builtin filter action is painted");
            for pressed in [true, false] {
                let _ = draw(vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ]);
            }
            assert_eq!(
                intents.len(),
                1,
                "{label} audio={audio} retained={retained}"
            );
            assert!(if audio {
                matches!(
                    intents.as_slice(),
                    [Intent::OpenDialog(Dialog::AudioFilters { .. })]
                )
            } else {
                matches!(
                    intents.as_slice(),
                    [Intent::OpenDialog(Dialog::VideoFilters(_))]
                )
            });
            assert!(Arc::ptr_eq(&before, &session.snapshot()));
        }
    }
}

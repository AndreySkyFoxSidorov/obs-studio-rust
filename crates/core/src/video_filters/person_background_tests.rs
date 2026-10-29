use super::*;
use crate::{Command, Session};
#[test]
fn person_new_and_reset_default_to_a_bounded_camera_refresh_without_migrating_old_filters() {
    let mut filter = VideoFilter::new(VideoEffect::person_background_removal()).unwrap();
    assert_eq!(filter.person_interval_ms(), PERSON_DEFAULT_INTERVAL_MS);
    let mut saved = serde_json::to_value(&filter).unwrap();
    assert_eq!(saved["person_interval_ms"], PERSON_DEFAULT_INTERVAL_MS);
    saved.as_object_mut().unwrap().remove("person_interval_ms");
    let old: VideoFilter = serde_json::from_value(saved).unwrap();
    assert_eq!(old.person_interval_ms(), 0);
    filter.set_person_interval_ms(0).unwrap();
    let saved = serde_json::to_value(&filter).unwrap();
    let reopened: VideoFilter = serde_json::from_value(saved).unwrap();
    assert_eq!(reopened.person_interval_ms(), 0);
    filter.reset_settings();
    assert_eq!(filter.person_interval_ms(), PERSON_DEFAULT_INTERVAL_MS);
}
#[test]
fn person_defaults_ranges_and_provider_are_explicit() {
    let effect = VideoEffect::person_background_removal();
    assert_eq!(effect.name(), "AI Background Removal (Person)");
    assert_eq!(effect.plugin_id(), PERSON_BACKGROUND_PLUGIN);
    assert!(effect.validate().is_ok());
    assert!(matches!(
        effect,
        VideoEffect::PersonBackgroundRemoval {
            threshold: 0.5,
            softness: 1.0
        }
    ));
    for probability in [0.0_f32, 0.0001, 0.12, 0.5, 0.81, 1.0] {
        let alpha = ((probability - 0.5) / 1.0 + 0.5).clamp(0.0, 1.0);
        assert!((alpha - probability).abs() <= f32::EPSILON);
    }
    for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -0.0001, 1.0001] {
        assert!(
            VideoEffect::PersonBackgroundRemoval {
                threshold: value,
                softness: 1.0
            }
            .validate()
            .is_err()
        );
    }
    for value in [
        f32::NAN,
        f32::INFINITY,
        f32::NEG_INFINITY,
        0.0,
        0.00099,
        1.0001,
    ] {
        assert!(
            VideoEffect::PersonBackgroundRemoval {
                threshold: 0.5,
                softness: value
            }
            .validate()
            .is_err()
        );
    }
    for (threshold, softness) in [(0.0, 0.001), (1.0, 1.0)] {
        assert!(
            VideoFilter::new(VideoEffect::PersonBackgroundRemoval {
                threshold,
                softness
            })
            .is_ok()
        );
    }
}
#[test]
fn default_off_requires_explicit_activation_without_inserting_and_retains_disabled_settings() {
    let mut owner = Session::new();
    let scene = owner.preview();
    owner
        .apply(Command::AddColorSource {
            scene,
            name: "Person camera stand-in".into(),
            rgba: [255; 4],
        })
        .unwrap();
    let source = owner.snapshot().sources()[0].id();
    let mut filter = VideoFilter::new(VideoEffect::person_background_removal()).unwrap();
    assert!(
        !owner
            .snapshot()
            .preferences()
            .plugin_enabled(PERSON_BACKGROUND_PLUGIN)
    );
    for bypassed in [false, true] {
        filter.enabled = !bypassed;
        let before = owner.snapshot();
        assert!(matches!(
            owner.apply(Command::SetSourceVideoFilters {
                source,
                filters: vec![filter.clone()]
            }),
            Err(Error::Unavailable(_))
        ));
        assert!(std::sync::Arc::ptr_eq(&before, &owner.snapshot()));
        assert!(
            owner
                .snapshot()
                .source(source)
                .unwrap()
                .video_filters()
                .is_empty()
        );
    }
    owner
        .apply(Command::SetPluginEnabled {
            plugin: PERSON_BACKGROUND_PLUGIN.into(),
            enabled: true,
        })
        .unwrap();
    assert!(
        owner
            .snapshot()
            .source(source)
            .unwrap()
            .video_filters()
            .is_empty()
    );
    owner
        .apply(Command::SetSourceVideoFilters {
            source,
            filters: vec![filter],
        })
        .unwrap();
    let authored = owner
        .snapshot()
        .source(source)
        .unwrap()
        .video_filters()
        .to_vec();
    owner
        .apply(Command::SetPluginEnabled {
            plugin: PERSON_BACKGROUND_PLUGIN.into(),
            enabled: false,
        })
        .unwrap();
    assert_eq!(
        owner.snapshot().source(source).unwrap().video_filters(),
        authored
    );
    assert!(
        !owner
            .apply(Command::SetSourceVideoFilters {
                source,
                filters: authored.clone()
            })
            .unwrap()
            .changed
    );
    let mut implicit_name = authored.clone();
    implicit_name[0].name = None;
    assert!(
        !owner
            .apply(Command::SetSourceVideoFilters {
                source,
                filters: implicit_name
            })
            .unwrap()
            .changed
    );
    let mut changed = authored.clone();
    changed[0].effect = VideoEffect::PersonBackgroundRemoval {
        threshold: 0.7,
        softness: 0.2,
    };
    let before = owner.snapshot();
    assert!(
        owner
            .apply(Command::SetSourceVideoFilters {
                source,
                filters: changed
            })
            .is_err()
    );
    assert!(std::sync::Arc::ptr_eq(&before, &owner.snapshot()));
    owner
        .apply(Command::SetSourceVideoFilters {
            source,
            filters: vec![],
        })
        .unwrap();
    owner.apply(Command::Undo).unwrap();
    assert_eq!(
        owner.snapshot().source(source).unwrap().video_filters(),
        authored
    );
    assert!(
        !owner
            .snapshot()
            .preferences()
            .plugin_enabled(PERSON_BACKGROUND_PLUGIN)
    );
}
#[test]
fn scene_filters_require_activation_before_lazy_owner_and_existing_craft_gates_match() {
    let mut owner = Session::new();
    let scene = owner.preview();
    let before = owner.snapshot();
    assert!(before.scene_source(scene).is_none());
    assert!(
        owner
            .apply(Command::SetSceneVideoFilters {
                scene,
                filters: vec![VideoFilter::new(VideoEffect::person_background_removal()).unwrap()]
            })
            .is_err()
    );
    assert!(std::sync::Arc::ptr_eq(&before, &owner.snapshot()));
    assert!(owner.snapshot().scene_source(scene).is_none());
    owner
        .apply(Command::SetPluginEnabled {
            plugin: PERSON_BACKGROUND_PLUGIN.into(),
            enabled: true,
        })
        .unwrap();
    owner
        .apply(Command::SetSceneVideoFilters {
            scene,
            filters: vec![VideoFilter::new(VideoEffect::person_background_removal()).unwrap()],
        })
        .unwrap();
    assert_eq!(
        owner
            .snapshot()
            .scene_source(scene)
            .unwrap()
            .video_filters()
            .len(),
        1
    );
    owner
        .apply(Command::SetPluginEnabled {
            plugin: CRAFT_VIDEO_PLUGIN.into(),
            enabled: false,
        })
        .unwrap();
    let before = owner.snapshot();
    assert!(
        owner
            .apply(Command::SetSceneVideoFilters {
                scene,
                filters: vec![VideoFilter::new(VideoEffect::color_key()).unwrap()]
            })
            .is_err()
    );
    assert!(std::sync::Arc::ptr_eq(&before, &owner.snapshot()));
}

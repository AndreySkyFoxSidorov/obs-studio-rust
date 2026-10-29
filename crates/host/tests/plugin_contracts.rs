//! Shared application/plugin persistence contracts; no device enumeration.
use skylive_core::{
    AUDIO_PAN_PLUGIN, AudioBuffering, AudioCaptureBinding, AudioFilter, AudioFlow, AudioSettings,
    AudioTarget, Command, Error, MAX_AUDIO_FILTERS, MAX_SCENE_NOTES_BYTES, SCENE_NOTES_PLUGIN,
    Session, SourceKind,
};
use skylive_host::{obs, storage};
use std::sync::Arc;

fn toggle(owner: &mut Session, id: &str, enabled: bool) {
    owner
        .apply(Command::SetPluginEnabled {
            plugin: id.into(),
            enabled,
        })
        .unwrap();
}
fn audio_source(owner: &mut Session) -> skylive_core::Id {
    owner
        .apply(Command::AddAudioCaptureSource {
            scene: owner.preview(),
            name: "Offline microphone".into(),
            binding: AudioCaptureBinding::new(AudioFlow::Input, AudioTarget::Default, false, false)
                .unwrap(),
        })
        .unwrap();
    owner
        .snapshot()
        .sources()
        .iter()
        .find(|s| matches!(s.kind(), SourceKind::AudioCapture { .. }))
        .unwrap()
        .id()
}
#[test]
fn providers_default_off_unknown_unported_and_conflicting_activation_preserve_owner() {
    let mut owner = Session::new();
    let before = owner.snapshot();
    assert!(before.preferences().enabled_plugins().is_empty());
    for plugin in [
        "missing/plugin",
        "norihiro/obs-mute-filter",
        "obsproject/obs-browser",
    ] {
        assert!(
            owner
                .apply(Command::SetPluginEnabled {
                    plugin: plugin.into(),
                    enabled: true
                })
                .is_err()
        );
        assert!(Arc::ptr_eq(&before, &owner.snapshot()));
    }
    assert!(matches!(
        owner.apply_at(
            Some(99),
            Command::SetPluginEnabled {
                plugin: AUDIO_PAN_PLUGIN.into(),
                enabled: true
            }
        ),
        Err(Error::Conflict { .. })
    ));
    assert!(Arc::ptr_eq(&before, &owner.snapshot()));
    toggle(&mut owner, AUDIO_PAN_PLUGIN, true);
    let enabled = owner.snapshot();
    assert_eq!(enabled.revision(), before.revision() + 1);
    assert!(!owner.can_undo());
    assert!(
        !owner
            .apply(Command::SetPluginEnabled {
                plugin: AUDIO_PAN_PLUGIN.into(),
                enabled: true
            })
            .unwrap()
            .changed
    );
    assert!(Arc::ptr_eq(&enabled, &owner.snapshot()));
    owner
        .apply(Command::SetAudioSettings {
            settings: AudioSettings::default()
                .with_buffering(AudioBuffering::low_latency())
                .unwrap(),
        })
        .unwrap();
    assert!(
        owner
            .snapshot()
            .preferences()
            .plugin_enabled(AUDIO_PAN_PLUGIN)
    );
}
#[test]
fn filter_drafts_require_activation_but_disable_retains_settings_and_scene_undo_cannot_enable() {
    let mut owner = Session::new();
    let source = audio_source(&mut owner);
    let filter = AudioFilter::default();
    let before = owner.snapshot();
    assert!(matches!(
        owner.apply(Command::SetSourceAudioFilters {
            source,
            filters: vec![filter]
        }),
        Err(Error::Unavailable(_))
    ));
    assert!(Arc::ptr_eq(&before, &owner.snapshot()));
    toggle(&mut owner, AUDIO_PAN_PLUGIN, true);
    owner
        .apply(Command::SetSourceAudioFilters {
            source,
            filters: vec![filter],
        })
        .unwrap();
    toggle(&mut owner, AUDIO_PAN_PLUGIN, false);
    assert_eq!(
        owner.snapshot().source(source).unwrap().audio_filters(),
        &[filter]
    );
    owner.apply(Command::Undo).unwrap();
    assert!(
        owner
            .snapshot()
            .source(source)
            .unwrap()
            .audio_filters()
            .is_empty()
    );
    assert!(
        !owner
            .snapshot()
            .preferences()
            .plugin_enabled(AUDIO_PAN_PLUGIN)
    );
    owner.apply(Command::Redo).unwrap();
    assert_eq!(
        owner.snapshot().source(source).unwrap().audio_filters(),
        &[filter]
    );
    assert!(
        !owner
            .snapshot()
            .preferences()
            .plugin_enabled(AUDIO_PAN_PLUGIN)
    );
    assert!(
        !owner
            .apply(Command::SetSourceAudioFilters {
                source,
                filters: vec![filter]
            })
            .unwrap()
            .changed
    );
    assert!(
        owner
            .apply(Command::SetSourceAudioFilters {
                source,
                filters: vec![filter, filter]
            })
            .is_err()
    );
    owner
        .apply(Command::SetSourceAudioFilters {
            source,
            filters: vec![],
        })
        .unwrap();
}
#[test]
fn scene_notes_are_bounded_authored_data_independent_of_global_activation() {
    let mut owner = Session::new();
    let scene = owner.preview();
    let before = owner.snapshot();
    assert!(
        owner
            .apply(Command::SetSceneNotes {
                scene,
                text: "Draft".into()
            })
            .is_err()
    );
    assert!(Arc::ptr_eq(&before, &owner.snapshot()));
    toggle(&mut owner, SCENE_NOTES_PLUGIN, true);
    owner
        .apply(Command::SetSceneNotes {
            scene,
            text: "Scene 1: cue\nПривіт!".into(),
        })
        .unwrap();
    let text = owner.snapshot().scene(scene).unwrap().notes().to_owned();
    let before = owner.snapshot();
    assert!(
        owner
            .apply(Command::SetSceneNotes {
                scene,
                text: "a".repeat(MAX_SCENE_NOTES_BYTES + 1)
            })
            .is_err()
    );
    assert!(Arc::ptr_eq(&before, &owner.snapshot()));
    toggle(&mut owner, SCENE_NOTES_PLUGIN, false);
    assert_eq!(owner.snapshot().scene(scene).unwrap().notes(), text);
    owner.apply(Command::Undo).unwrap();
    assert_eq!(owner.snapshot().scene(scene).unwrap().notes(), "");
    assert!(
        !owner
            .snapshot()
            .preferences()
            .plugin_enabled(SCENE_NOTES_PLUGIN)
    );
    owner.apply(Command::Redo).unwrap();
    assert_eq!(owner.snapshot().scene(scene).unwrap().notes(), text);
    let copied = owner
        .apply(Command::DuplicateScene {
            scene,
            name: "Copy".into(),
        })
        .unwrap()
        .created
        .unwrap();
    assert_eq!(owner.snapshot().scene(copied).unwrap().notes(), text);
}
#[test]
fn native_format_preserves_activation_and_disabled_authored_settings_with_strict_migration() {
    let mut owner = Session::new();
    let source = audio_source(&mut owner);
    toggle(&mut owner, AUDIO_PAN_PLUGIN, true);
    toggle(&mut owner, SCENE_NOTES_PLUGIN, true);
    owner
        .apply(Command::SetSourceAudioFilters {
            source,
            filters: vec![AudioFilter::default()],
        })
        .unwrap();
    owner
        .apply(Command::SetSceneNotes {
            scene: owner.preview(),
            text: "Native notes".into(),
        })
        .unwrap();
    toggle(&mut owner, AUDIO_PAN_PLUGIN, false);
    let bytes = storage::encode(&owner.snapshot()).unwrap();
    let loaded = storage::decode(&bytes, true).unwrap();
    assert_eq!(*owner.snapshot(), *loaded.snapshot());
    assert_ne!(owner.identity(), loaded.identity());
    let mut value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    value["preferences"]
        .as_object_mut()
        .unwrap()
        .remove("enabled_plugins");
    let migrated = storage::decode(&serde_json::to_vec(&value).unwrap(), true).unwrap();
    assert!(
        migrated
            .snapshot()
            .preferences()
            .enabled_plugins()
            .is_empty()
    );
    assert_eq!(
        migrated
            .snapshot()
            .scene(migrated.preview())
            .unwrap()
            .notes(),
        "Native notes"
    );
    value["preferences"]["enabled_plugins"] = serde_json::json!(["not/ported"]);
    assert!(storage::decode(&serde_json::to_vec(&value).unwrap(), true).is_err());
    value["preferences"]["enabled_plugins"] = serde_json::json!([AUDIO_PAN_PLUGIN]);
    value["collection"]["sources"][0]["audio_filters"][0]["pan_left"] = serde_json::json!(101);
    assert!(storage::decode(&serde_json::to_vec(&value).unwrap(), true).is_err());
    assert!(
        obs::export_collection(&owner, None).is_err(),
        "unsupported plugin data cannot disappear on OBS export"
    );
}
#[test]
fn filter_limits_nonfinite_and_non_audio_sources_reject_before_publication() {
    let mut owner = Session::new();
    let source = audio_source(&mut owner);
    toggle(&mut owner, AUDIO_PAN_PLUGIN, true);
    let before = owner.snapshot();
    assert!(
        owner
            .apply(Command::SetSourceAudioFilters {
                source,
                filters: vec![AudioFilter::default(); MAX_AUDIO_FILTERS + 1]
            })
            .is_err()
    );
    assert!(Arc::ptr_eq(&before, &owner.snapshot()));
    let bad = AudioFilter::Pan {
        pan_left: f32::NAN,
        pan_right: 100.0,
        gain_left_db: 0.0,
        gain_right_db: 0.0,
        invert_left: false,
        invert_right: false,
    };
    assert!(
        owner
            .apply(Command::SetSourceAudioFilters {
                source,
                filters: vec![bad]
            })
            .is_err()
    );
    assert!(Arc::ptr_eq(&before, &owner.snapshot()));
    owner
        .apply(Command::AddColorSource {
            scene: owner.preview(),
            name: "Color".into(),
            rgba: [255; 4],
        })
        .unwrap();
    let color = owner
        .snapshot()
        .sources()
        .iter()
        .find(|s| matches!(s.kind(), SourceKind::Color { .. }))
        .unwrap()
        .id();
    assert!(
        owner
            .apply(Command::SetSourceAudioFilters {
                source: color,
                filters: vec![AudioFilter::default()]
            })
            .is_err()
    );
}

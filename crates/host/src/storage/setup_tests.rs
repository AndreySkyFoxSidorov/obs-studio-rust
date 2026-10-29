use super::*;
use skylive_core::{Command, SetupCommand, SetupState};

#[test]
fn setup_minimum9_writer_roundtrip_preserves_older_default_files_and_rejects_backdating() {
    let mut session = Session::new();
    let bytes = encode(&session.snapshot()).unwrap();
    for schema in 2..=8 {
        let mut value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        value["schema"] = schema.into();
        let legacy = decode(&serde_json::to_vec(&value).unwrap(), true).unwrap();
        assert_eq!(
            legacy.snapshot().preferences().setup_state(),
            SetupState::Unseen
        );
        assert!(legacy.snapshot().preferences().setup_on_new_profile());
    }
    session
        .apply(Command::Setup {
            command: SetupCommand::Skip,
        })
        .unwrap();
    let original: serde_json::Value =
        serde_json::from_slice(&encode(&session.snapshot()).unwrap()).unwrap();
    assert_eq!(original["schema"], CURRENT_SCHEMA);
    for (state, expected) in [
        ("skipped", SetupState::Skipped),
        ("completed", SetupState::Completed),
    ] {
        let mut value = original.clone();
        value["preferences"]["setup_state"] = state.into();
        let current = decode(&serde_json::to_vec(&value).unwrap(), true).unwrap();
        assert_eq!(current.snapshot().preferences().setup_state(), expected);
        let mut minimum = value.clone();
        minimum["schema"] = 9.into();
        assert_eq!(
            decode(&serde_json::to_vec(&minimum).unwrap(), true)
                .unwrap()
                .snapshot()
                .preferences()
                .setup_state(),
            expected
        );
        for schema in 1..=8 {
            value["schema"] = schema.into();
            let error = decode(&serde_json::to_vec(&value).unwrap(), true)
                .err()
                .unwrap();
            assert_eq!(error.phase, Phase::BeforePublication);
            // Schema1 cannot carry modern preferences at all: its strict legacy
            // envelope rejects them before snapshot/feature decoding.
            if schema >= 2 {
                assert!(
                    error
                        .to_string()
                        .contains("setup preferences require native schema9"),
                    "schema{schema}/{state}: {error}"
                );
            }
        }
    }
}

#[test]
fn schemas1_through8_cannot_backdate_new_profile_checkbox_even_when_setup_unseen() {
    let mut session = Session::new();
    session
        .apply(Command::Setup {
            command: SetupCommand::CreateProfile {
                name: "No wizard".into(),
                run_setup: false,
            },
        })
        .unwrap();
    let mut value: serde_json::Value =
        serde_json::from_slice(&encode(&session.snapshot()).unwrap()).unwrap();
    let current = decode(&serde_json::to_vec(&value).unwrap(), true).unwrap();
    assert_eq!(
        current.snapshot().preferences().setup_state(),
        SetupState::Unseen
    );
    assert!(!current.snapshot().preferences().setup_on_new_profile());
    for schema in 1..=8 {
        value["schema"] = schema.into();
        assert!(
            decode(&serde_json::to_vec(&value).unwrap(), true).is_err(),
            "new Setup checkbox must reject claimed native{schema}"
        );
    }
}

#[test]
fn setup_minimum9_and_person_filter_minimum8_remain_independent() {
    use skylive_core::{PERSON_BACKGROUND_PLUGIN, VideoEffect, VideoFilter};
    let mut session = Session::new();
    session
        .apply(Command::SetPluginEnabled {
            plugin: PERSON_BACKGROUND_PLUGIN.into(),
            enabled: true,
        })
        .unwrap();
    let scene = session.preview();
    let mut filter = VideoFilter::new(VideoEffect::person_background_removal()).unwrap();
    filter.enabled = false;
    session
        .apply(Command::SetSceneVideoFilters {
            scene,
            filters: vec![filter],
        })
        .unwrap();
    let ordinary8 = session.snapshot();
    let mut value: serde_json::Value =
        serde_json::from_slice(&encode(&ordinary8).unwrap()).unwrap();
    assert_eq!(value["schema"], CURRENT_SCHEMA);
    value["schema"] = 8.into();
    let legacy = decode_with_report(&serde_json::to_vec(&value).unwrap(), true).unwrap();
    assert_eq!(legacy.session.snapshot(), ordinary8);
    assert!(!legacy.migration.has_semantic_changes());
    assert!(!legacy.session.is_document_dirty());
    value["schema"] = 7.into();
    let error = decode(&serde_json::to_vec(&value).unwrap(), true)
        .err()
        .unwrap();
    assert!(
        error.to_string().contains("requires native schema8"),
        "{error}"
    );
    session
        .apply(Command::Setup {
            command: SetupCommand::Skip,
        })
        .unwrap();
    let before = session.snapshot();
    let bytes = encode(&before).unwrap();
    let mut value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    value["schema"] = 8.into();
    let error = decode(&serde_json::to_vec(&value).unwrap(), true)
        .err()
        .unwrap();
    assert!(
        error
            .to_string()
            .contains("setup preferences require native schema9"),
        "{error}"
    );
    let loaded = decode(&bytes, true).unwrap();
    assert_eq!(
        loaded.snapshot().preferences().setup_state(),
        SetupState::Skipped
    );
    assert_eq!(
        loaded
            .snapshot()
            .scene_source(scene)
            .unwrap()
            .video_filters(),
        before.scene_source(scene).unwrap().video_filters()
    );
    assert!(std::sync::Arc::ptr_eq(&before, &session.snapshot()));
}

#[test]
fn setup_wire_validation_and_legacy_preference_defaults_belong_to_storage_boundary() {
    let session = Session::new();
    let snapshot = session.snapshot();
    let profile = snapshot.profiles().selected();
    let configuration = skylive_core::SetupConfiguration::new(
        profile.id(),
        snapshot.video().clone(),
        profile.recording().clone(),
        profile.streaming().clone(),
    )
    .unwrap();
    let mut wire = serde_json::to_value(Command::Setup {
        command: SetupCommand::Apply {
            configuration: configuration.clone(),
        },
    })
    .unwrap();
    for bad in [0, 1, 8193] {
        wire["command"]["configuration"]["video"]["width"] = bad.into();
        assert!(serde_json::from_value::<Command>(wire.clone()).is_err());
    }
    let legacy: skylive_core::UserPreferences = serde_json::from_str(
        r#"{"audio_buffering":{"max_ms":0,"fixed":false},"enabled_plugins":[],"language":"en-US"}"#,
    )
    .unwrap();
    assert!(legacy.setup_on_new_profile());
    assert_eq!(legacy.setup_state(), SetupState::Unseen);
}

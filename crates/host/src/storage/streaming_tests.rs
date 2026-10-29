//! Source-only until copied to host storage/streaming_tests.rs after unfreeze.
use super::*;
use skylive_core::{Command, ProfileCommand, StreamingSettings, VideoEffect, VideoFilter};
fn configured() -> StreamingSettings {
    StreamingSettings::new(
        "rtmp://[::1]:1935/live/raw?auth=a%2Fb".into(),
        "  public/Ключ?token=x%2By  ".into(),
        8000,
        2,
        true,
        128,
    )
    .unwrap()
}
#[test]
fn schema5_streaming_values_roundtrip_exactly_and_old4_default_is_additive() {
    let mut session = Session::new();
    let profile = session.snapshot().profiles().selected_id();
    let settings = configured();
    session
        .apply(Command::Profile {
            command: ProfileCommand::SetStreaming {
                id: profile,
                settings: settings.clone(),
            },
        })
        .unwrap();
    let bytes = encode(&session.snapshot()).unwrap();
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["schema"], CURRENT_SCHEMA);
    assert_eq!(
        value["profiles"]["profiles"][0]["streaming"]["stream_key"],
        settings.stream_key()
    );
    let loaded = decode(&bytes, true).unwrap();
    assert_eq!(
        loaded.snapshot().profiles().selected().streaming(),
        &settings
    );
    let mut old =
        serde_json::to_value(serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()).unwrap();
    old["schema"] = serde_json::json!(4);
    old["profiles"]["profiles"][0]
        .as_object_mut()
        .unwrap()
        .remove("streaming");
    let loaded = decode(&serde_json::to_vec(&old).unwrap(), true).unwrap();
    assert!(
        loaded
            .snapshot()
            .profiles()
            .selected()
            .streaming()
            .is_default()
    );
    old["profiles"]["profiles"][0]["streaming"] = serde_json::to_value(&settings).unwrap();
    assert!(
        decode(&serde_json::to_vec(&old).unwrap(), true).is_err(),
        "schema4 must not accept later authored fields"
    );
}
#[test]
fn schema4_scene_filter_ids_parameters_order_and_saved_state_are_not_remigrated_by5() {
    let mut session = Session::new();
    let scene = session.preview();
    session
        .apply(Command::SetSceneVideoFilters {
            scene,
            filters: vec![
                VideoFilter::new(VideoEffect::color_correction())
                    .unwrap()
                    .with_name("Retained scene filter".into())
                    .unwrap(),
            ],
        })
        .unwrap();
    let snapshot = session.snapshot();
    let mut old: serde_json::Value = serde_json::from_slice(&encode(&snapshot).unwrap()).unwrap();
    old["schema"] = serde_json::json!(4);
    let loaded = decode_with_report(&serde_json::to_vec(&old).unwrap(), true).unwrap();
    assert!(!loaded.migration.has_semantic_changes());
    assert!(!loaded.session.is_document_dirty());
    assert_eq!(
        loaded
            .session
            .snapshot()
            .scene_source(scene)
            .unwrap()
            .video_filters(),
        snapshot.scene_source(scene).unwrap().video_filters()
    );
}
#[test]
fn schema5_null_partial_unknown_and_invalid_profiles_reject_without_candidate_publication() {
    let mut session = Session::new();
    let id = session.snapshot().profiles().selected_id();
    session
        .apply(Command::Profile {
            command: ProfileCommand::SetStreaming {
                id,
                settings: configured(),
            },
        })
        .unwrap();
    let baseline = session.snapshot();
    let value: serde_json::Value = serde_json::from_slice(&encode(&baseline).unwrap()).unwrap();
    for bad in [
        serde_json::Value::Null,
        serde_json::json!({"server_url":"rtmp://127.0.0.1/live"}),
        serde_json::json!({"server_url":"rtmps://127.0.0.1/live","stream_key":"public","video_bitrate_kbps":6000,"keyframe_interval_seconds":2,"audio_enabled":true,"audio_bitrate_kbps":160}),
    ] {
        let mut malformed = value.clone();
        malformed["profiles"]["profiles"][0]["streaming"] = bad;
        assert!(decode(&serde_json::to_vec(&malformed).unwrap(), true).is_err());
        assert!(std::sync::Arc::ptr_eq(&baseline, &session.snapshot()));
    }
    let mut unknown = value;
    unknown["profiles"]["profiles"][0]["streaming"]["service_oauth"] =
        serde_json::json!("not implemented");
    assert!(decode(&serde_json::to_vec(&unknown).unwrap(), true).is_err());
    unknown["schema"] = serde_json::json!(CURRENT_SCHEMA + 1);
    assert!(decode(&serde_json::to_vec(&unknown).unwrap(), true).is_err());
}

#[test]
fn legacy_schema1_through4_default_streaming_without_new_authored_fields() {
    let session = Session::new();
    let current = session.snapshot();
    let template: serde_json::Value = serde_json::from_slice(&encode(&current).unwrap()).unwrap();
    for schema in 2..=4 {
        let mut value = template.clone();
        value["schema"] = serde_json::json!(schema);
        let loaded = decode(&serde_json::to_vec(&value).unwrap(), true).unwrap();
        assert!(
            loaded
                .snapshot()
                .profiles()
                .selected()
                .streaming()
                .is_default()
        );
    }
    let mut legacy = template;
    legacy["schema"] = serde_json::json!(1);
    legacy.as_object_mut().unwrap().remove("profiles");
    legacy.as_object_mut().unwrap().remove("preferences");
    legacy.as_object_mut().unwrap().remove("revision");
    legacy["collection"]["revision"] = serde_json::json!(current.revision());
    legacy["collection"]["video"] = serde_json::to_value(current.video()).unwrap();
    legacy["collection"]["audio"] = serde_json::to_value(current.audio()).unwrap();
    let loaded = decode(&serde_json::to_vec(&legacy).unwrap(), true).unwrap();
    assert!(
        loaded
            .snapshot()
            .profiles()
            .selected()
            .streaming()
            .is_default()
    );
}

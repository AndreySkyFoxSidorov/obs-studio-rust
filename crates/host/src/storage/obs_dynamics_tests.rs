use super::*;
use serde_json::{Value, json};
use skylive_core::{
    AudioCaptureBinding, AudioFilter, AudioFlow, AudioTarget, Command, VideoEffect, VideoFilter,
};
#[test]
fn ordinary_schema5_is_exact_saved_additive_migration_without_scene_warning() {
    let mut session = Session::new();
    let scene = session.preview();
    session
        .apply(Command::SetSceneVideoFilters {
            scene,
            filters: vec![
                VideoFilter::new(VideoEffect::color_correction())
                    .unwrap()
                    .with_name("Existing root filter".into())
                    .unwrap(),
            ],
        })
        .unwrap();
    let snapshot = session.snapshot();
    let mut value: Value = serde_json::from_slice(&encode(&snapshot).unwrap()).unwrap();
    assert_eq!(value["schema"], CURRENT_SCHEMA);
    value["schema"] = json!(5);
    let loaded = decode_with_report(&serde_json::to_vec(&value).unwrap(), true).unwrap();
    assert_eq!(loaded.migration.from_schema, 5);
    assert_eq!(loaded.migration.to_schema, CURRENT_SCHEMA);
    assert!(!loaded.migration.has_semantic_changes());
    assert!(!loaded.session.is_document_dirty());
    assert_eq!(loaded.session.snapshot(), snapshot);
}
#[test]
fn authored_obs_dynamics_require_schema6_even_disabled_and_cannot_be_smuggled_into1through5() {
    let mut session = Session::new();
    let scene = session.preview();
    session
        .apply(Command::AddAudioCaptureSource {
            scene,
            name: "Persisted dynamics".into(),
            binding: AudioCaptureBinding::new(AudioFlow::Input, AudioTarget::Default, false, false)
                .unwrap(),
        })
        .unwrap();
    let source = session.snapshot().sources()[0].id();
    for filter in [
        AudioFilter::ObsLimiter {
            threshold_db: -6.,
            release_ms: 60,
            enabled: false,
        },
        AudioFilter::obs_compressor(),
        AudioFilter::obs_expander(),
    ] {
        session
            .apply(Command::SetSourceAudioFilters {
                source,
                filters: vec![filter],
            })
            .unwrap();
        let snapshot = session.snapshot();
        let current: Value = serde_json::from_slice(&encode(&snapshot).unwrap()).unwrap();
        assert_eq!(current["schema"], CURRENT_SCHEMA);
        let good = decode(&serde_json::to_vec(&current).unwrap(), true).unwrap();
        assert_eq!(
            good.snapshot().source(source).unwrap().audio_filters(),
            [filter]
        );
        for schema in 1..=5 {
            let mut bad = current.clone();
            bad["schema"] = json!(schema);
            if schema == 1 {
                let object = bad.as_object_mut().unwrap();
                object.remove("profiles");
                object.remove("preferences");
                object.remove("revision");
                bad["collection"]["revision"] = json!(snapshot.revision());
                bad["collection"]["video"] = serde_json::to_value(snapshot.video()).unwrap();
                bad["collection"]["audio"] = serde_json::to_value(snapshot.audio()).unwrap();
            }
            let error = decode_with_report(&serde_json::to_vec(&bad).unwrap(), true)
                .err()
                .expect("old schema must reject later filter variants");
            assert_eq!(error.phase, Phase::BeforePublication);
            assert!(
                error
                    .detail
                    .contains("OBS dynamics requires native schema6"),
                "{schema}: {error}"
            );
            assert!(std::sync::Arc::ptr_eq(&snapshot, &session.snapshot()));
        }
    }
}
#[test]
fn schema6_preserves_forward_audio_chain_and_unknown_future_version_rejects() {
    let mut session = Session::new();
    let scene = session.preview();
    session
        .apply(Command::AddAudioCaptureSource {
            scene,
            name: "Forward chain".into(),
            binding: AudioCaptureBinding::new(AudioFlow::Input, AudioTarget::Default, false, false)
                .unwrap(),
        })
        .unwrap();
    let source = session.snapshot().sources()[0].id();
    let chain = vec![
        AudioFilter::ObsGain {
            db: 20.,
            enabled: true,
        },
        AudioFilter::obs_limiter(),
    ];
    session
        .apply(Command::SetSourceAudioFilters {
            source,
            filters: chain.clone(),
        })
        .unwrap();
    let mut value: Value = serde_json::from_slice(&encode(&session.snapshot()).unwrap()).unwrap();
    assert_eq!(
        decode(&serde_json::to_vec(&value).unwrap(), true)
            .unwrap()
            .snapshot()
            .source(source)
            .unwrap()
            .audio_filters(),
        chain
    );
    value["schema"] = json!(CURRENT_SCHEMA + 1);
    assert!(decode(&serde_json::to_vec(&value).unwrap(), true).is_err());
}

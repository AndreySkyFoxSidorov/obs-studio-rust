use super::*;
use serde_json::{Value, json};
use skylive_core::{Command, PERSON_BACKGROUND_PLUGIN, VideoEffect, VideoFilter};

fn fixture(scene_filter: bool, bypassed: bool) -> (Session, Id) {
    let mut owner = Session::new();
    let scene = owner.preview();
    owner
        .apply(Command::SetPluginEnabled {
            plugin: PERSON_BACKGROUND_PLUGIN.into(),
            enabled: true,
        })
        .unwrap();
    let mut filter = VideoFilter::new(VideoEffect::PersonBackgroundRemoval {
        threshold: 0.71,
        softness: 0.24,
    })
    .unwrap();
    // This fixture represents the original schema-8 Person filter, which
    // predates the configurable inference interval introduced in schema 9.
    filter.set_person_interval_ms(0).unwrap();
    filter.enabled = !bypassed;
    if scene_filter {
        owner
            .apply(Command::SetSceneVideoFilters {
                scene,
                filters: vec![filter],
            })
            .unwrap();
        let source = owner.snapshot().scene_source(scene).unwrap().id();
        (owner, source)
    } else {
        owner
            .apply(Command::AddColorSource {
                scene,
                name: "Persisted person filter".into(),
                rgba: [255; 4],
            })
            .unwrap();
        let source = owner.snapshot().sources()[0].id();
        owner
            .apply(Command::SetSourceVideoFilters {
                source,
                filters: vec![filter],
            })
            .unwrap();
        (owner, source)
    }
}
#[test]
fn source_and_scene_person_filters_require8_even_bypassed_and_disabled_provider() {
    for scene_filter in [false, true] {
        for bypassed in [false, true] {
            let (mut owner, source) = fixture(scene_filter, bypassed);
            owner
                .apply(Command::SetPluginEnabled {
                    plugin: PERSON_BACKGROUND_PLUGIN.into(),
                    enabled: false,
                })
                .unwrap();
            let snapshot = owner.snapshot();
            let current: Value = serde_json::from_slice(&encode(&snapshot).unwrap()).unwrap();
            assert_eq!(current["schema"], CURRENT_SCHEMA);
            for schema in 1..=7 {
                let mut old = current.clone();
                old["schema"] = json!(schema);
                if schema == 1 {
                    let object = old.as_object_mut().unwrap();
                    object.remove("profiles");
                    object.remove("preferences");
                    object.remove("revision");
                    old["collection"]["revision"] = json!(snapshot.revision());
                    old["collection"]["video"] = serde_json::to_value(snapshot.video()).unwrap();
                    old["collection"]["audio"] = serde_json::to_value(snapshot.audio()).unwrap();
                }
                let error = decode_with_report(&serde_json::to_vec(&old).unwrap(), true)
                    .err()
                    .expect("older version must reject authored Person effect");
                assert_eq!(error.phase, Phase::BeforePublication);
                assert!(
                    error.detail.contains("requires native schema8"),
                    "{schema}/{scene_filter}/{bypassed}: {error}"
                );
                assert!(std::sync::Arc::ptr_eq(&snapshot, &owner.snapshot()));
            }
            let decoded = decode(&serde_json::to_vec(&current).unwrap(), true).unwrap();
            assert_eq!(
                decoded.snapshot().source(source).unwrap().video_filters(),
                snapshot.source(source).unwrap().video_filters()
            );
            assert!(
                !decoded
                    .snapshot()
                    .preferences()
                    .plugin_enabled(PERSON_BACKGROUND_PLUGIN)
            );
            assert!(!decoded.is_document_dirty());
        }
    }
}
#[test]
fn ordinary7_and8_stay_exact_saved_without_semantic_migration_and_future_versions_reject() {
    let owner = Session::new();
    let snapshot = owner.snapshot();
    let mut value: Value = serde_json::from_slice(&encode(&snapshot).unwrap()).unwrap();
    for schema in [7, 8] {
        value["schema"] = json!(schema);
        let loaded = decode_with_report(&serde_json::to_vec(&value).unwrap(), true).unwrap();
        assert_eq!(loaded.migration.from_schema, schema);
        assert_eq!(loaded.migration.to_schema, CURRENT_SCHEMA);
        assert!(!loaded.migration.has_semantic_changes());
        assert_eq!(loaded.session.snapshot(), snapshot);
        assert!(!loaded.session.is_document_dirty());
    }
    value["schema"] = json!(CURRENT_SCHEMA + 1);
    assert!(decode(&serde_json::to_vec(&value).unwrap(), true).is_err());
}

#[test]
fn person_background_codec_is_strict_at_the_storage_boundary() {
    let filter = VideoFilter::new(VideoEffect::person_background_removal()).unwrap();
    let mut value = serde_json::to_value(&filter).unwrap();
    assert_eq!(value["effect"]["kind"], "person_background_removal");
    value["effect"]["gpu"] = json!(true);
    assert!(serde_json::from_value::<VideoFilter>(value).is_err());
}

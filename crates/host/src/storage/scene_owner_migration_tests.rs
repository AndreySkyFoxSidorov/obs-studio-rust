//! Schema4 changes Scene source ownership, never filter identities or order.
use super::*;
use serde_json::{Value, json};
use skylive_core::{Command, VideoEffect, VideoFilter};
#[test]
fn schema3_semantic_receipt_preserves_graph_ids_order_gamma_and_requires_explicit_save() {
    let mut session = Session::new();
    let root = session.preview();
    let host = session
        .apply(Command::AddScene {
            name: "Host".into(),
        })
        .unwrap()
        .created
        .unwrap();
    session
        .apply(Command::AddSceneSource {
            scene: host,
            target: root,
        })
        .unwrap();
    let mut first = VideoEffect::color_correction();
    if let VideoEffect::ColorCorrection { gamma, .. } = &mut first {
        *gamma = 0.49;
    }
    session
        .apply(Command::SetSceneVideoFilters {
            scene: root,
            filters: vec![
                VideoFilter::new(first)
                    .unwrap()
                    .with_name("Gamma".into())
                    .unwrap(),
                VideoFilter::new(VideoEffect::color_correction())
                    .unwrap()
                    .with_name("Last".into())
                    .unwrap(),
            ],
        })
        .unwrap();
    let before = session.snapshot();
    let owner = before.scene_source(root).unwrap();
    let mut legacy: Value = serde_json::from_slice(&encode(&before).unwrap()).unwrap();
    legacy["schema"] = json!(3);
    let loaded = decode_with_report(&serde_json::to_vec(&legacy).unwrap(), true).unwrap();
    assert!(loaded.session.is_dirty());
    assert_eq!(loaded.session.snapshot().as_ref(), before.as_ref());
    assert_eq!(loaded.session.snapshot().revision(), before.revision());
    assert_eq!(loaded.migration.from_schema, 3);
    assert_eq!(loaded.migration.to_schema, CURRENT_SCHEMA);
    assert_eq!(loaded.migration.scene_source_filters.len(), 1);
    let affected = &loaded.migration.scene_source_filters[0];
    assert_eq!(
        (
            affected.scene,
            affected.source,
            affected.name.as_str(),
            affected.filter_count
        ),
        (root, owner.id(), owner.name(), 2)
    );
    assert!(loaded.migration.retained_bytes() <= MAX_MIGRATION_REPORT_BYTES);
    assert_eq!(
        loaded
            .session
            .snapshot()
            .scene_source(root)
            .unwrap()
            .video_filters(),
        owner.video_filters()
    );
    assert!(loaded.session.snapshot().scene(host).is_some());
    let bytes = encode(&loaded.session.snapshot()).unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&bytes).unwrap()["schema"],
        CURRENT_SCHEMA
    );
    let reopened = decode_with_report(&bytes, true).unwrap();
    assert!(!reopened.migration.has_semantic_changes());
    assert!(!reopened.session.is_dirty());
    assert_eq!(reopened.session.snapshot().as_ref(), before.as_ref());
    let mut no_history = loaded.session;
    assert!(
        matches!(
            no_history.apply(Command::Undo),
            Err(skylive_core::Error::EmptyHistory)
        ),
        "migration is not a fake authored edit"
    );
}
#[test]
fn old_unfiltered_projects_have_no_owner_allocation_warning_or_dirty_flag() {
    let session = Session::new();
    let before = session.snapshot();
    let mut legacy: Value = serde_json::from_slice(&encode(&before).unwrap()).unwrap();
    legacy["schema"] = json!(3);
    let loaded = decode_with_report(&serde_json::to_vec(&legacy).unwrap(), true).unwrap();
    assert!(!loaded.session.is_dirty());
    assert!(!loaded.migration.has_semantic_changes());
    assert_eq!(loaded.session.snapshot().as_ref(), before.as_ref());
    assert!(loaded.session.snapshot().sources().is_empty());
}

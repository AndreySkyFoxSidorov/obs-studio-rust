use super::*;
use crate::{Command, Session};
use std::sync::Arc;
fn source(s: &mut Session, name: &str) -> Id {
    let scene = s.preview();
    s.apply(Command::AddColorSource {
        scene,
        name: name.into(),
        rgba: [255; 4],
    })
    .unwrap();
    s.snapshot().sources().last().unwrap().id()
}
fn draft() -> VideoFilter {
    VideoFilter::new(VideoEffect::color_correction()).unwrap()
}
#[test]
fn issued_ids_names_reorder_edits_noop_and_rejection_are_transactional() {
    let mut s = Session::new();
    let a = source(&mut s, "A");
    let b = source(&mut s, "B");
    s.apply(Command::SetSourceVideoFilters {
        source: a,
        filters: vec![
            draft(),
            draft().with_name("Color Correction".into()).unwrap(),
        ],
    })
    .unwrap();
    let old = s.snapshot();
    let filters = old.source(a).unwrap().video_filters();
    assert_eq!(filters[0].name(), "Color Correction 2");
    assert_eq!(filters[1].name(), "Color Correction");
    assert_ne!(filters[0].id(), filters[1].id());
    let mut reordered = vec![
        filters[1].clone(),
        filters[0]
            .clone()
            .with_name("Проверка цвета".into())
            .unwrap(),
    ];
    if let VideoEffect::ColorCorrection { opacity, .. } = &mut reordered[0].effect {
        *opacity = 0.5;
    }
    s.apply(Command::SetSourceVideoFilters {
        source: a,
        filters: reordered.clone(),
    })
    .unwrap();
    let edited = s.snapshot();
    assert_eq!(
        edited.source(a).unwrap().video_filters()[0].id(),
        filters[1].id()
    );
    assert!(
        !s.apply(Command::SetSourceVideoFilters {
            source: a,
            filters: reordered.clone()
        })
        .unwrap()
        .changed
    );
    assert!(Arc::ptr_eq(&edited, &s.snapshot()));
    let mut omit_names = reordered.clone();
    for filter in &mut omit_names {
        filter.name = None;
    }
    assert!(
        !s.apply(Command::SetSourceVideoFilters {
            source: a,
            filters: omit_names
        })
        .unwrap()
        .changed
    );
    assert!(Arc::ptr_eq(&edited, &s.snapshot()));
    for (target, replacement) in [
        (b, vec![reordered[0].clone()]),
        (a, vec![reordered[0].clone(), reordered[0].clone()]),
        (
            a,
            vec![
                draft().with_name("same".into()).unwrap(),
                draft().with_name("same".into()).unwrap(),
            ],
        ),
    ] {
        assert!(
            s.apply(Command::SetSourceVideoFilters {
                source: target,
                filters: replacement
            })
            .is_err()
        );
        assert!(Arc::ptr_eq(&edited, &s.snapshot()));
    }
    let mut forged = draft();
    forged.id = Some(VideoFilterId::new(edited.collection.next_id).unwrap());
    assert!(
        s.apply(Command::SetSourceVideoFilters {
            source: a,
            filters: vec![forged]
        })
        .is_err()
    );
    assert!(Arc::ptr_eq(&edited, &s.snapshot()));
    s.apply(Command::Undo).unwrap();
    assert_eq!(s.snapshot().source(a).unwrap().video_filters(), filters);
    s.apply(Command::Redo).unwrap();
    assert_eq!(s.snapshot().source(a).unwrap().video_filters(), reordered);
}
#[test]
fn undo_and_reopen_never_reuse_issued_filter_ids_and_exhaustion_rolls_back() {
    let mut s = Session::new();
    let a = source(&mut s, "A");
    s.apply(Command::SetSourceVideoFilters {
        source: a,
        filters: vec![draft()],
    })
    .unwrap();
    let retired = s.snapshot().source(a).unwrap().video_filters()[0]
        .id()
        .unwrap();
    s.apply(Command::Undo).unwrap();
    let mut reopened = Session::from_snapshot((*s.snapshot()).clone(), true).unwrap();
    reopened
        .apply(Command::SetSourceVideoFilters {
            source: a,
            filters: vec![draft()],
        })
        .unwrap();
    assert!(
        reopened.snapshot().source(a).unwrap().video_filters()[0]
            .id()
            .unwrap()
            .get()
            > retired.get()
    );
    let mut exhausted = (*reopened.snapshot()).clone();
    exhausted.collection.next_id = u64::MAX;
    let mut exhausted = Session::from_snapshot(exhausted, true).unwrap();
    let before = exhausted.snapshot();
    assert!(matches!(
        exhausted.apply(Command::SetSourceVideoFilters {
            source: a,
            filters: vec![draft()]
        }),
        Err(Error::IdentityExhausted)
    ));
    assert!(Arc::ptr_eq(&before, &exhausted.snapshot()));
}
#[test]
fn owned_legacy_migration_prevalidates_present_ids_before_issuing_missing_metadata() {
    let mut s = Session::new();
    let a = source(&mut s, "A");
    let mut legacy = (*s.snapshot()).clone();
    legacy.collection.sources[0].video_filters = vec![draft(), draft()];
    let old_next = legacy.collection.next_id;
    let migrated = Session::from_snapshot(legacy.clone(), true).unwrap();
    let model = migrated.snapshot();
    let filters = model.source(a).unwrap().video_filters();
    assert_eq!(filters[0].id().unwrap().get(), old_next);
    assert_eq!(filters[1].id().unwrap().get(), old_next + 1);
    assert_eq!(filters[1].name(), "Color Correction 2");
    legacy.collection.sources[0].video_filters[1].id = Some(VideoFilterId::new(old_next).unwrap());
    assert!(
        Session::from_snapshot(legacy, true).is_err(),
        "a future provided ID cannot become valid during migration"
    );
}
#[test]
fn private_group_duplication_gets_distinct_filter_ids_and_preserves_authored_names() {
    let mut s = Session::new();
    let root = s.preview();
    source(&mut s, "Leaf");
    let item = s.snapshot().scene(root).unwrap().items()[0].id();
    let wrapper = s
        .apply(Command::GroupItems {
            scene: root,
            name: "Group".into(),
            items: vec![item],
        })
        .unwrap()
        .created
        .unwrap();
    let group_source = s
        .snapshot()
        .scene(root)
        .unwrap()
        .items()
        .iter()
        .find(|i| i.id() == wrapper)
        .unwrap()
        .source();
    s.apply(Command::SetSourceVideoFilters {
        source: group_source,
        filters: vec![draft().with_name("Колір".into()).unwrap()],
    })
    .unwrap();
    s.apply(Command::DuplicateScene {
        scene: root,
        name: "Copy".into(),
    })
    .unwrap();
    let model = s.snapshot();
    let filters: Vec<_> = model
        .sources()
        .iter()
        .flat_map(|s| s.video_filters())
        .collect();
    assert_eq!(filters.len(), 2);
    assert_ne!(filters[0].id(), filters[1].id());
    assert!(filters.iter().all(|f| f.name() == "Колір"));
}

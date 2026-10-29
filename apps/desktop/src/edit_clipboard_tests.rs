use super::*;
use skylive_core::{Crop, VideoEffect, VideoFilter};
fn color(s: &mut Session, name: &str, w: u32, h: u32) -> Id {
    s.apply(Command::AddColorSourceSized {
        scene: s.preview(),
        name: name.into(),
        rgba: [255; 4],
        width: w,
        height: h,
    })
    .unwrap()
    .created
    .unwrap()
}
fn copy(c: &mut Clipboard, s: &Session, item: Id, action: Action) {
    let r = c.request(action, s, Some(item)).unwrap();
    assert!(matches!(
        c.execute(r, s, Some(item)).unwrap(),
        Outcome::Copied(_)
    ));
}
fn paste(c: &mut Clipboard, s: &mut Session, item: Id, action: Action) {
    let r = c.request(action, s, Some(item)).unwrap();
    let Outcome::Command { revision, command } = c.execute(r, s, Some(item)).unwrap() else {
        panic!()
    };
    s.apply_at(Some(revision), command).unwrap();
}
fn assert_undo_content(
    before: &skylive_core::StudioSnapshot,
    pasted: &skylive_core::StudioSnapshot,
    actual: &skylive_core::StudioSnapshot,
) {
    let mut expected = serde_json::to_value(before.collection()).unwrap();
    expected["next_id"] = serde_json::to_value(pasted.collection()).unwrap()["next_id"].clone();
    assert_eq!(serde_json::to_value(actual.collection()).unwrap(), expected);
}
#[test]
fn transform_copies_scale_and_crop_immediately_but_preserves_destination_opacity() {
    let mut s = Session::new();
    let a = color(&mut s, "A", 100, 50);
    let b = color(&mut s, "B", 400, 200);
    let mut t = Transform::full(100, 50);
    t.width = 200.;
    t.height = 25.;
    t.x = -17.;
    t.y = 23.;
    t.rotation_degrees = 90.;
    t.flip_x = true;
    t.crop = Crop {
        left: 2,
        top: 3,
        right: 4,
        bottom: 5,
    };
    t.opacity = 0.2;
    s.apply(Command::SetTransform {
        scene: s.preview(),
        item: a,
        transform: t.clone(),
    })
    .unwrap();
    let mut dst = Transform::full(400, 200);
    dst.opacity = 0.7;
    s.apply(Command::SetTransform {
        scene: s.preview(),
        item: b,
        transform: dst,
    })
    .unwrap();
    let mut c = Clipboard::default();
    copy(&mut c, &s, a, Action::CopyTransform);
    s.apply(Command::SetTransform {
        scene: s.preview(),
        item: a,
        transform: Transform::full(100, 50),
    })
    .unwrap();
    paste(&mut c, &mut s, b, Action::PasteTransform);
    let model = s.snapshot();
    let (_, item) = model.item_container(b).unwrap();
    let pasted = item.transform();
    assert_eq!((pasted.width, pasted.height), (800., 100.));
    assert_eq!(pasted.opacity, 0.7);
    assert_eq!(
        (pasted.x, pasted.y, pasted.rotation_degrees),
        (-17., 23., 90.)
    );
    assert!(pasted.flip_x);
    assert_eq!(pasted.crop, t.crop);
}
#[test]
fn filters_read_current_weak_source_and_expire_on_delete_or_session_replacement() {
    let mut s = Session::new();
    let a = color(&mut s, "A", 16, 16);
    let b = color(&mut s, "B", 16, 16);
    let from = s.snapshot().item_container(a).unwrap().1.source();
    let to = s.snapshot().item_container(b).unwrap().1.source();
    let mut c = Clipboard::default();
    copy(&mut c, &s, a, Action::CopyFilters);
    s.apply(Command::SetSourceVideoFilters {
        source: from,
        filters: vec![
            VideoFilter::new(VideoEffect::color_correction())
                .unwrap()
                .with_name("Latest".into())
                .unwrap(),
        ],
    })
    .unwrap();
    paste(&mut c, &mut s, b, Action::PasteFilters);
    assert_eq!(
        s.snapshot().source(to).unwrap().video_filters()[0].name(),
        "Latest"
    );
    s.apply(Command::RemoveItem {
        scene: s.preview(),
        item: a,
    })
    .unwrap();
    c.synchronize(&s).unwrap();
    assert!(c.request(Action::PasteFilters, &s, Some(b)).is_err());
    s.apply(Command::Undo).unwrap();
    assert!(
        c.request(Action::PasteFilters, &s, Some(b)).is_err(),
        "expired weak reference cannot resurrect after Undo"
    );
    let mut other = Session::new();
    let item = color(&mut other, "Other", 16, 16);
    assert!(c.request(Action::PasteFilters, &other, Some(item)).is_err());
}
#[test]
fn queued_requests_recheck_revision_selection_preview_owner_and_clipboard_generation() {
    let mut s = Session::new();
    let a = color(&mut s, "A", 16, 16);
    let b = color(&mut s, "B", 16, 16);
    let mut c = Clipboard::default();
    copy(&mut c, &s, a, Action::CopyTransform);
    let r = c.request(Action::PasteTransform, &s, Some(b)).unwrap();
    assert!(c.execute(r, &s, Some(a)).is_err());
    copy(&mut c, &s, a, Action::CopyFilters);
    assert!(c.execute(r, &s, Some(b)).is_err());
    let r = c.request(Action::PasteTransform, &s, Some(b)).unwrap();
    s.apply(Command::RenameScene {
        scene: s.preview(),
        name: "Renamed".into(),
    })
    .unwrap();
    assert!(c.execute(r, &s, Some(b)).is_err());
    let mut r = c.request(Action::PasteTransform, &s, Some(b)).unwrap();
    r.target.source = Id::new(999);
    assert!(c.execute(r, &s, Some(b)).is_err());
    let old_preview = s.preview();
    let other = s
        .apply(Command::AddScene {
            name: "Other scene".into(),
        })
        .unwrap()
        .created
        .unwrap();
    let r = c.request(Action::PasteTransform, &s, Some(b)).unwrap();
    let revision = s.snapshot().revision();
    s.select_preview(other).unwrap();
    assert_eq!(s.snapshot().revision(), revision);
    assert!(c.execute(r, &s, Some(b)).is_err());
    s.select_preview(old_preview).unwrap();
}
#[test]
fn locked_targets_and_invalid_native_crop_reject_without_mutation_and_copy_identity_never_wraps() {
    let mut s = Session::new();
    let a = color(&mut s, "A", 100, 100);
    let b = color(&mut s, "B", 1, 1);
    let mut t = Transform::full(100, 100);
    t.crop.left = 20;
    s.apply(Command::SetTransform {
        scene: s.preview(),
        item: a,
        transform: t,
    })
    .unwrap();
    let mut c = Clipboard::default();
    copy(&mut c, &s, a, Action::CopyTransform);
    let before = s.snapshot();
    assert!(c.request(Action::PasteTransform, &s, Some(b)).is_err());
    assert!(std::sync::Arc::ptr_eq(&before, &s.snapshot()));
    s.apply(Command::SetLocked {
        scene: s.preview(),
        item: a,
        locked: true,
    })
    .unwrap();
    assert!(c.request(Action::PasteTransform, &s, Some(a)).is_err());
    c.generation = u64::MAX;
    let r = c.request(Action::CopyFilters, &s, Some(a)).unwrap();
    assert!(c.execute(r, &s, Some(a)).is_err());
    assert_eq!(c.generation, u64::MAX);
}

#[test]
fn source11_neural_clipboard_reads_current_parameters_and_rechecks_disabled_provider() {
    let mut s = Session::new();
    let a = color(&mut s, "Neural A", 16, 16);
    let b = color(&mut s, "Neural B", 16, 16);
    let from = s.snapshot().item_container(a).unwrap().1.source();
    let to = s.snapshot().item_container(b).unwrap().1.source();
    s.apply(Command::SetPluginEnabled {
        plugin: skylive_core::PERSON_BACKGROUND_PLUGIN.into(),
        enabled: true,
    })
    .unwrap();
    let mut original = VideoFilter::new(VideoEffect::PersonBackgroundRemoval {
        threshold: 0.71,
        softness: 0.24,
    })
    .unwrap();
    original.enabled = false;
    s.apply(Command::SetSourceVideoFilters {
        source: from,
        filters: vec![original],
    })
    .unwrap();
    let mut clipboard = Clipboard::default();
    copy(&mut clipboard, &s, a, Action::CopyFilters);
    s.apply(Command::SetPluginEnabled {
        plugin: skylive_core::PERSON_BACKGROUND_PLUGIN.into(),
        enabled: false,
    })
    .unwrap();
    assert!(matches!(
        clipboard.request(Action::PasteFilters, &s, Some(b)),
        Err(Error::Core(skylive_core::Error::Unavailable(_)))
    ));
    assert!(s.snapshot().source(to).unwrap().video_filters().is_empty());
    s.apply(Command::SetPluginEnabled {
        plugin: skylive_core::PERSON_BACKGROUND_PLUGIN.into(),
        enabled: true,
    })
    .unwrap();
    let mut current = s.snapshot().source(from).unwrap().video_filters().to_vec();
    current[0].effect = VideoEffect::PersonBackgroundRemoval {
        threshold: 0.82,
        softness: 0.11,
    };
    s.apply(Command::SetSourceVideoFilters {
        source: from,
        filters: current,
    })
    .unwrap();
    let before = s.snapshot();
    paste(&mut clipboard, &mut s, b, Action::PasteFilters);
    let after = s.snapshot();
    let copied = &after.source(to).unwrap().video_filters()[0];
    assert_eq!(
        copied.effect,
        VideoEffect::PersonBackgroundRemoval {
            threshold: 0.82,
            softness: 0.11
        }
    );
    assert!(!copied.enabled);
    assert_ne!(
        copied.id(),
        after.source(from).unwrap().video_filters()[0].id()
    );
    s.apply(Command::Undo).unwrap();
    assert_undo_content(&before, &after, &s.snapshot());
}

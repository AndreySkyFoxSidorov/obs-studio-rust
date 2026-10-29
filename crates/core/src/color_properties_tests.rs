//! Shared command boundary only; imported as core::color_properties_tests.
use super::*;
use std::sync::Arc;
fn add(session: &mut Session, name: &str, width: u32, height: u32) -> (Id, Id) {
    let scene = session.preview();
    let item = session
        .apply(Command::AddColorSourceSized {
            scene,
            name: name.into(),
            rgba: [10, 20, 30, 128],
            width,
            height,
        })
        .unwrap()
        .created
        .unwrap();
    let source = session
        .snapshot()
        .scene(scene)
        .unwrap()
        .items()
        .iter()
        .find(|i| i.id() == item)
        .unwrap()
        .source();
    (source, item)
}
#[test]
fn combined_color_and_size_is_one_undo_and_rescales_every_reference_not_crop_counts() {
    let mut s = Session::new();
    let scene = s.preview();
    let (source, first) = add(&mut s, "Shared", 100, 80);
    let second = s
        .apply(Command::AddExistingSource { scene, source })
        .unwrap()
        .created
        .unwrap();
    let transform = Transform {
        x: 9.0,
        y: -3.0,
        width: 50.0,
        height: 160.0,
        crop: Crop {
            left: 10,
            top: 2,
            right: 5,
            bottom: 3,
        },
        rotation_degrees: 37.0,
        flip_x: true,
        ..Transform::full(100, 80)
    };
    s.apply(Command::SetTransform {
        scene,
        item: second,
        transform: transform.clone(),
    })
    .unwrap();
    let before = s.snapshot();
    let command = Command::ConfigureColorSource {
        source,
        rgba: [200, 4, 90, 64],
        width: 200,
        height: 40,
    };
    s.apply(command.clone()).unwrap();
    let after = s.snapshot();
    assert_eq!(after.revision(), before.revision() + 1);
    assert_eq!(
        after.source(source).unwrap().kind(),
        &SourceKind::Color {
            rgba: [200, 4, 90, 64],
            width: 200,
            height: 40
        }
    );
    let items = after.scene(scene).unwrap().items();
    assert_eq!(
        items.iter().find(|i| i.id() == first).unwrap().transform(),
        &Transform::full(200, 40)
    );
    let actual = items.iter().find(|i| i.id() == second).unwrap().transform();
    assert_eq!((actual.width, actual.height), (100.0, 80.0));
    assert_eq!(
        (actual.x, actual.y, actual.rotation_degrees, actual.flip_x),
        (9.0, -3.0, 37.0, true)
    );
    assert_eq!(actual.crop, transform.crop);
    assert!(!s.apply(command).unwrap().changed);
    assert!(Arc::ptr_eq(&after, &s.snapshot()));
    s.apply(Command::Undo).unwrap();
    assert_eq!(s.snapshot().sources(), before.sources());
    assert_eq!(s.snapshot().scenes(), before.scenes());
    s.apply(Command::Redo).unwrap();
    assert_eq!(s.snapshot().sources(), after.sources());
    assert_eq!(s.snapshot().scenes(), after.scenes());
}
#[test]
fn rejected_sizes_and_crop_changes_leave_rgba_geometry_and_highwater_unpublished() {
    let mut s = Session::new();
    let scene = s.preview();
    let (source, item) = add(&mut s, "Crop", 100, 80);
    s.apply(Command::SetTransform {
        scene,
        item,
        transform: Transform {
            crop: Crop {
                left: 60,
                ..Crop::default()
            },
            ..Transform::full(100, 80)
        },
    })
    .unwrap();
    let before = s.snapshot();
    for (width, height) in [(0, 80), (8193, 80), (100, 0), (50, 80)] {
        assert!(
            s.apply(Command::ConfigureColorSource {
                source,
                rgba: [255; 4],
                width,
                height
            })
            .is_err()
        );
        assert!(Arc::ptr_eq(&before, &s.snapshot()));
    }
    // Add rejects before publishing issued IDs; compare the next successful Add
    // with an independently equivalent control history that never attempted it.
    let mut a = Session::new();
    let mut b = Session::new();
    let scene = a.preview();
    assert!(
        a.apply(Command::AddColorSourceSized {
            scene,
            name: "Rejected".into(),
            rgba: [0; 4],
            width: 0,
            height: 10
        })
        .is_err()
    );
    let (_, ia) = add(&mut a, "Accepted", 9, 7);
    let (_, ib) = add(&mut b, "Accepted", 9, 7);
    assert_eq!(ia, ib);
}
#[test]
fn old_color_commands_remain_compatible_and_configure_rejects_another_kind() {
    let mut s = Session::new();
    let scene = s.preview();
    let item = s
        .apply(Command::AddColorSource {
            scene,
            name: "Legacy".into(),
            rgba: [1, 2, 3, 4],
        })
        .unwrap()
        .created
        .unwrap();
    let snap = s.snapshot();
    let source = snap
        .scene(scene)
        .unwrap()
        .items()
        .iter()
        .find(|i| i.id() == item)
        .unwrap()
        .source();
    assert_eq!(
        snap.source(source).unwrap().kind().dimensions(),
        (snap.video().width, snap.video().height)
    );
    s.apply(Command::SetColorSize {
        source,
        width: 8,
        height: 6,
    })
    .unwrap();
    s.apply(Command::SetColor {
        source,
        rgba: [6, 5, 4, 3],
    })
    .unwrap();
    assert_eq!(
        s.snapshot().source(source).unwrap().kind(),
        &SourceKind::Color {
            rgba: [6, 5, 4, 3],
            width: 8,
            height: 6
        }
    );
    let audio = s
        .apply(Command::AddAudioCaptureSource {
            scene,
            name: "Audio".into(),
            binding: AudioCaptureBinding::new(AudioFlow::Input, AudioTarget::Default, false, false)
                .unwrap(),
        })
        .unwrap()
        .created
        .unwrap();
    let snap = s.snapshot();
    let source = snap
        .scene(scene)
        .unwrap()
        .items()
        .iter()
        .find(|i| i.id() == audio)
        .unwrap()
        .source();
    assert!(
        s.apply(Command::ConfigureColorSource {
            source,
            rgba: [0; 4],
            width: 8,
            height: 6
        })
        .is_err()
    );
    assert!(Arc::ptr_eq(&snap, &s.snapshot()));
}

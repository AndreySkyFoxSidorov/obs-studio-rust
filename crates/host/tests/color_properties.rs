//! Copy to crates/host/tests/color_properties.rs after the staged core/UI patch.
use skylive_core::{Command, Session, SourceKind};
use skylive_host::storage;
#[test]
fn sized_color_native_roundtrip_keeps_actual_basis_and_rgba_without_new_schema_fields() {
    let mut session = Session::new();
    let scene = session.preview();
    session
        .apply(Command::AddColorSourceSized {
            scene,
            name: "Native size".into(),
            rgba: [20, 30, 40, 50],
            width: 317,
            height: 229,
        })
        .unwrap();
    let source = session.snapshot().sources()[0].id();
    session
        .apply(Command::ConfigureColorSource {
            source,
            rgba: [5, 99, 201, 111],
            width: 601,
            height: 409,
        })
        .unwrap();
    let snapshot = session.snapshot();
    let loaded = storage::decode(&storage::encode(&snapshot).unwrap(), true).unwrap();
    assert_ne!(loaded.identity(), session.identity());
    assert_eq!(
        loaded.snapshot().source(source).unwrap().kind(),
        &SourceKind::Color {
            rgba: [5, 99, 201, 111],
            width: 601,
            height: 409
        }
    );
    assert_eq!(
        loaded.snapshot().scene(scene).unwrap().items(),
        snapshot.scene(scene).unwrap().items()
    );
}

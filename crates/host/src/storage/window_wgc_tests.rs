use super::*;
use skylive_core::{
    Command, WindowCaptureBinding, WindowCaptureMethod, WindowMatchPriority, WindowTarget,
};
fn fixture() -> Session {
    let mut s = Session::new();
    let scene = s.preview();
    s.apply(Command::AddWindowCaptureSource {
        scene,
        name: "WGC native document".into(),
        binding: WindowCaptureBinding::new(
            WindowTarget::Windows {
                title: "Saved".into(),
                class: "FixtureClass".into(),
                executable: "fixture.exe".into(),
            },
            WindowMatchPriority::Class,
            false,
            false,
            16,
            16,
        )
        .unwrap()
        .with_method(WindowCaptureMethod::Wgc)
        .unwrap(),
    })
    .unwrap();
    s
}
#[test]
fn schema7_preserves_method_and_undo_while_old_schema_cannot_backdate_wgc() {
    let mut s = fixture();
    let original = s.snapshot();
    let source = original.sources()[0].id();
    let bytes = encode(&original).unwrap();
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["schema"], CURRENT_SCHEMA);
    let mut schema7 = value.clone();
    schema7["schema"] = 7.into();
    let loaded7 = decode(&serde_json::to_vec(&schema7).unwrap(), true).unwrap();
    assert_eq!(
        loaded7.snapshot().source(source).unwrap().kind(),
        original.source(source).unwrap().kind()
    );
    let loaded = decode(&bytes, true).unwrap();
    assert_eq!(
        loaded.snapshot().source(source).unwrap().kind(),
        original.source(source).unwrap().kind()
    );
    let skylive_core::SourceKind::WindowCapture { binding } =
        original.source(source).unwrap().kind()
    else {
        panic!()
    };
    s.apply(Command::SetWindowCaptureSource {
        source,
        binding: binding
            .clone()
            .with_method(WindowCaptureMethod::Gdi)
            .unwrap(),
    })
    .unwrap();
    s.apply(Command::Undo).unwrap();
    assert_eq!(
        s.snapshot().source(source).unwrap().kind(),
        original.source(source).unwrap().kind()
    );
    for schema in [2, 3, 4, 5, 6] {
        let mut old = value.clone();
        old["schema"] = schema.into();
        assert!(
            decode(&serde_json::to_vec(&old).unwrap(), true).is_err(),
            "nondefault WGC must not be accepted under schema{schema}"
        );
    }
}
#[test]
fn schema6_missing_method_migrates_to_actual_legacy_gdi_and_bad_method_rejects() {
    let s = fixture();
    let mut value: serde_json::Value =
        serde_json::from_slice(&encode(&s.snapshot()).unwrap()).unwrap();
    value["schema"] = 6.into();
    let sources = value["collection"]["sources"].as_array_mut().unwrap();
    let binding = sources[0]["kind"]["binding"].as_object_mut().unwrap();
    binding.remove("method");
    let loaded = decode(&serde_json::to_vec(&value).unwrap(), true).unwrap();
    assert!(
        matches!(loaded.snapshot().sources()[0].kind(),skylive_core::SourceKind::WindowCapture{binding} if binding.method()==WindowCaptureMethod::Gdi)
    );
    value["schema"] = 7.into();
    value["collection"]["sources"][0]["kind"]["binding"]["method"] = "not_a_method".into();
    assert!(decode(&serde_json::to_vec(&value).unwrap(), true).is_err());
}

use super::*;
use skylive_core::VideoEffect;
#[test]
fn real_image_source_filter_uses_decoded_dimensions_rgb_and_same_session_export_metadata() {
    static SERIAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    struct Directory(PathBuf);
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let path = std::env::temp_dir().join(format!(
        "skylive-obs-cc-image-{}-{}",
        std::process::id(),
        SERIAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::create_dir(&path).unwrap();
    let directory = Directory(path);
    const PNG: &[u8] = &[
        137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 4, 0, 0, 0, 2, 8, 6,
        0, 0, 0, 127, 168, 125, 99, 0, 0, 0, 18, 73, 68, 65, 84, 120, 156, 99, 248, 207, 192, 240,
        31, 25, 51, 160, 11, 0, 0, 15, 33, 15, 241, 4, 55, 198, 159, 0, 0, 0, 0, 73, 69, 78, 68,
        174, 66, 96, 130,
    ];
    let file = directory.0.join("red.png");
    std::fs::write(&file, PNG).unwrap();
    let file = std::fs::canonicalize(file).unwrap();
    let mut value = fixture();
    value["sources"][0]["id"] = json!("image_source");
    value["sources"][0]["versioned_id"] = json!("image_source");
    value["sources"][0]["settings"] =
        json!({"file":file,"linear_alpha":false,"unload":false,"is_slide":false});
    value["sources"][0]["filters"]
        .as_array_mut()
        .unwrap()
        .truncate(1);
    value["sources"][0]["filters"][0]["name"] = json!("Відтінок");
    value["sources"][0]["filters"][0]["settings"]["brightness"] = json!(0);
    value["sources"][0]["filters"][0]["settings"]["hue_shift"] = json!(120);
    let imported = import(&value).unwrap();
    let model = imported.session.snapshot();
    assert_eq!(model.sources()[0].kind().video_dimensions(), Some((4, 2)));
    assert_eq!(&pixels(&imported.session)[..4], &[0, 0, 255, 255]);
    let output = exported(&imported.session, Some(&imported.identities));
    assert_eq!(
        output["sources"][0]["filters"][0]["uuid"],
        value["sources"][0]["filters"][0]["uuid"]
    );
    assert_eq!(output["sources"][0]["settings"]["file"], json!(file));
    let reopened = import(&output).unwrap();
    assert_eq!(pixels(&reopened.session), pixels(&imported.session));
}
fn fixture() -> Value {
    serde_json::from_str(include_str!(
        "../../tests/fixtures/obs-color-correction-v2.json"
    ))
    .unwrap()
}
fn video() -> ImportVideo {
    ImportVideo {
        width: 16,
        height: 16,
        fps_num: 30,
        fps_den: 1,
    }
}
fn import(value: &Value) -> Result<ImportedCollection, ObsError> {
    import_collection(&serde_json::to_vec(value).unwrap(), video())
}
fn source(s: &Session) -> Id {
    s.snapshot().sources()[0].id()
}
fn pixels(s: &Session) -> Vec<u8> {
    crate::preview::render(&s.snapshot(), s.program(), 16, 16)
        .unwrap()
        .pixels()
        .to_vec()
}
fn exported(s: &Session, ids: Option<&ObsIdentities>) -> Value {
    serde_json::from_slice(&export_collection(s, ids).unwrap()).unwrap()
}
#[test]
fn literal_obs_gui_json_and_execution_are_forward_and_identity_survives_edit_reorder_undo() {
    // save_source reverses internal source->filters, enum_filters does the
    // same and AddFilter appends. Thus this literal array is OBS GUI order.
    // D(gray)-1 clamps0, then+1 giveswhite. Reverse givesblack, independently.
    let mut input = import(&fixture()).unwrap();
    let id = source(&input.session);
    let before = input.session.snapshot();
    let filters = before.source(id).unwrap().video_filters();
    assert_eq!(
        filters.iter().map(|f| f.name()).collect::<Vec<_>>(),
        vec!["Черный", "White", "Disabled dark"]
    );
    assert!(
        pixels(&input.session)
            .chunks_exact(4)
            .all(|p| p == [255; 4])
    );
    let original = exported(&input.session, Some(&input.identities));
    for index in 0..3 {
        for field in ["uuid", "name", "id", "versioned_id", "enabled"] {
            assert_eq!(
                original["sources"][0]["filters"][index][field],
                fixture()["sources"][0]["filters"][index][field]
            );
        }
    }
    assert_eq!(
        original["sources"][0]["filters"][0]["settings"]["color_multiply"],
        2885681151_u32
    );
    assert_eq!(
        original["sources"][0]["filters"][0]["settings"]["color_add"],
        3439329280_u32
    );
    let mut reordered = filters.to_vec();
    reordered.swap(0, 1);
    reordered[1] = reordered[1]
        .clone()
        .with_name("Темний колір".into())
        .unwrap();
    input
        .session
        .apply(Command::SetSourceVideoFilters {
            source: id,
            filters: reordered,
        })
        .unwrap();
    assert!(
        pixels(&input.session)
            .chunks_exact(4)
            .all(|p| p == [0, 0, 0, 255])
    );
    let changed = exported(&input.session, Some(&input.identities));
    assert_eq!(
        changed["sources"][0]["filters"][0]["uuid"],
        original["sources"][0]["filters"][1]["uuid"]
    );
    assert_eq!(
        changed["sources"][0]["filters"][1]["uuid"],
        original["sources"][0]["filters"][0]["uuid"]
    );
    assert_eq!(changed["sources"][0]["filters"][1]["name"], "Темний колір");
    input.session.apply(Command::Undo).unwrap();
    assert_eq!(
        input.session.snapshot().source(id).unwrap().video_filters(),
        filters
    );
    assert_eq!(exported(&input.session, Some(&input.identities)), original);
}
#[test]
fn native_reopen_preserves_engine_identity_and_names_but_original_obs_uuid_sidecar_is_session_only()
{
    let input = import(&fixture()).unwrap();
    let original = exported(&input.session, Some(&input.identities));
    let bytes = crate::storage::encode(&input.session.snapshot()).unwrap();
    let reopened = crate::storage::decode(&bytes, true).unwrap();
    assert_eq!(
        reopened.snapshot().sources()[0].video_filters(),
        input.session.snapshot().sources()[0].video_filters()
    );
    assert!(export_collection(&reopened, Some(&input.identities)).is_err());
    let generated = exported(&reopened, None);
    assert_ne!(
        generated["sources"][0]["uuid"],
        original["sources"][0]["uuid"]
    );
    assert_ne!(
        generated["sources"][0]["filters"][0]["uuid"],
        original["sources"][0]["filters"][0]["uuid"]
    );
    assert_eq!(generated["sources"][0]["filters"][0]["name"], "Черный");
    let roundtrip = import(&generated).unwrap();
    assert_eq!(pixels(&roundtrip.session), pixels(&reopened));
}
#[test]
fn identical_effects_have_distinct_stable_uuid_names_after_reorder_and_settings_edits() {
    let mut value = fixture();
    value["sources"][0]["filters"][1]["settings"] =
        value["sources"][0]["filters"][0]["settings"].clone();
    let mut input = import(&value).unwrap();
    let id = source(&input.session);
    let old = input.session.snapshot();
    let filters = old.source(id).unwrap().video_filters();
    assert_eq!(filters[0].effect, filters[1].effect);
    let mut next = filters.to_vec();
    next.swap(0, 1);
    if let skylive_core::VideoEffect::ColorCorrection { gamma, .. } = &mut next[0].effect {
        *gamma = 1.0;
    }
    input
        .session
        .apply(Command::SetSourceVideoFilters {
            source: id,
            filters: next,
        })
        .unwrap();
    let output = exported(&input.session, Some(&input.identities));
    assert_eq!(
        output["sources"][0]["filters"][0]["uuid"],
        value["sources"][0]["filters"][1]["uuid"]
    );
    assert_eq!(output["sources"][0]["filters"][0]["settings"]["gamma"], 1.0);
    assert_eq!(
        output["sources"][0]["filters"][1]["uuid"],
        value["sources"][0]["filters"][0]["uuid"]
    );
}
#[test]
fn meaningful_unknown_v1_private_invalid_and_duplicate_metadata_reject_even_disabled_before_image_io()
 {
    for (pointer, value) in [
        ("/sources/0/filters/0/versioned_id", json!("color_filter")),
        ("/sources/0/filters/0/id", json!("unknown_filter")),
        ("/sources/0/filters/0/settings/unknown", json!(0)),
        ("/sources/0/filters/0/settings/gamma", json!(3.01)),
        (
            "/sources/0/filters/0/private_settings",
            json!({"meaningful":true}),
        ),
        ("/sources/0/filters/0/hotkeys", json!({"toggle":[]})),
        ("/sources/0/filters/0/filters", json!([{}])),
        ("/sources/0/filters/1/name", json!("Черный")),
        (
            "/sources/0/filters/1/uuid",
            json!("4457ed57-09de-4c8e-aad8-3708dd393b9b"),
        ),
        (
            "/sources/0/filters/1/uuid",
            json!("1457ed57-09de-4c8e-aad8-3708dd393b9b"),
        ),
    ] {
        let mut invalid = fixture();
        let (parent, key) = pointer.rsplit_once('/').unwrap();
        invalid
            .pointer_mut(parent)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert(key.into(), value);
        invalid["sources"][0]["filters"][0]["enabled"] = json!(false);
        assert!(import(&invalid).is_err(), "{pointer}");
    }
    let mut invalid = fixture();
    invalid["sources"][0]["id"] = json!("image_source");
    invalid["sources"][0]["versioned_id"] = json!("image_source");
    invalid["sources"][0]["settings"] = json!({"file":"missing.png"});
    invalid["sources"][0]["filters"][0]["settings"]["gamma"] = json!(4);
    let error = import(&invalid).err().unwrap();
    assert!(error.issues[0].path.ends_with(".gamma"));
}

#[test]
fn literal_keyer_v2_settings_order_identity_inactive_custom_color_and_native_roundtrip() {
    let mut input = fixture();
    input["sources"][0]["settings"]["color"] = json!(0xff00ff00u32);
    input["sources"][0]["filters"] = json!([
        {"id":"color_key_filter","versioned_id":"color_key_filter_v2","name":"Key RGB","uuid":"12345678-1234-1234-1234-123456789a01","enabled":true,
         "settings":{"key_color_type":"green","key_color":0xab030201u32,"similarity":80,"smoothness":50,"gamma":0.123456789012345,"contrast":-0.234567890123456,"brightness":0.0,"opacity":1.0}},
        {"id":"chroma_key_filter","versioned_id":"chroma_key_filter_v2","name":"Хромакей","uuid":"12345678-1234-1234-1234-123456789a02","enabled":false,
         "settings":{"key_color_type":"blue","key_color":0xcd060504u32,"similarity":400,"smoothness":80,"spill":100,"gamma":0.0,"contrast":0.0,"brightness":0.0,"opacity":1.0}}
    ]);
    let mut imported = import(&input).unwrap();
    let id = source(&imported.session);
    let old = imported.session.snapshot();
    let authored = old.source(id).unwrap().video_filters();
    assert_eq!(authored.len(), 2);
    assert!(matches!(
        authored[0].effect,
        VideoEffect::ColorKey {
            key_color: [1, 2, 3],
            ..
        }
    ));
    assert!(
        pixels(&imported.session)
            .chunks_exact(4)
            .all(|p| p == [0, 0, 0, 255])
    );
    let output = exported(&imported.session, Some(&imported.identities));
    for i in 0..2 {
        for key in ["uuid", "name", "enabled", "id", "versioned_id", "settings"] {
            assert_eq!(
                output["sources"][0]["filters"][i][key], input["sources"][0]["filters"][i][key],
                "filter{i} {key}"
            );
        }
    }
    let reopened = crate::storage::decode(&crate::storage::encode(&old).unwrap(), true).unwrap();
    assert_eq!(
        reopened.snapshot().source(id).unwrap().video_filters(),
        authored
    );
    let mut filters = authored.to_vec();
    filters.swap(0, 1);
    filters[0].enabled = true;
    imported
        .session
        .apply(Command::SetSourceVideoFilters {
            source: id,
            filters,
        })
        .unwrap();
    let reordered = exported(&imported.session, Some(&imported.identities));
    assert_eq!(
        reordered["sources"][0]["filters"][0]["uuid"],
        input["sources"][0]["filters"][1]["uuid"]
    );
    imported.session.apply(Command::Undo).unwrap();
    assert_eq!(
        imported
            .session
            .snapshot()
            .source(id)
            .unwrap()
            .video_filters(),
        authored
    );
}
#[test]
fn keyer_v1_unknown_fields_wrong_integer_and_invalid_preset_are_rejected_before_resource_io() {
    for (version, settings) in [
        ("color_key_filter", json!({})),
        ("color_key_filter_v2", json!({"similarity":0})),
        ("color_key_filter_v2", json!({"smoothness":80.25})),
        ("color_key_filter_v2", json!({"key_color_type":"yellow"})),
        ("color_key_filter_v2", json!({"spill":1})),
        ("color_key_filter_v2", json!({"gamma":1.001})),
        ("color_key_filter_v2", json!({"opacity":-0.01})),
    ] {
        let mut value = fixture();
        value["sources"][0]["id"] = json!("image_source");
        value["sources"][0]["versioned_id"] = json!("image_source");
        value["sources"][0]["settings"] = json!({"file":"nonexistent-do-not-open.png"});
        value["sources"][0]["filters"] = json!([{"id":"color_key_filter","versioned_id":version,"name":"Key","settings":settings}]);
        let error = import(&value).err().unwrap();
        assert!(
            error.issues.iter().any(|i| i.path.contains("filters")),
            "{error:?}"
        );
    }
}

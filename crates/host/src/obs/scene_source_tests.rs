//! One OBS scene object/UUID owns its root and nested filter stack.
use super::*;
use skylive_core::{VideoEffect, VideoFilter};
fn scene_fixture() -> Session {
    let mut s = Session::new();
    let root = s.preview();
    s.apply(Command::SetVideo {
        width: 16,
        height: 16,
        fps_num: 30,
        fps_den: 1,
    })
    .unwrap();
    s.apply(Command::AddColorSource {
        scene: root,
        name: "White".into(),
        rgba: [255; 4],
    })
    .unwrap();
    let child = s
        .apply(Command::AddScene {
            name: "Container".into(),
        })
        .unwrap()
        .created
        .unwrap();
    s.apply(Command::AddSceneSource {
        scene: child,
        target: root,
    })
    .unwrap();
    let mut effect = VideoEffect::color_correction();
    if let VideoEffect::ColorCorrection { opacity, .. } = &mut effect {
        *opacity = 0.5;
    }
    s.apply(Command::SetSceneVideoFilters {
        scene: root,
        filters: vec![
            VideoFilter::new(effect)
                .unwrap()
                .with_name("Scene alpha".into())
                .unwrap(),
        ],
    })
    .unwrap();
    s
}
fn import(value: &Value) -> Result<ImportedCollection, ObsError> {
    import_collection(
        &serde_json::to_vec(value).unwrap(),
        ImportVideo {
            width: 16,
            height: 16,
            fps_num: 30,
            fps_den: 1,
        },
    )
}
#[test]
fn scene_owner_is_one_object_uuid_and_shared_stack_with_matching_direct_nested_pixels() {
    let original = scene_fixture();
    let wire: Value = serde_json::from_slice(&export_collection(&original, None).unwrap()).unwrap();
    assert_eq!(
        wire["sources"].as_array().unwrap().len(),
        3,
        "one white source and two scenes, no duplicate alias source"
    );
    let root = wire["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "Scene")
        .unwrap();
    assert_eq!(root["filters"][0]["name"], "Scene alpha");
    let container = wire["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "Container")
        .unwrap();
    assert_eq!(
        container["settings"]["items"][0]["source_uuid"],
        root["uuid"]
    );
    let imported = import(&wire).unwrap();
    let model = imported.session.snapshot();
    let root = model
        .scenes()
        .iter()
        .find(|s| s.name() == "Scene")
        .unwrap()
        .id();
    let container = model
        .scenes()
        .iter()
        .find(|s| s.name() == "Container")
        .unwrap()
        .id();
    assert_eq!(model.scene_source(root).unwrap().video_filters().len(), 1);
    for scene in [root, container] {
        let image = crate::preview::render(&model, scene, 16, 16).unwrap();
        assert!(
            image
                .pixels()
                .chunks_exact(4)
                .all(|p| p == [188, 188, 188, 255])
        );
    }
    let reexported: Value = serde_json::from_slice(
        &export_collection(&imported.session, Some(&imported.identities)).unwrap(),
    )
    .unwrap();
    let output = reexported["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "Scene")
        .unwrap();
    let old = wire["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "Scene")
        .unwrap();
    assert_eq!(output["uuid"], old["uuid"]);
    assert_eq!(output["filters"][0]["uuid"], old["filters"][0]["uuid"]);
    // Owner survives zero incoming refs and remains exported on its root object.
    let mut session = imported.session;
    let item = session.snapshot().scene(container).unwrap().items()[0].id();
    session
        .apply(Command::RemoveItem {
            scene: container,
            item,
        })
        .unwrap();
    let output: Value =
        serde_json::from_slice(&export_collection(&session, None).unwrap()).unwrap();
    let output = output["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "Scene")
        .unwrap();
    assert_eq!(output["filters"][0]["name"], "Scene alpha");
}
#[test]
fn imported_scene_cycles_reject_before_linked_image_io() {
    let s = scene_fixture();
    let mut wire: Value = serde_json::from_slice(&export_collection(&s, None).unwrap()).unwrap();
    let sources = wire["sources"].as_array_mut().unwrap();
    let container = sources
        .iter()
        .find(|v| v["name"] == "Container")
        .unwrap()
        .clone();
    let root = sources.iter_mut().find(|v| v["name"] == "Scene").unwrap();
    let mut nested = root["settings"]["items"][0].clone();
    nested["id"] = json!(99);
    nested["name"] = json!("Container");
    nested["source_uuid"] = container["uuid"].clone();
    root["settings"]["items"]
        .as_array_mut()
        .unwrap()
        .push(nested);
    let image = sources.iter_mut().find(|v| v["name"] == "White").unwrap();
    image["id"] = json!("image_source");
    image["versioned_id"] = json!("image_source");
    image["settings"] =
        json!({"file":std::env::current_dir().unwrap().join("nonexistent-never-read.png")});
    let error = import(&wire).err().unwrap();
    assert!(
        error.issues.iter().any(|i| i.message.contains("cycle")),
        "{error}"
    );
}

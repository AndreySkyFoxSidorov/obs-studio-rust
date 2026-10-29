//! Source-traced named built-in transition catalog and shared references.
//! OBSBasic_Transitions.cpp:1429..1483 saves only name/id/settings. Swipe,
//! Slide and Fade to Color are configurable; Cut/Fade are inherent names.
use super::{ObsError, array, issue, known, neutral, object, required};
use serde_json::{Map, Value, json};
use skylive_core::{
    Command, MAX_QUICK_TRANSITIONS, MAX_TRANSITION_PRESETS, QuickTransition, QuickTransitionId,
    Scene, Session, StudioSnapshot, TransitionDirection, TransitionKind, TransitionPreset,
    TransitionPresetId, TransitionRef, TransitionSelection,
};
use std::collections::{HashMap, HashSet};

pub(super) struct Catalog {
    entries: Vec<(TransitionPreset, Map<String, Value>)>,
}
impl Catalog {
    pub fn parse(root: &Map<String, Value>) -> Result<Self, ObsError> {
        let Some(value) = root.get("transitions") else {
            return Ok(Self {
                entries: Vec::new(),
            });
        };
        let rows = array(value, "$.transitions")?;
        if rows.len() > MAX_TRANSITION_PRESETS {
            return Err(issue(
                "$.transitions",
                "limit",
                "at most 64 named transition presets are supported",
            ));
        }
        let mut entries = Vec::with_capacity(rows.len());
        let mut names = HashSet::new();
        for (index, value) in rows.iter().enumerate() {
            let path = format!("$.transitions[{index}]");
            let row = object(value, &path)?;
            known(row, &["name", "id", "settings"], &path)?;
            let name = required(row, "name", &path)?.as_str().ok_or_else(|| {
                issue(
                    &format!("{path}.name"),
                    "invalid",
                    "expected transition name string",
                )
            })?;
            if !names.insert(name.to_owned()) {
                return Err(issue(
                    &format!("{path}.name"),
                    "invalid",
                    "duplicate transition preset name",
                ));
            }
            let provider = required(row, "id", &path)?.as_str().ok_or_else(|| {
                issue(
                    &format!("{path}.id"),
                    "invalid",
                    "expected transition provider ID string",
                )
            })?;
            let settings = object(
                required(row, "settings", &path)?,
                &format!("{path}.settings"),
            )?;
            let kind = settings_kind(provider, settings, &path)?;
            let id = TransitionPresetId::new(index as u32 + 1)
                .map_err(|error| issue(&path, "invalid", error.to_string()))?;
            let preset = TransitionPreset::new(id, name.to_owned(), kind)
                .map_err(|error| issue(&format!("{path}.name"), "invalid", error.to_string()))?;
            entries.push((preset, row.clone()));
        }
        Ok(Self { entries })
    }
    pub fn reference(&self, value: &Value, path: &str) -> Result<TransitionRef, ObsError> {
        let name = value
            .as_str()
            .ok_or_else(|| issue(path, "invalid", "expected transition name string"))?;
        match name {
            "Cut" => Ok(TransitionRef::Cut),
            "Fade" => Ok(TransitionRef::Fade),
            _ => self
                .entries
                .iter()
                .find(|(preset, _)| preset.name() == name)
                .map(|(preset, _)| TransitionRef::Preset(preset.id()))
                .ok_or_else(|| {
                    issue(
                        path,
                        "unsupported",
                        format!("unknown or unsupported transition name {name:?}"),
                    )
                }),
        }
    }
    pub fn install(&self, session: &mut Session) -> Result<(), ObsError> {
        for (preset, _) in &self.entries {
            let change = session
                .apply(Command::AddTransitionPreset {
                    name: preset.name().to_owned(),
                    kind: preset.kind(),
                })
                .map_err(|error| issue("$.transitions", "engine_validation", error.to_string()))?;
            if change.created_transition_preset != Some(preset.id()) {
                return Err(issue(
                    "$.transitions",
                    "engine_validation",
                    "transition catalog identity assignment mismatch",
                ));
            }
        }
        Ok(())
    }
    pub fn metadata(&self) -> HashMap<TransitionPresetId, Map<String, Value>> {
        self.entries
            .iter()
            .map(|(preset, raw)| (preset.id(), raw.clone()))
            .collect()
    }
}
fn direction(settings: &Map<String, Value>, path: &str) -> Result<TransitionDirection, ObsError> {
    match settings.get("direction") {
        None => Ok(TransitionDirection::Left),
        Some(Value::String(value)) => match value.as_str() {
            "" | "left" => Ok(TransitionDirection::Left),
            "right" => Ok(TransitionDirection::Right),
            "up" => Ok(TransitionDirection::Up),
            "down" => Ok(TransitionDirection::Down),
            _ => Err(issue(
                &format!("{path}.direction"),
                "unsupported",
                "unknown transition direction",
            )),
        },
        _ => Err(issue(
            &format!("{path}.direction"),
            "invalid",
            "expected direction string",
        )),
    }
}
fn settings_kind(
    provider: &str,
    settings: &Map<String, Value>,
    path: &str,
) -> Result<TransitionKind, ObsError> {
    let settings_path = format!("{path}.settings");
    match provider {
        "wipe_transition"=>{
            known(settings,&["luma_image","luma_softness","luma_invert"],&settings_path)?;
            let mask=match settings.get("luma_image"){
                None=>skylive_core::LumaWipePattern::default(),
                Some(value)=>skylive_core::LumaWipePattern::from_filename(value.as_str().ok_or_else(||issue(&settings_path,"invalid","expected stock Luma Wipe filename"))?)
                    .map_err(|e|issue(&settings_path,"unsupported",e.to_string()))?,
            };
            let softness=match settings.get("luma_softness"){
                None=>skylive_core::LumaWipeSoftness::default(),
                Some(value)=>{
                    let number=value.as_f64().filter(|n|n.is_finite()&&(0.0..=1.0).contains(n)).ok_or_else(||issue(&settings_path,"invalid","Luma Wipe softness must be finite0..1"))?;
                    skylive_core::LumaWipeSoftness::new(number as f32).map_err(|e|issue(&settings_path,"invalid",e.to_string()))?
                },
            };
            let invert=settings.get("luma_invert").map(|v|v.as_bool().ok_or_else(||issue(&settings_path,"invalid","expected Luma Wipe inversion boolean"))).transpose()?.unwrap_or(false);
            Ok(TransitionKind::LumaWipe{mask,softness,invert})
        }
        "swipe_transition" => {
            known(settings, &["direction", "swipe_in"], &settings_path)?;
            let swipe_in = match settings.get("swipe_in") {
                None => false,
                Some(value) => value.as_bool().ok_or_else(|| {
                    issue(
                        &format!("{settings_path}.swipe_in"),
                        "invalid",
                        "expected boolean",
                    )
                })?,
            };
            Ok(TransitionKind::Swipe {
                direction: direction(settings, &settings_path)?,
                swipe_in,
            })
        }
        "slide_transition" => {
            known(settings, &["direction"], &settings_path)?;
            Ok(TransitionKind::Slide {
                direction: direction(settings, &settings_path)?,
            })
        }
        "fade_to_color_transition" => {
            known(settings, &["color", "switch_point"], &settings_path)?;
            let color = match settings.get("color") {
                None => 0xff000000u32,
                Some(value) => value
                    .as_u64()
                    .and_then(|value| u32::try_from(value).ok())
                    .ok_or_else(|| {
                        issue(
                            &format!("{settings_path}.color"),
                            "invalid",
                            "expected unsigned packed RGBA32 color",
                        )
                    })?,
            };
            let switch_point = match settings.get("switch_point") {
                None => 50,
                Some(value) => value
                    .as_u64()
                    .filter(|value| *value <= 100)
                    .ok_or_else(|| {
                        issue(
                            &format!("{settings_path}.switch_point"),
                            "invalid",
                            "switch point must be an integer in 0..=100",
                        )
                    })? as u8,
            };
            // The source ORs 0xff000000 into a local copy. Its stored alpha
            // byte is ignored visually, and may remain in original metadata.
            Ok(TransitionKind::FadeToColor {
                rgb: [color as u8, (color >> 8) as u8, (color >> 16) as u8],
                switch_point,
            })
        }
        _ => Err(issue(
            &format!("{path}.id"),
            "unsupported",
            format!("unsupported configured transition provider {provider:?}"),
        )),
    }
}
fn duration(value: &Value, path: &str) -> Result<u32, ObsError> {
    match value.as_u64() {
        Some(value) if (50..=20_000).contains(&value) => Ok(value as u32),
        _ => Err(issue(
            path,
            "unsupported",
            "supported transition duration is 50..20000 ms",
        )),
    }
}
pub(super) fn quick_rows(
    root: &Map<String, Value>,
    catalog: &Catalog,
) -> Result<Vec<QuickTransition>, ObsError> {
    let Some(value) = root.get("quick_transitions") else {
        return Ok(Vec::new());
    };
    let rows = array(value, "$.quick_transitions")?;
    if rows.len() > MAX_QUICK_TRANSITIONS {
        return Err(issue(
            "$.quick_transitions",
            "limit",
            "at most 64 quick transitions are supported",
        ));
    }
    let mut output = Vec::with_capacity(rows.len());
    let mut ids = HashSet::new();
    for (index, row) in rows.iter().enumerate() {
        let path = format!("$.quick_transitions[{index}]");
        let row = object(row, &path)?;
        known(
            row,
            &["name", "duration", "hotkeys", "id", "fade_to_black"],
            &path,
        )?;
        neutral(row, "hotkeys", json!([]), &path)?;
        let id = required(row, "id", &path)?
            .as_u64()
            .and_then(|value| u32::try_from(value).ok())
            .and_then(|value| QuickTransitionId::new(value).ok())
            .ok_or_else(|| {
                issue(
                    &format!("{path}.id"),
                    "invalid",
                    "quick transition ID must be in 1..=2147483646",
                )
            })?;
        if !ids.insert(id) {
            return Err(issue(
                &format!("{path}.id"),
                "invalid",
                "duplicate quick transition ID",
            ));
        }
        let reference =
            catalog.reference(required(row, "name", &path)?, &format!("{path}.name"))?;
        let duration = duration(
            required(row, "duration", &path)?,
            &format!("{path}.duration"),
        )?;
        let to_black = match row.get("fade_to_black") {
            None => false,
            Some(value) => value.as_bool().ok_or_else(|| {
                issue(
                    &format!("{path}.fade_to_black"),
                    "invalid",
                    "expected boolean",
                )
            })?,
        };
        let selection = TransitionSelection::new(reference, duration)
            .map_err(|error| issue(&path, "invalid", error.to_string()))?;
        output.push(
            QuickTransition::from_selection(id, selection, to_black)
                .map_err(|error| issue(&path, "invalid", error.to_string()))?,
        );
    }
    Ok(output)
}
pub(super) fn scene_override(
    source: &Map<String, Value>,
    path: &str,
    catalog: &Catalog,
) -> Result<(Option<TransitionRef>, u32), ObsError> {
    let Some(value) = source.get("private_settings") else {
        return Ok((None, 300));
    };
    let path = format!("{path}.private_settings");
    let settings = object(value, &path)?;
    known(settings, &["transition", "transition_duration", "show_in_multiview"], &path)?;
    if let Some(value)=settings.get("show_in_multiview"){if !value.is_boolean(){return Err(issue(&format!("{path}.show_in_multiview"),"invalid","expected boolean"));}}
    let name = match settings.get("transition") {
        None => "",
        Some(value) => value.as_str().ok_or_else(|| {
            issue(
                &format!("{path}.transition"),
                "invalid",
                "expected transition name string",
            )
        })?,
    };
    let duration = match settings.get("transition_duration") {
        None => 300,
        Some(value) => duration(value, &format!("{path}.transition_duration"))?,
    };
    if name.is_empty() {
        return Ok((None, duration));
    }
    Ok((
        Some(catalog.reference(
            &Value::String(name.to_owned()),
            &format!("{path}.transition"),
        )?),
        duration,
    ))
}
pub(super) fn reference_name(
    collection: &StudioSnapshot,
    reference: TransitionRef,
) -> Result<&str, ObsError> {
    match reference {
        TransitionRef::Cut => Ok("Cut"),
        TransitionRef::Fade => Ok("Fade"),
        TransitionRef::Preset(id) => collection
            .transition_preset(id)
            .map(TransitionPreset::name)
            .ok_or_else(|| {
                issue(
                    "$.transitions",
                    "invalid_reference",
                    format!("transition preset {} missing", id.get()),
                )
            }),
    }
}
fn direction_name(direction: TransitionDirection) -> &'static str {
    match direction {
        TransitionDirection::Left => "left",
        TransitionDirection::Right => "right",
        TransitionDirection::Up => "up",
        TransitionDirection::Down => "down",
    }
}
pub(super) fn encode_catalog(
    collection: &StudioSnapshot,
    metadata: Option<&HashMap<TransitionPresetId, Map<String, Value>>>,
) -> Result<Value, ObsError> {
    let mut output = Vec::with_capacity(collection.transition_presets().len());
    for preset in collection.transition_presets() {
        let (provider, settings) = match preset.kind() {
            TransitionKind::LumaWipe{mask,softness,invert}=>("wipe_transition",json!({"luma_image":mask.filename(),"luma_softness":softness.get(),"luma_invert":invert})),
            TransitionKind::Swipe {
                direction,
                swipe_in,
            } => (
                "swipe_transition",
                json!({"direction":direction_name(direction),"swipe_in":swipe_in}),
            ),
            TransitionKind::Slide { direction } => (
                "slide_transition",
                json!({"direction":direction_name(direction)}),
            ),
            TransitionKind::FadeToColor { rgb, switch_point } => (
                "fade_to_color_transition",
                json!({"color":0xff000000u32|u32::from(rgb[0])|(u32::from(rgb[1])<<8)|(u32::from(rgb[2])<<16),"switch_point":switch_point}),
            ),
            _ => {
                return Err(issue(
                    "$.transitions",
                    "invalid",
                    "named preset kind is not supported by OBS interchange",
                ));
            }
        };
        let old = metadata.and_then(|metadata| metadata.get(&preset.id()));
        let mut row = old.cloned().unwrap_or_default();
        let settings = if let Some(old) = old {
            match (
                old.get("id").and_then(Value::as_str),
                old.get("settings").and_then(Value::as_object),
            ) {
                (Some(old_provider), Some(old_settings))
                    if settings_kind(old_provider, old_settings, "$.transitions").ok()
                        == Some(preset.kind()) =>
                {
                    Value::Object(old_settings.clone())
                }
                _ => settings,
            }
        } else {
            settings
        };
        row.insert("name".into(), json!(preset.name()));
        row.insert("id".into(), json!(provider));
        row.insert("settings".into(), settings);
        output.push(Value::Object(row));
    }
    Ok(Value::Array(output))
}
pub(super) fn encode_rows(collection: &StudioSnapshot) -> Result<Value, ObsError> {
    let mut rows = Vec::with_capacity(collection.quick_transitions().len());
    for row in collection.quick_transitions() {
        rows.push(json!({"name":reference_name(collection,row.config().reference())?,"duration":row.config().duration_ms(),"id":row.id().get(),"fade_to_black":row.fade_to_black(),"hotkeys":[]}));
    }
    Ok(Value::Array(rows))
}
pub(super) fn encode_override(
    source: &mut Map<String, Value>,
    scene: &Scene,
    collection: &StudioSnapshot,
) -> Result<(), ObsError> {
    let mut settings = Map::new();
    if !scene.show_in_multiview(){settings.insert("show_in_multiview".into(),json!(false));}
    if let Some(config) = scene.transition_override() {
        settings.insert(
            "transition".into(),
            json!(reference_name(collection, config.reference())?),
        );
        settings.insert("transition_duration".into(), json!(config.duration_ms()));
    } else if scene.transition_duration_ms() != 300 {
        settings.insert("transition".into(), json!(""));
        settings.insert(
            "transition_duration".into(),
            json!(scene.transition_duration_ms()),
        );
    }
    source.insert("private_settings".into(), Value::Object(settings));
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::obs::{ImportVideo, export_collection, import_collection};
    use skylive_core::{Command, Session, TransitionConfig};
    fn video() -> ImportVideo {
        ImportVideo {
            width: 1920,
            height: 1080,
            fps_num: 30,
            fps_den: 1,
        }
    }
    fn fixture() -> Value {
        serde_json::from_str(include_str!("../../tests/fixtures/obs-color-absolute.json")).unwrap()
    }
    fn rows() -> Value {
        json!([
            {"name":"Cut","duration":300,"id":1,"hotkeys":[],"fade_to_black":false},
            {"name":"Fade","duration":650,"id":7,"hotkeys":[],"fade_to_black":true},
            {"name":"Fade","duration":500,"id":3,"hotkeys":[],"fade_to_black":false}
        ])
    }
    fn encoded(value: &Value) -> Vec<u8> {
        serde_json::to_vec(value).unwrap()
    }
    #[test]
    fn actual_obs_rows_and_scene_private_override_roundtrip_order_ids_and_edits() {
        let mut input = fixture();
        input["quick_transitions"] = rows();
        let first = input["sources"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|source| source["name"] == "First")
            .unwrap();
        first["private_settings"] = json!({"transition":"Fade","transition_duration":1250});
        let mut imported = import_collection(&encoded(&input), video()).unwrap();
        let initial = imported.session.snapshot();
        let scene = initial
            .scenes()
            .iter()
            .find(|scene| scene.name() == "First")
            .unwrap()
            .id();
        assert_eq!(
            initial
                .quick_transitions()
                .iter()
                .map(|row| row.id().get())
                .collect::<Vec<_>>(),
            [1, 7, 3]
        );
        assert_eq!(initial.quick_transitions()[1].config().duration_ms(), 650);
        assert!(initial.quick_transitions()[1].fade_to_black());
        assert_eq!(
            initial.scene(scene).unwrap().transition_override(),
            Some(&TransitionSelection::new(TransitionRef::Fade, 1250).unwrap())
        );
        let original: Value = serde_json::from_slice(
            &export_collection(&imported.session, Some(&imported.identities)).unwrap(),
        )
        .unwrap();
        assert_eq!(original["quick_transitions"], input["quick_transitions"]);
        imported
            .session
            .apply(Command::MoveQuickTransition {
                id: QuickTransitionId::new(3).unwrap(),
                index: 0,
            })
            .unwrap();
        imported
            .session
            .apply(Command::RemoveQuickTransition {
                id: QuickTransitionId::new(7).unwrap(),
            })
            .unwrap();
        imported
            .session
            .apply(Command::UpdateQuickTransition {
                id: QuickTransitionId::new(1).unwrap(),
                kind: TransitionKind::Fade,
                duration_ms: 900,
                fade_to_black: true,
            })
            .unwrap();
        imported
            .session
            .apply(Command::SetSceneTransition {
                scene,
                config: Some(TransitionConfig::new(TransitionKind::Cut, 750).unwrap()),
            })
            .unwrap();
        let exported = export_collection(&imported.session, Some(&imported.identities)).unwrap();
        let value: Value = serde_json::from_slice(&exported).unwrap();
        assert_eq!(value["quick_transitions"][0]["id"], 3);
        assert_eq!(value["quick_transitions"][1]["id"], 1);
        assert_eq!(value["quick_transitions"][1]["name"], "Fade");
        assert_eq!(value["quick_transitions"][1]["duration"], 900);
        let again = import_collection(&exported, video()).unwrap();
        assert_eq!(
            again.session.snapshot().quick_transitions(),
            imported.session.snapshot().quick_transitions()
        );
        assert_eq!(
            again
                .session
                .snapshot()
                .scenes()
                .iter()
                .find(|scene| scene.name() == "First")
                .unwrap()
                .transition_override(),
            Some(&TransitionSelection::new(TransitionRef::Cut, 750).unwrap())
        );
        assert!(
            value
                .get("transitions")
                .is_none_or(|providers| providers == &json!([])),
            "built-ins are not custom serialized providers"
        );
    }
    #[test]
    fn loaded_empty_or_missing_obs_rows_stay_empty_but_new_native_defaults_export() {
        for explicit in [false, true] {
            let mut input = fixture();
            if explicit {
                input["quick_transitions"] = json!([]);
            }
            let imported = import_collection(&encoded(&input), video()).unwrap();
            assert!(imported.session.snapshot().quick_transitions().is_empty());
        }
        let session = Session::new();
        let bytes = export_collection(&session, None).unwrap();
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["quick_transitions"].as_array().unwrap().len(), 3);
        let imported = import_collection(&bytes, video()).unwrap();
        assert_eq!(
            imported.session.snapshot().quick_transitions(),
            session.snapshot().quick_transitions()
        );
    }
    #[test]
    fn malformed_unknown_hotkey_provider_and_duplicate_rows_fail_explicitly() {
        for (pointer, value, path) in [
            ("/0/id", json!(0), "$.quick_transitions[0].id"),
            ("/0/name", json!("Stinger"), "$.quick_transitions[0].name"),
            ("/0/duration", json!(0), "$.quick_transitions[0].duration"),
            (
                "/0/hotkeys",
                json!([{"key":"OBS_KEY_F1"}]),
                "$.quick_transitions[0].hotkeys",
            ),
            ("/1/id", json!(1), "$.quick_transitions[1].id"),
        ] {
            let mut input = fixture();
            let mut quick = rows();
            *quick.pointer_mut(pointer).unwrap() = value;
            input["quick_transitions"] = quick;
            let error = import_collection(&encoded(&input), video()).err().unwrap();
            assert_eq!(error.issues[0].path, path);
        }
        let mut input = fixture();
        input["quick_transitions"] = Value::Array(vec![rows()[0].clone(); 65]);
        assert_eq!(
            import_collection(&encoded(&input), video())
                .err()
                .unwrap()
                .issues[0]
                .code,
            "limit"
        );
    }
    #[test]
    fn scene_override_unknown_provider_inactive_duration_and_source_type_are_not_dropped() {
        for settings in [
            json!({"transition":"Stinger"}),
            json!({"transition":"Fade","transition_duration":49}),
            json!({"transition":"Fade","future":true}),
        ] {
            let mut input = fixture();
            input["sources"][1]["private_settings"] = settings;
            assert!(import_collection(&encoded(&input), video()).is_err());
        }
        let mut input = fixture();
        input["sources"][0]["private_settings"] =
            json!({"transition":"Fade","transition_duration":300});
        assert_eq!(
            import_collection(&encoded(&input), video())
                .err()
                .unwrap()
                .issues[0]
                .path,
            "$.sources[0].private_settings"
        );
    }
    #[test]
    fn inactive_override_duration_survives_obs_disable_native_and_reenable() {
        let mut input = fixture();
        let source = input["sources"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|source| source["name"] == "First")
            .unwrap();
        source["private_settings"] = json!({"transition":"","transition_duration":750});
        let mut imported = import_collection(&encoded(&input), video()).unwrap();
        let scene = imported
            .session
            .snapshot()
            .scenes()
            .iter()
            .find(|scene| scene.name() == "First")
            .unwrap()
            .id();
        assert!(
            imported
                .session
                .snapshot()
                .scene(scene)
                .unwrap()
                .transition_override()
                .is_none()
        );
        assert_eq!(
            imported
                .session
                .snapshot()
                .scene(scene)
                .unwrap()
                .transition_duration_ms(),
            750
        );
        imported
            .session
            .apply(Command::SetSceneTransition {
                scene,
                config: Some(TransitionConfig::new(TransitionKind::Fade, 1250).unwrap()),
            })
            .unwrap();
        imported
            .session
            .apply(Command::SetSceneTransition {
                scene,
                config: None,
            })
            .unwrap();
        let bytes = export_collection(&imported.session, Some(&imported.identities)).unwrap();
        let again = import_collection(&bytes, video()).unwrap();
        let snapshot = again.session.snapshot();
        let scene = snapshot
            .scenes()
            .iter()
            .find(|scene| scene.name() == "First")
            .unwrap();
        assert!(scene.transition_override().is_none());
        assert_eq!(scene.transition_duration_ms(), 1250);
        let native = crate::storage::encode(&snapshot).unwrap();
        let reopened = crate::storage::decode(&native, true).unwrap();
        assert_eq!(
            reopened
                .snapshot()
                .scene(scene.id())
                .unwrap()
                .transition_duration_ms(),
            1250
        );
    }
    fn catalog_fixture() -> Value {
        let mut input = fixture();
        input["transitions"] = json!([
            {"name":"Push Right","id":"swipe_transition","settings":{"direction":"right","swipe_in":true}},
            {"name":"Separate Alias","id":"swipe_transition","settings":{"direction":"right","swipe_in":true}},
            {"name":"Unused Slide","id":"slide_transition","settings":{}},
        ]);
        input["current_transition"] = json!("Push Right");
        input["transition_duration"] = json!(750);
        input["quick_transitions"] = json!([
            {"name":"Push Right","duration":500,"id":5,"hotkeys":[],"fade_to_black":false},
            {"name":"Separate Alias","duration":650,"id":8,"hotkeys":[],"fade_to_black":true},
        ]);
        let scene = input["sources"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|source| source["name"] == "First")
            .unwrap();
        scene["private_settings"] = json!({"transition":"Push Right","transition_duration":1250});
        input
    }
    #[test]
    fn configured_obs_catalog_keeps_real_shared_refs_aliases_unused_presets_and_raw_defaults() {
        let input = catalog_fixture();
        let mut imported = import_collection(&encoded(&input), video()).unwrap();
        let initial = imported.session.snapshot();
        assert_eq!(initial.transition_presets().len(), 3);
        let first = initial.transition_presets()[0].id();
        let alias = initial.transition_presets()[1].id();
        assert_ne!(first, alias);
        assert_eq!(
            initial.transition().reference(),
            TransitionRef::Preset(first)
        );
        assert_eq!(
            initial.quick_transitions()[0].config().reference(),
            TransitionRef::Preset(first)
        );
        assert_eq!(
            initial.quick_transitions()[1].config().reference(),
            TransitionRef::Preset(alias)
        );
        let scene = initial
            .scenes()
            .iter()
            .find(|scene| scene.name() == "First")
            .unwrap()
            .id();
        assert_eq!(
            initial
                .scene(scene)
                .unwrap()
                .transition_override()
                .unwrap()
                .reference(),
            TransitionRef::Preset(first)
        );
        assert_eq!(
            initial
                .resolve_transition(initial.transition())
                .unwrap()
                .kind(),
            TransitionKind::Swipe {
                direction: TransitionDirection::Right,
                swipe_in: true
            }
        );
        let original: Value = serde_json::from_slice(
            &export_collection(&imported.session, Some(&imported.identities)).unwrap(),
        )
        .unwrap();
        assert_eq!(original["transitions"], input["transitions"]);
        assert_eq!(original["quick_transitions"], input["quick_transitions"]);
        let old_revision = initial.revision();
        let edited = TransitionKind::Slide {
            direction: TransitionDirection::Up,
        };
        imported
            .session
            .apply(Command::ConfigureTransitionPreset {
                id: first,
                name: "Renamed Shared".into(),
                kind: edited,
            })
            .unwrap();
        let current = imported.session.snapshot();
        assert_eq!(current.revision(), old_revision + 1);
        for selection in [
            current.transition(),
            current.quick_transitions()[0].config(),
            current.scene(scene).unwrap().transition_override().unwrap(),
        ] {
            assert_eq!(
                current.resolve_transition(selection).unwrap().kind(),
                edited
            );
        }
        assert_eq!(
            current
                .resolve_transition(current.quick_transitions()[1].config())
                .unwrap()
                .kind(),
            initial.transition_presets()[1].kind()
        );
        let value: Value = serde_json::from_slice(
            &export_collection(&imported.session, Some(&imported.identities)).unwrap(),
        )
        .unwrap();
        assert_eq!(value["current_transition"], "Renamed Shared");
        assert_eq!(value["quick_transitions"][0]["name"], "Renamed Shared");
        assert_eq!(
            value["transitions"][0],
            json!({"name":"Renamed Shared","id":"slide_transition","settings":{"direction":"up"}})
        );
        assert_eq!(value["transitions"][2], input["transitions"][2]);
        let exported_scene = value["sources"]
            .as_array()
            .unwrap()
            .iter()
            .find(|source| source["name"] == "First")
            .unwrap();
        assert_eq!(
            exported_scene["private_settings"]["transition"],
            "Renamed Shared"
        );
    }
    #[test]
    fn configured_catalog_and_references_survive_native_save_reopen_without_obs_sidecar() {
        let imported = import_collection(&encoded(&catalog_fixture()), video()).unwrap();
        let original = imported.session.snapshot();
        let bytes = crate::storage::encode(&original).unwrap();
        let reopened = crate::storage::decode(&bytes, true).unwrap();
        let current = reopened.snapshot();
        assert_eq!(current.transition_presets(), original.transition_presets());
        assert_eq!(current.transition(), original.transition());
        assert_eq!(current.quick_transitions(), original.quick_transitions());
        let output = export_collection(&reopened, None).unwrap();
        let again = import_collection(&output, video()).unwrap();
        let again = again.session.snapshot();
        assert_eq!(again.transition_presets(), current.transition_presets());
        assert_eq!(again.quick_transitions(), current.quick_transitions());
        assert_eq!(again.transition(), current.transition());
        assert!(
            again
                .transition_presets()
                .iter()
                .any(|preset| preset.name() == "Unused Slide")
        );
        assert_eq!(
            again
                .resolve_transition(again.transition())
                .unwrap()
                .duration_ms(),
            750
        );
        let native: Value = serde_json::from_slice(&bytes).unwrap();
        assert!(native["collection"]["transition"].get("kind").is_none());
        assert!(
            native["collection"]["quick_transitions"][0]["config"]
                .get("kind")
                .is_none()
        );
    }
    #[test]
    fn legacy_native_cut_fade_leaf_configs_migrate_without_inventing_presets() {
        let session = Session::new();
        let mut native: Value =
            serde_json::from_slice(&crate::storage::encode(&session.snapshot()).unwrap()).unwrap();
        let collection = native["collection"].as_object_mut().unwrap();
        collection.remove("transition_presets");
        collection.remove("next_transition_preset_id");
        collection.insert("transition".into(), json!({"kind":"cut","duration_ms":750}));
        collection.insert(
            "quick_transitions".into(),
            json!([{"id":1,"config":{"kind":"fade","duration_ms":500},"fade_to_black":false}]),
        );
        collection.get_mut("scenes").unwrap()[0]["transition_override"] =
            json!({"kind":"fade","duration_ms":1250});
        let reopened = crate::storage::decode(&encoded(&native), true).unwrap();
        let current = reopened.snapshot();
        assert!(current.transition_presets().is_empty());
        assert_eq!(current.transition().reference(), TransitionRef::Cut);
        assert_eq!(
            current.resolve_transition(current.transition()).unwrap(),
            TransitionConfig::new(TransitionKind::Cut, 750).unwrap()
        );
        assert_eq!(
            current.quick_transitions()[0].config().reference(),
            TransitionRef::Fade
        );
        assert_eq!(
            current.scenes()[0]
                .transition_override()
                .unwrap()
                .reference(),
            TransitionRef::Fade
        );
        let upgraded: Value =
            serde_json::from_slice(&crate::storage::encode(&current).unwrap()).unwrap();
        assert_eq!(upgraded["schema"], crate::storage::CURRENT_SCHEMA);
        assert_eq!(upgraded["collection"]["transition"]["reference"], "cut");
        assert!(upgraded["collection"]["transition"].get("kind").is_none());
        let mut invalid = upgraded.clone();
        invalid["collection"]["transition"] = json!({"reference":{"preset":999},"duration_ms":300});
        assert!(crate::storage::decode(&encoded(&invalid), true).is_err());
        invalid = upgraded;
        invalid["collection"]["transition"] =
            json!({"kind":{"swipe":{"direction":"left","swipe_in":false}},"duration_ms":300});
        assert!(
            crate::storage::decode(&encoded(&invalid), true).is_err(),
            "inline spatial native copies are not silently materialized into aliases"
        );
    }
    #[test]
    fn configured_catalog_defaults_and_every_direction_roundtrip() {
        for (direction, expected) in [
            ("left", TransitionDirection::Left),
            ("right", TransitionDirection::Right),
            ("up", TransitionDirection::Up),
            ("down", TransitionDirection::Down),
        ] {
            for (provider, kind) in [
                (
                    "slide_transition",
                    TransitionKind::Slide {
                        direction: expected,
                    },
                ),
                (
                    "swipe_transition",
                    TransitionKind::Swipe {
                        direction: expected,
                        swipe_in: false,
                    },
                ),
            ] {
                let mut input = fixture();
                input["transitions"] =
                    json!([{"name":"Configured","id":provider,"settings":{"direction":direction}}]);
                input["current_transition"] = json!("Configured");
                let imported = import_collection(&encoded(&input), video()).unwrap();
                let current = imported.session.snapshot();
                assert_eq!(
                    current
                        .resolve_transition(current.transition())
                        .unwrap()
                        .kind(),
                    kind
                );
                let output =
                    export_collection(&imported.session, Some(&imported.identities)).unwrap();
                let again = import_collection(&output, video()).unwrap();
                let current = again.session.snapshot();
                assert_eq!(
                    current
                        .resolve_transition(current.transition())
                        .unwrap()
                        .kind(),
                    kind
                );
            }
        }
        for settings in [
            json!({}),
            json!({"direction":""}),
            json!({"swipe_in":false}),
        ] {
            let mut input = fixture();
            input["transitions"] =
                json!([{"name":"Default","id":"swipe_transition","settings":settings}]);
            input["current_transition"] = json!("Default");
            let imported = import_collection(&encoded(&input), video()).unwrap();
            let current = imported.session.snapshot();
            assert_eq!(
                current
                    .resolve_transition(current.transition())
                    .unwrap()
                    .kind(),
                TransitionKind::Swipe {
                    direction: TransitionDirection::Left,
                    swipe_in: false
                }
            );
        }
    }
    #[test]
    fn malformed_unimplemented_ambiguous_and_dangling_catalog_data_is_explicitly_rejected() {
        for (pointer, value, path) in [
            (
                "/0/id",
                json!("obs_stinger_transition"),
                "$.transitions[0].id",
            ),
            (
                "/0/settings/direction",
                json!("diagonal"),
                "$.transitions[0].settings.direction",
            ),
            (
                "/0/settings/direction",
                json!(42),
                "$.transitions[0].settings.direction",
            ),
            (
                "/0/settings/swipe_in",
                json!(1),
                "$.transitions[0].settings.swipe_in",
            ),
            (
                "/0/settings",
                json!({"direction":"left","easing":"linear"}),
                "$.transitions[0].settings.easing",
            ),
            ("/0/name", json!("Cut"), "$.transitions[0].name"),
            ("/1/name", json!("Push Right"), "$.transitions[1].name"),
        ] {
            let mut input = catalog_fixture();
            *input["transitions"].pointer_mut(pointer).unwrap() = value;
            let error = import_collection(&encoded(&input), video()).err().unwrap();
            assert_eq!(error.issues[0].path, path);
        }
        for target in [0, 1, 2] {
            let mut input = catalog_fixture();
            match target {
                0 => input["current_transition"] = json!("Unknown"),
                1 => input["quick_transitions"][0]["name"] = json!("Unknown"),
                _ => {
                    let scene = input["sources"]
                        .as_array_mut()
                        .unwrap()
                        .iter_mut()
                        .find(|source| source["name"] == "First")
                        .unwrap();
                    scene["private_settings"]["transition"] = json!("Unknown");
                }
            }
            assert!(import_collection(&encoded(&input), video()).is_err());
        }
        let mut input = catalog_fixture();
        input["transitions"] = json!(vec![input["transitions"][0].clone(); 65]);
        assert_eq!(
            import_collection(&encoded(&input), video())
                .err()
                .unwrap()
                .issues[0]
                .code,
            "limit"
        );
    }
    fn fade_color_fixture(settings: Value) -> Value {
        let mut input = fixture();
        input["transitions"] =
            json!([{"name":"Color Bridge","id":"fade_to_color_transition","settings":settings}]);
        input["current_transition"] = json!("Color Bridge");
        input["quick_transitions"] = json!([{"name":"Color Bridge","id":9,"duration":650,"hotkeys":[],"fade_to_black":false}]);
        let scene = input["sources"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|source| source["name"] == "First")
            .unwrap();
        scene["private_settings"] = json!({"transition":"Color Bridge","transition_duration":750});
        input
    }
    #[test]
    fn fade_to_color_uses_source_defaults_and_ignores_packed_alpha_without_losing_native_meaning() {
        let input = fade_color_fixture(json!({}));
        let imported = import_collection(&encoded(&input), video()).unwrap();
        let current = imported.session.snapshot();
        assert_eq!(
            current
                .resolve_transition(current.transition())
                .unwrap()
                .kind(),
            TransitionKind::FadeToColor {
                rgb: [0, 0, 0],
                switch_point: 50
            }
        );
        let roundtrip: Value = serde_json::from_slice(
            &export_collection(&imported.session, Some(&imported.identities)).unwrap(),
        )
        .unwrap();
        assert_eq!(roundtrip["transitions"], input["transitions"]);
        for alpha in [0u32, 0x12, 0xff] {
            let packed = (alpha << 24) | 0x00345678;
            let input = fade_color_fixture(json!({"color":packed,"switch_point":37}));
            let imported = import_collection(&encoded(&input), video()).unwrap();
            let snapshot = imported.session.snapshot();
            let kind = TransitionKind::FadeToColor {
                rgb: [0x78, 0x56, 0x34],
                switch_point: 37,
            };
            assert_eq!(
                snapshot
                    .resolve_transition(snapshot.transition())
                    .unwrap()
                    .kind(),
                kind
            );
            assert_eq!(
                snapshot
                    .resolve_transition(snapshot.quick_transitions()[0].config())
                    .unwrap()
                    .kind(),
                kind
            );
            let exact: Value = serde_json::from_slice(
                &export_collection(&imported.session, Some(&imported.identities)).unwrap(),
            )
            .unwrap();
            assert_eq!(
                exact["transitions"][0]["settings"]["color"], packed,
                "OBS stores the original alpha byte, but ORs FF only into its render-local copy"
            );
            let reopened =
                crate::storage::decode(&crate::storage::encode(&snapshot).unwrap(), true).unwrap();
            let normalized: Value =
                serde_json::from_slice(&export_collection(&reopened, None).unwrap()).unwrap();
            assert_eq!(
                normalized["transitions"][0]["settings"]["color"],
                0xff345678u32
            );
            let again = import_collection(&encoded(&normalized), video()).unwrap();
            let current = again.session.snapshot();
            assert_eq!(
                current
                    .resolve_transition(current.transition())
                    .unwrap()
                    .kind(),
                kind
            );
        }
        for switch_point in [0, 1, 50, 99, 100] {
            let input =
                fade_color_fixture(json!({"color":0xffffffffu32,"switch_point":switch_point}));
            let imported = import_collection(&encoded(&input), video()).unwrap();
            let current = imported.session.snapshot();
            assert_eq!(
                current
                    .resolve_transition(current.transition())
                    .unwrap()
                    .kind(),
                TransitionKind::FadeToColor {
                    rgb: [255; 3],
                    switch_point
                }
            );
        }
    }
    #[test]
    fn fade_to_color_rejects_unknown_fields_invalid_ranges_and_invalid_native_preset() {
        for (settings, path) in [
            (json!({"color":-1}), "$.transitions[0].settings.color"),
            (
                json!({"color":4294967296u64}),
                "$.transitions[0].settings.color",
            ),
            (json!({"color":"red"}), "$.transitions[0].settings.color"),
            (
                json!({"switch_point":-1}),
                "$.transitions[0].settings.switch_point",
            ),
            (
                json!({"switch_point":101}),
                "$.transitions[0].settings.switch_point",
            ),
            (
                json!({"switch_point":50.5}),
                "$.transitions[0].settings.switch_point",
            ),
            (
                json!({"switch_point":true}),
                "$.transitions[0].settings.switch_point",
            ),
            (json!({"alpha":0.5}), "$.transitions[0].settings.alpha"),
        ] {
            let input = fade_color_fixture(settings);
            let error = import_collection(&encoded(&input), video()).err().unwrap();
            assert_eq!(error.issues[0].path, path);
        }
        let imported =
            import_collection(&encoded(&fade_color_fixture(json!({}))), video()).unwrap();
        let mut native: Value =
            serde_json::from_slice(&crate::storage::encode(&imported.session.snapshot()).unwrap())
                .unwrap();
        native["collection"]["transition_presets"][0]["kind"]["fade_to_color"]["switch_point"] =
            json!(101);
        assert!(crate::storage::decode(&encoded(&native), true).is_err());
    }
    #[test]
    fn shared_preset_type_edit_exports_fade_to_color_for_every_reference() {
        let mut imported = import_collection(&encoded(&catalog_fixture()), video()).unwrap();
        let current = imported.session.snapshot();
        let id = current.transition_presets()[0].id();
        let kind = TransitionKind::FadeToColor {
            rgb: [7, 8, 9],
            switch_point: 100,
        };
        imported
            .session
            .apply(Command::SetTransitionPresetKind { id, kind })
            .unwrap();
        let value: Value = serde_json::from_slice(
            &export_collection(&imported.session, Some(&imported.identities)).unwrap(),
        )
        .unwrap();
        assert_eq!(
            value["transitions"][0],
            json!({"name":"Push Right","id":"fade_to_color_transition","settings":{"color":0xff090807u32,"switch_point":100}})
        );
        assert_eq!(value["quick_transitions"][0]["name"], "Push Right");
        let again = import_collection(&encoded(&value), video()).unwrap();
        let current = again.session.snapshot();
        assert_eq!(
            current
                .resolve_transition(current.transition())
                .unwrap()
                .kind(),
            kind
        );
        assert_eq!(
            current
                .resolve_transition(current.quick_transitions()[0].config())
                .unwrap()
                .kind(),
            kind
        );
    }
    #[test]
    fn compound_delete_export_has_no_dangling_names_and_undo_restores_all_authored_refs() {
        let mut imported = import_collection(&encoded(&catalog_fixture()), video()).unwrap();
        let original = imported.session.snapshot();
        let id = original.transition_presets()[0].id();
        let scene = original
            .scenes()
            .iter()
            .find(|scene| scene.name() == "First")
            .unwrap()
            .id();
        imported
            .session
            .apply(Command::RemoveTransitionPresetAndReferences { id })
            .unwrap();
        let cleaned = imported.session.snapshot();
        assert_eq!(cleaned.quick_transitions().len(), 1);
        assert_eq!(cleaned.scene(scene).unwrap().transition_override(), None);
        assert_eq!(cleaned.scene(scene).unwrap().transition_duration_ms(), 1250);
        let bytes = export_collection(&imported.session, Some(&imported.identities)).unwrap();
        let text = std::str::from_utf8(&bytes).unwrap();
        assert!(!text.contains("Push Right"));
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["current_transition"], "Unused Slide");
        assert_eq!(value["transitions"].as_array().unwrap().len(), 2);
        let again = import_collection(&bytes, video()).unwrap();
        let current = again.session.snapshot();
        assert_eq!(
            reference_name(&current, current.transition().reference()).unwrap(),
            "Unused Slide"
        );
        let reopened =
            crate::storage::decode(&crate::storage::encode(&cleaned).unwrap(), true).unwrap();
        assert_eq!(
            reopened.snapshot().transition_presets(),
            cleaned.transition_presets()
        );
        imported.session.apply(Command::Undo).unwrap();
        let restored: Value = serde_json::from_slice(
            &export_collection(&imported.session, Some(&imported.identities)).unwrap(),
        )
        .unwrap();
        assert_eq!(restored["transitions"], catalog_fixture()["transitions"]);
        assert_eq!(restored["current_transition"], "Push Right");
        assert_eq!(
            restored["quick_transitions"],
            catalog_fixture()["quick_transitions"]
        );
    }
}

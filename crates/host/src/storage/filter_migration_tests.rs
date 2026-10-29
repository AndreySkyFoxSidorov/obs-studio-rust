use super::*;
use serde_json::{Value, json};
#[test]
fn authored_gamma_bits_survive_json_typed_value_and_native_wire_without_tolerance() {
    // These exact promoted-f32 decimals exposed the non-float_roundtrip parser.
    // Adjacent doubles must remain separate authored/cache identities even when
    // the resulting shader coefficient happens to round to the same f32.
    let values = [
        f64::from(-1.93_f32),
        f64::from(0.49_f32),
        -1.93_f64,
        0.49,
        f64::from_bits(0.49_f64.to_bits() - 1),
        f64::from_bits(0.49_f64.to_bits() + 1),
        -0.0,
        0.0,
        f64::MIN_POSITIVE,
        -f64::MIN_POSITIVE,
        f64::from_bits(1),
        -f64::from_bits(1),
        -3.0,
        3.0,
    ];
    let mut session = Session::new();
    let scene = session.preview();
    session
        .apply(Command::AddColorSource {
            scene,
            name: "Wire".into(),
            rgba: [255; 4],
        })
        .unwrap();
    let id = session.snapshot().sources()[0].id();
    for value in values {
        let wire = serde_json::to_vec(&value).unwrap();
        let typed: f64 = serde_json::from_slice(&wire).unwrap();
        let dynamic: Value = serde_json::from_slice(&wire).unwrap();
        assert_eq!(
            typed.to_bits(),
            value.to_bits(),
            "typed {}",
            String::from_utf8_lossy(&wire)
        );
        assert_eq!(
            dynamic.as_f64().unwrap().to_bits(),
            value.to_bits(),
            "Value {}",
            String::from_utf8_lossy(&wire)
        );
        let mut effect = VideoEffect::color_correction();
        if let VideoEffect::ColorCorrection { gamma, .. } = &mut effect {
            *gamma = value;
        }
        session
            .apply(Command::SetSourceVideoFilters {
                source: id,
                filters: vec![VideoFilter::new(effect).unwrap()],
            })
            .unwrap();
        let bytes = encode(&session.snapshot()).unwrap();
        let decoded = decode(&bytes, true).unwrap();
        let VideoEffect::ColorCorrection { gamma, .. } =
            decoded.snapshot().source(id).unwrap().video_filters()[0].effect
        else {
            panic!("color correction")
        };
        assert_eq!(
            gamma.to_bits(),
            value.to_bits(),
            "native wire {:016x}",
            value.to_bits()
        );
    }
}
#[test]
fn schema3_retains_modern_gamma_double_and_legacy_normalizes_exact_old_float_operand() {
    let (s, _) = fixture();
    let mut native: Value = serde_json::from_slice(&encode(&s.snapshot()).unwrap()).unwrap();
    native["schema"] = json!(3);
    native["collection"]["sources"][0]["video_filters"][0]["effect"]["gamma"] = json!(-1.93_f64);
    let current = decode(&serde_json::to_vec(&native).unwrap(), true).unwrap();
    let VideoEffect::ColorCorrection { gamma, .. } =
        current.snapshot().sources()[0].video_filters()[0].effect
    else {
        panic!("color correction")
    };
    assert_eq!(gamma.to_bits(), (-1.93_f64).to_bits());
    native["schema"] = json!(2);
    let legacy = decode(&serde_json::to_vec(&native).unwrap(), true).unwrap();
    // Legacy order reverses too, so the original first row becomes last.
    let VideoEffect::ColorCorrection { gamma, .. } =
        legacy.snapshot().sources()[0].video_filters()[1].effect
    else {
        panic!("color correction")
    };
    assert_eq!(gamma.to_bits(), f64::from(-1.93_f32).to_bits());
    let reopened = decode(&encode(&legacy.snapshot()).unwrap(), true).unwrap();
    assert_eq!(reopened.snapshot().sources(), legacy.snapshot().sources());
}
use skylive_core::{
    AudioCaptureBinding, AudioFilter, AudioFlow, AudioTarget, Command, VideoEffect, VideoFilter,
};
fn fixture() -> (Session, Id) {
    let mut s = Session::new();
    let scene = s.preview();
    s.apply(Command::SetVideo {
        width: 16,
        height: 16,
        fps_num: 30,
        fps_den: 1,
    })
    .unwrap();
    s.apply(Command::AddColorSource {
        scene,
        name: "Gray".into(),
        rgba: [128, 128, 128, 255],
    })
    .unwrap();
    let id = s.snapshot().sources()[0].id();
    let mut dark = VideoEffect::color_correction();
    if let VideoEffect::ColorCorrection { brightness, .. } = &mut dark {
        *brightness = -1.0;
    }
    let mut light = VideoEffect::color_correction();
    if let VideoEffect::ColorCorrection { brightness, .. } = &mut light {
        *brightness = 1.0;
    }
    s.apply(Command::SetSourceVideoFilters {
        source: id,
        filters: vec![
            VideoFilter::new(dark).unwrap(),
            VideoFilter::new(light).unwrap(),
        ],
    })
    .unwrap();
    s.apply(Command::AddAudioCaptureSource {
        scene,
        name: "Voice".into(),
        binding: AudioCaptureBinding::new(AudioFlow::Input, AudioTarget::Default, false, false)
            .unwrap(),
    })
    .unwrap();
    let audio = s.snapshot().sources()[1].id();
    s.apply(Command::SetPluginEnabled {
        plugin: skylive_core::AUDIO_PAN_PLUGIN.into(),
        enabled: true,
    })
    .unwrap();
    s.apply(Command::SetSourceAudioFilters {
        source: audio,
        filters: vec![
            AudioFilter::Pan {
                pan_left: 0.0,
                pan_right: 0.0,
                gain_left_db: 6.0,
                gain_right_db: 0.0,
                invert_left: false,
                invert_right: false,
            },
            AudioFilter::compressor(),
        ],
    })
    .unwrap();
    (s, id)
}
fn reverse_legacy(value: &mut Value) {
    for source in value["collection"]["sources"].as_array_mut().unwrap() {
        for key in ["video_filters", "audio_filters"] {
            if let Some(filters) = source.get_mut(key).and_then(Value::as_array_mut) {
                filters.reverse();
                if key == "video_filters" {
                    for filter in filters {
                        filter.as_object_mut().unwrap().remove("id");
                        filter.as_object_mut().unwrap().remove("name");
                    }
                }
            }
        }
    }
}
#[test]
fn schema2_order_upgrade_preserves_literal_video_and_audio_order_then_schema3_reopen_is_idempotent()
{
    let (s, id) = fixture();
    let mut legacy: Value = serde_json::from_slice(&encode(&s.snapshot()).unwrap()).unwrap();
    assert_eq!(legacy["schema"], CURRENT_SCHEMA);
    reverse_legacy(&mut legacy);
    legacy["schema"] = json!(2);
    let restored = decode(&serde_json::to_vec(&legacy).unwrap(), true).unwrap();
    assert_eq!(
        restored.snapshot().sources()[1].audio_filters(),
        s.snapshot().sources()[1].audio_filters()
    );
    assert_eq!(
        restored
            .snapshot()
            .source(id)
            .unwrap()
            .video_filters()
            .iter()
            .map(|f| f.effect)
            .collect::<Vec<_>>(),
        s.snapshot()
            .source(id)
            .unwrap()
            .video_filters()
            .iter()
            .map(|f| f.effect)
            .collect::<Vec<_>>()
    );
    let image = crate::preview::render(&restored.snapshot(), restored.preview(), 16, 16).unwrap();
    assert_eq!(&image.pixels()[..4], &[255; 4]);
    let native = encode(&restored.snapshot()).unwrap();
    let again = decode(&native, true).unwrap();
    assert_eq!(again.snapshot().sources(), restored.snapshot().sources());
    assert_eq!(encode(&again.snapshot()).unwrap(), native);
}
#[test]
fn schema1_flat_graph_reverses_both_legacy_stacks_before_issuing_filter_metadata() {
    let (s, _) = fixture();
    let mut value: Value = serde_json::from_slice(&encode(&s.snapshot()).unwrap()).unwrap();
    reverse_legacy(&mut value);
    let profile = &value["profiles"]["profiles"][0];
    let video = profile["video"].clone();
    let buffering = value["preferences"]["audio_buffering"].clone();
    let revision = value["revision"].clone();
    let mut collection = value["collection"].take();
    collection["video"] = video;
    collection["revision"] = revision;
    collection["audio"] = json!({"sample_rate":48000,"speakers":"stereo","buffering":buffering});
    let legacy = json!({"schema":1,"collection":collection});
    let restored = decode(&serde_json::to_vec(&legacy).unwrap(), true).unwrap();
    assert_eq!(
        restored.snapshot().sources()[1].audio_filters(),
        s.snapshot().sources()[1].audio_filters()
    );
    let image = crate::preview::render(&restored.snapshot(), restored.preview(), 16, 16).unwrap();
    assert_eq!(&image.pixels()[..4], &[255; 4]);
    assert!(
        restored.snapshot().sources()[0]
            .video_filters()
            .iter()
            .all(|f| f.id().is_some() && !f.name().is_empty())
    );
}
#[test]
fn schema3_missing_null_forged_and_duplicate_filter_metadata_reject_before_migration() {
    let (s, _) = fixture();
    let good: Value = serde_json::from_slice(&encode(&s.snapshot()).unwrap()).unwrap();
    for key in ["id", "name"] {
        let mut bad = good.clone();
        bad["collection"]["sources"][0]["video_filters"][0]
            .as_object_mut()
            .unwrap()
            .remove(key);
        assert!(
            decode(&serde_json::to_vec(&bad).unwrap(), true).is_err(),
            "missing {key}"
        );
    }
    for (key, value) in [
        ("id", Value::Null),
        ("name", Value::Null),
        ("id", json!(0)),
        ("id", json!(999999)),
        ("name", json!("")),
        ("unknown", json!(0)),
    ] {
        let mut bad = good.clone();
        bad["collection"]["sources"][0]["video_filters"][0][key] = value;
        assert!(
            decode(&serde_json::to_vec(&bad).unwrap(), true).is_err(),
            "{key}"
        );
    }
    for key in ["id", "name"] {
        let mut bad = good.clone();
        bad["collection"]["sources"][0]["video_filters"][1][key] =
            bad["collection"]["sources"][0]["video_filters"][0][key].clone();
        assert!(
            decode(&serde_json::to_vec(&bad).unwrap(), true).is_err(),
            "duplicate {key}"
        );
    }
}
#[test]
fn saved_undo_highwater_prevents_filter_identity_reuse_after_native_reopen() {
    let (mut s, id) = fixture();
    let retired = s
        .snapshot()
        .source(id)
        .unwrap()
        .video_filters()
        .last()
        .unwrap()
        .id()
        .unwrap();
    // Undo audio chain and source, then the video stack.
    for _ in 0..3 {
        s.apply(Command::Undo).unwrap();
    }
    assert!(s.snapshot().source(id).unwrap().video_filters().is_empty());
    let mut reopened = decode(&encode(&s.snapshot()).unwrap(), true).unwrap();
    reopened
        .apply(Command::SetSourceVideoFilters {
            source: id,
            filters: vec![VideoFilter::new(VideoEffect::color_correction()).unwrap()],
        })
        .unwrap();
    assert!(
        reopened.snapshot().source(id).unwrap().video_filters()[0]
            .id()
            .unwrap()
            .get()
            > retired.get()
    );
}

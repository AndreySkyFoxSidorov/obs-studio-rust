use super::*;
use skylive_core::{Command, Session, VideoFilter};
#[test]
fn cache_key_contains_every_authored_gamma_double_bit() {
    let gamma = 0.49_f64;
    let mut first = VideoEffect::color_correction();
    let mut second = first;
    if let VideoEffect::ColorCorrection { gamma: value, .. } = &mut first {
        *value = gamma;
    }
    if let VideoEffect::ColorCorrection { gamma: value, .. } = &mut second {
        *value = f64::from_bits(gamma.to_bits() + 1);
    }
    let a = prepare(first).0;
    let b = prepare(second).0;
    assert_ne!(a, b);
    assert_eq!(u64::from(a[1]) | (u64::from(a[2]) << 32), gamma.to_bits());
}
fn fixture(rgba: [u8; 4]) -> (Session, skylive_core::Id, skylive_core::Id) {
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
        name: "Patch".into(),
        rgba,
    })
    .unwrap();
    let source = s.snapshot().sources()[0].id();
    (s, scene, source)
}
fn set(s: &mut Session, source: skylive_core::Id, effect: VideoEffect) {
    s.apply(Command::SetSourceVideoFilters {
        source,
        filters: vec![VideoFilter::new(effect).unwrap()],
    })
    .unwrap();
}
#[test]
fn independent_translucent_pixels_survive_nested_group_and_native_history() {
    let (mut s, scene, source) = fixture([255, 255, 255, 128]);
    let before = s.snapshot();
    let mut effect = VideoEffect::color_correction();
    if let VideoEffect::ColorCorrection { brightness, .. } = &mut effect {
        *brightness = 1.0;
    }
    set(&mut s, source, effect);
    let render = |s: &Session| crate::preview::render(&s.snapshot(), scene, 16, 16).unwrap();
    assert_eq!(&render(&s).pixels()[..4], &[225, 225, 225, 255]);
    s.apply(Command::Undo).unwrap();
    assert_eq!(
        s.snapshot().source(source).unwrap().video_filters(),
        before.source(source).unwrap().video_filters()
    );
    assert_eq!(&render(&s).pixels()[..4], &[188, 188, 188, 255]);
    s.apply(Command::Redo).unwrap();
    let item = s.snapshot().scene(scene).unwrap().items()[0].id();
    s.apply(Command::GroupItems {
        scene,
        name: "Inner".into(),
        items: vec![item],
    })
    .unwrap();
    let group = s.snapshot().scene(scene).unwrap().items()[0].id();
    let outer = s
        .apply(Command::AddGroup {
            scene,
            name: "Outer".into(),
        })
        .unwrap()
        .created
        .unwrap();
    s.apply(Command::MoveIntoGroup {
        scene,
        item: group,
        group: outer,
    })
    .unwrap();
    assert_eq!(&render(&s).pixels()[..4], &[225, 225, 225, 255]);
    let native = crate::storage::encode(&s.snapshot()).unwrap();
    let reopened = crate::storage::decode(&native, true).unwrap();
    assert_eq!(render(&s).pixels(), render(&reopened).pixels());
    assert_eq!(
        reopened.snapshot().source(source).unwrap().video_filters()[0].effect,
        effect
    );
}
#[test]
fn builtin_is_available_with_craft_off_and_cache_identity_includes_encoding_all_fields() {
    let (mut s, _scene, source) = fixture([255, 255, 255, 128]);
    s.apply(Command::SetPluginEnabled {
        plugin: skylive_core::CRAFT_VIDEO_PLUGIN.into(),
        enabled: false,
    })
    .unwrap();
    assert!(
        s.snapshot()
            .preferences()
            .plugin_enabled(skylive_core::OBS_VIDEO_FILTERS_PLUGIN)
    );
    set(&mut s, source, VideoEffect::color_correction());
    let snapshot = s.snapshot();
    let source = snapshot.source(source).unwrap();
    let input = Arc::new(RgbaFrame::new(1, 1, vec![255, 255, 255, 128]).unwrap());
    let mut cache = FilterCache::default();
    let color = cache
        .apply_encoded(
            source,
            input.clone(),
            SourceBlend::LinearSrgb,
            false,
            32,
            16,
        )
        .unwrap();
    let image = cache
        .apply_encoded(
            source,
            input.clone(),
            SourceBlend::EncodedPremultipliedSrgb,
            false,
            32,
            16,
        )
        .unwrap();
    assert_eq!(color.image.pixels(), &[188, 188, 188, 128]);
    assert_eq!(image.image.pixels(), &[128; 4]);
    assert!(!Arc::ptr_eq(&color.image, &image.image));
    let cached = cache
        .apply_encoded(source, input, SourceBlend::LinearSrgb, false, 0, 0)
        .unwrap();
    assert!(Arc::ptr_eq(&color.image, &cached.image));
    assert_eq!((cached.new_bytes, cached.pixel_stages), (0, 0));
}
#[test]
fn malformed_or_disabled_settings_reject_atomically_and_legacy_stack_is_preserved() {
    let (mut s, _scene, source) = fixture([0, 0, 0, 255]);
    let original = s.snapshot();
    let mut invalid = VideoEffect::color_correction();
    if let VideoEffect::ColorCorrection { gamma, .. } = &mut invalid {
        *gamma = f64::NAN;
    }
    let mut invalid_filter = VideoFilter::new(VideoEffect::color_correction()).unwrap();
    invalid_filter.enabled = false;
    invalid_filter.effect = invalid;
    assert!(
        s.apply(Command::SetSourceVideoFilters {
            source,
            filters: vec![invalid_filter]
        })
        .is_err()
    );
    assert!(Arc::ptr_eq(&original, &s.snapshot()));
    let valid =
        serde_json::to_value(VideoFilter::new(VideoEffect::color_correction()).unwrap()).unwrap();
    for (field, value) in [
        ("gamma", serde_json::json!(3.1)),
        ("opacity", serde_json::json!(-0.1)),
        ("unknown", serde_json::json!(0)),
        ("hue_shift", serde_json::json!("12")),
    ] {
        let mut bad = valid.clone();
        bad["enabled"] = serde_json::json!(false);
        bad["effect"][field] = value;
        assert!(
            serde_json::from_value::<VideoFilter>(bad).is_err(),
            "{field}"
        );
    }
    let old = serde_json::json!({"enabled":true,"effect":{"kind":"brightness_contrast","brightness":12.0,"contrast":-4.0,"legacy":false}});
    assert!(serde_json::from_value::<VideoFilter>(old).is_ok());
}

//! Real fixed-model host tests. No camera, UI, fake matte or global shutdown.
use super::*;
use sha2::{Digest, Sha256};
use skylive_core::{Command, Id, PERSON_BACKGROUND_PLUGIN, Session, VideoFilter};
use skylive_media::{PersonMatteLimits, apply_person_matte, prepare_person_rgb};
use std::sync::Mutex;

const RGB_FIXTURE: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../person-matte/tests/data/portrait-input.f32"
));
const ONNX_MASK: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../person-matte/tests/data/portrait-onnx-reference-mask.f32"
));
const FREE_BYTES: usize = 8 * 1024 * 1024;
const FREE_WORK: usize = 4 * 1024 * 1024;
static REAL_INFERENCE: Mutex<()> = Mutex::new(());

fn floats(bytes: &[u8], expected: usize) -> Vec<f32> {
    assert_eq!(bytes.len(), expected * 4);
    bytes
        .chunks_exact(4)
        .map(|v| {
            let value = f32::from_le_bytes(v.try_into().unwrap());
            assert!(value.is_finite() && (0.0..=1.0).contains(&value));
            value
        })
        .collect()
}

fn portrait(varied_alpha: bool) -> Arc<RgbaFrame> {
    let rgb = floats(RGB_FIXTURE, 256 * 256 * 3);
    let mut rgba = Vec::with_capacity(256 * 256 * 4);
    for (index, pixel) in rgb.chunks_exact(3).enumerate() {
        rgba.extend(pixel.iter().map(|v| (v * 255.0).round() as u8));
        rgba.push(if varied_alpha {
            (index % 256) as u8
        } else {
            255
        });
    }
    Arc::new(RgbaFrame::new(256, 256, rgba).unwrap())
}

fn owner() -> (Session, Id) {
    let mut owner = Session::new();
    owner
        .apply(Command::AddColorSource {
            scene: owner.preview(),
            name: "Injected real portrait pixel boundary".into(),
            rgba: [255; 4],
        })
        .unwrap();
    let source = owner.snapshot().sources()[0].id();
    (owner, source)
}

fn enable(owner: &mut Session, enabled: bool) {
    owner
        .apply(Command::SetPluginEnabled {
            plugin: PERSON_BACKGROUND_PLUGIN.into(),
            enabled,
        })
        .unwrap();
}

fn install(owner: &mut Session, source: Id, effects: &[VideoEffect]) {
    owner
        .apply(Command::SetSourceVideoFilters {
            source,
            filters: effects
                .iter()
                .map(|e| VideoFilter::new(*e).unwrap())
                .collect(),
        })
        .unwrap();
}

fn filtered(
    cache: &mut FilterCache,
    owner: &Session,
    source: Id,
    input: Arc<RgbaFrame>,
    blend: SourceBlend,
    free_bytes: usize,
    free_work: usize,
) -> Result<FilterResult, FilterError> {
    let model = owner.snapshot();
    cache.apply_with_providers(
        model.source(source).unwrap(),
        input,
        blend,
        FilterProviders::from_snapshot(&model),
        free_bytes,
        free_work,
    )
}

fn pure_limits(input: &RgbaFrame) -> PersonMatteLimits {
    PersonMatteLimits {
        max_input_bytes: input.byte_len() + 65536 * 4,
        max_output_bytes: input.byte_len().max(196608 * 4),
        max_pixel_visits: 65536 * 5 + input.byte_len() as u64 / 4 * 5,
    }
}

fn bbox(foreground: &[bool]) -> [usize; 4] {
    let mut result = [256, 256, 0, 0];
    let mut count = 0;
    for (index, value) in foreground.iter().enumerate() {
        if *value {
            result[0] = result[0].min(index % 256);
            result[1] = result[1].min(index / 256);
            result[2] = result[2].max(index % 256);
            result[3] = result[3].max(index / 256);
            count += 1;
        }
    }
    assert!(count > 0, "real portrait must contain foreground");
    result
}

#[test]
fn real_person_portrait_matches_independent_onnx_foreground_and_requires_activation() {
    let _serial = REAL_INFERENCE.lock().unwrap();
    assert_eq!(
        format!("{:x}", Sha256::digest(RGB_FIXTURE)),
        "1a915a5073d37cb9cadf853b69b3946d884afb5770fee7387bf09177aae13089"
    );
    assert_eq!(
        format!("{:x}", Sha256::digest(ONNX_MASK)),
        "be587ab06614f2a7d3f7206c16c9629daf6672c77840cd2e31e692a9477f841c"
    );
    let (mut session, source) = owner();
    assert!(
        !session
            .snapshot()
            .preferences()
            .plugin_enabled(PERSON_BACKGROUND_PLUGIN)
    );
    let before = session.snapshot();
    assert!(
        session
            .apply(Command::SetSourceVideoFilters {
                source,
                filters: vec![VideoFilter::new(VideoEffect::person_background_removal()).unwrap()],
            })
            .is_err()
    );
    assert!(Arc::ptr_eq(&before, &session.snapshot()));
    enable(&mut session, true);
    assert!(
        session
            .snapshot()
            .source(source)
            .unwrap()
            .video_filters()
            .is_empty()
    );
    install(
        &mut session,
        source,
        &[VideoEffect::person_background_removal()],
    );
    let input = portrait(false);
    let model_rgb =
        prepare_person_rgb(&input, SourceBlend::LinearSrgb, pure_limits(&input)).unwrap();
    let original_rgb = floats(RGB_FIXTURE, 196608);
    assert!(
        model_rgb
            .iter()
            .zip(original_rgb)
            .all(|(a, b)| (*a - b).abs() <= 0.00000003)
    );
    let mut cache = FilterCache::default();
    let output = filtered(
        &mut cache,
        &session,
        source,
        input.clone(),
        SourceBlend::LinearSrgb,
        FREE_BYTES,
        FREE_WORK,
    )
    .unwrap();
    assert_eq!(cache.neural_calls, 1, "must run the real fixed model");
    assert_eq!(output.blend, SourceBlend::LinearSrgb);
    assert_eq!(output.new_bytes, input.byte_len());
    let reference: Vec<_> = floats(ONNX_MASK, 65536).iter().map(|p| *p >= 0.5).collect();
    let actual: Vec<_> = output
        .image
        .pixels()
        .chunks_exact(4)
        .map(|p| p[3] >= 128)
        .collect();
    assert_eq!(reference.iter().filter(|v| **v).count(), 18317);
    assert_eq!(bbox(&reference), [39, 22, 203, 255]);
    assert_eq!(bbox(&actual), [39, 22, 203, 255]);
    let intersection = actual
        .iter()
        .zip(&reference)
        .filter(|(a, b)| **a && **b)
        .count();
    let union = actual
        .iter()
        .zip(&reference)
        .filter(|(a, b)| **a || **b)
        .count();
    let iou = intersection as f64 / union as f64;
    assert!(
        iou >= 0.999,
        "real NN/independent ONNX threshold0.5 IoU={iou}"
    );
    for (a, b) in input
        .pixels()
        .chunks_exact(4)
        .zip(output.image.pixels().chunks_exact(4))
    {
        assert_eq!(&a[..3], &b[..3], "straight source RGB must remain exact");
    }
}

#[test]
fn real_person_output_preserves_existing_alpha_and_matches_pure_application() {
    let _serial = REAL_INFERENCE.lock().unwrap();
    let (mut session, source) = owner();
    enable(&mut session, true);
    install(
        &mut session,
        source,
        &[VideoEffect::PersonBackgroundRemoval {
            threshold: 0.57,
            softness: 0.73,
        }],
    );
    let input = portrait(true);
    let before = input.clone();
    let limits = pure_limits(&input);
    let rgb = prepare_person_rgb(&input, SourceBlend::LinearSrgb, limits).unwrap();
    let inference = skylive_person_matte::evaluate_wait(&rgb).unwrap();
    let expected = apply_person_matte(
        &input,
        SourceBlend::LinearSrgb,
        &inference.probabilities,
        0.57,
        0.73,
        limits,
    )
    .unwrap();
    let mut cache = FilterCache::default();
    let actual = filtered(
        &mut cache,
        &session,
        source,
        input.clone(),
        SourceBlend::LinearSrgb,
        FREE_BYTES,
        FREE_WORK,
    )
    .unwrap();
    assert_eq!(cache.neural_calls, 1);
    assert_eq!(actual.image.pixels(), expected.pixels());
    let mut attenuated_visible = 0;
    for (a, b) in input
        .pixels()
        .chunks_exact(4)
        .zip(actual.image.pixels().chunks_exact(4))
    {
        assert_eq!(&a[..3], &b[..3]);
        assert!(b[3] <= a[3], "matte may only attenuate existing alpha");
        if a[3] == 0 {
            assert_eq!(b[3], 0);
        }
        if b[3] > 0 && b[3] < a[3] {
            attenuated_visible += 1;
        }
    }
    assert!(
        attenuated_visible > 100,
        "real portrait must exercise soft existing alpha"
    );
    assert_eq!(input, before);
}

#[test]
fn real_person_provider_and_filter_bypass_retain_settings_through_native8_reopen() {
    let _serial = REAL_INFERENCE.lock().unwrap();
    let (mut session, source) = owner();
    enable(&mut session, true);
    install(
        &mut session,
        source,
        &[VideoEffect::PersonBackgroundRemoval {
            threshold: 0.71,
            softness: 0.24,
        }],
    );
    let input = portrait(false);
    let mut cache = FilterCache::default();
    let first = filtered(
        &mut cache,
        &session,
        source,
        input.clone(),
        SourceBlend::LinearSrgb,
        FREE_BYTES,
        FREE_WORK,
    )
    .unwrap();
    let authored = session
        .snapshot()
        .source(source)
        .unwrap()
        .video_filters()
        .to_vec();
    let raw_settings = serde_json::to_value(&authored).unwrap();
    enable(&mut session, false);
    let bypass = filtered(
        &mut cache,
        &session,
        source,
        input.clone(),
        SourceBlend::LinearSrgb,
        0,
        0,
    )
    .unwrap();
    assert!(Arc::ptr_eq(&bypass.image, &input));
    assert_eq!(
        (bypass.new_bytes, bypass.pixel_stages, cache.neural_calls),
        (0, 0, 1)
    );
    assert_eq!(
        session.snapshot().source(source).unwrap().video_filters(),
        authored
    );
    let native = crate::storage::encode(&session.snapshot()).unwrap();
    let json: serde_json::Value = serde_json::from_slice(&native).unwrap();
    assert_eq!(json["schema"], crate::storage::CURRENT_SCHEMA);
    let mut reopened = crate::storage::decode(&native, true).unwrap();
    assert!(
        !reopened
            .snapshot()
            .preferences()
            .plugin_enabled(PERSON_BACKGROUND_PLUGIN)
    );
    assert_eq!(
        serde_json::to_value(reopened.snapshot().source(source).unwrap().video_filters()).unwrap(),
        raw_settings
    );
    let mut restored_cache = FilterCache::default();
    let off = filtered(
        &mut restored_cache,
        &reopened,
        source,
        input.clone(),
        SourceBlend::LinearSrgb,
        0,
        0,
    )
    .unwrap();
    assert!(Arc::ptr_eq(&off.image, &input));
    assert_eq!(restored_cache.neural_calls, 0);
    enable(&mut reopened, true);
    let restored = filtered(
        &mut restored_cache,
        &reopened,
        source,
        input.clone(),
        SourceBlend::LinearSrgb,
        FREE_BYTES,
        FREE_WORK,
    )
    .unwrap();
    assert_eq!(restored.image.pixels(), first.image.pixels());
    assert_eq!(restored_cache.neural_calls, 1);
    let mut bypassed = reopened
        .snapshot()
        .source(source)
        .unwrap()
        .video_filters()
        .to_vec();
    bypassed[0].enabled = false;
    reopened
        .apply(Command::SetSourceVideoFilters {
            source,
            filters: bypassed.clone(),
        })
        .unwrap();
    let bypass = filtered(
        &mut restored_cache,
        &reopened,
        source,
        input.clone(),
        SourceBlend::LinearSrgb,
        0,
        0,
    )
    .unwrap();
    assert!(Arc::ptr_eq(&bypass.image, &input));
    assert_eq!(restored_cache.neural_calls, 1);
    assert_eq!(
        reopened.snapshot().source(source).unwrap().video_filters(),
        bypassed
    );
    reopened.apply(Command::Undo).unwrap();
    let restored_again = filtered(
        &mut restored_cache,
        &reopened,
        source,
        input,
        SourceBlend::LinearSrgb,
        0,
        0,
    )
    .unwrap();
    assert!(Arc::ptr_eq(&restored.image, &restored_again.image));
    assert_eq!(restored_cache.neural_calls, 1);
}

fn point(
    input: &RgbaFrame,
    blend: SourceBlend,
    effect: SourceVideoEffect,
) -> skylive_media::SourceEffectFrame {
    skylive_media::source_video_effects(
        input,
        blend,
        &[effect],
        SourceEffectsLimits {
            max_output_bytes: input.byte_len(),
            max_workspace_bytes: input.width() as usize * 48,
            max_pixel_stages: input.byte_len() / 4,
            max_texel_visits: 0,
        },
    )
    .unwrap()
}

fn manual_real_segments(
    input: &RgbaFrame,
    first: SourceVideoEffect,
    last: SourceVideoEffect,
) -> skylive_media::SourceEffectFrame {
    let before = point(input, SourceBlend::LinearSrgb, first);
    let limits = pure_limits(&before.image);
    let rgb = prepare_person_rgb(&before.image, before.blend, limits).unwrap();
    let inference = skylive_person_matte::evaluate_wait(&rgb).unwrap();
    let person = apply_person_matte(
        &before.image,
        before.blend,
        &inference.probabilities,
        0.43,
        0.8,
        limits,
    )
    .unwrap();
    point(&person, before.blend, last)
}

#[test]
fn real_person_segments_preserve_craft_nn_obs_order_and_blend_contract() {
    let _serial = REAL_INFERENCE.lock().unwrap();
    let input = portrait(false);
    let craft = VideoEffect::BrightnessContrast {
        brightness: 20.0,
        contrast: 3.0,
        legacy: false,
    };
    let craft_pixel = SourceVideoEffect::Craft(CraftVideoEffect::BrightnessContrast {
        brightness: 20.0,
        contrast: 3.0,
        legacy: false,
    });
    let correction = ColorCorrectionSettings {
        gamma: 0.2,
        contrast: 0.1,
        brightness: 0.05,
        opacity: 0.7,
        ..Default::default()
    };
    let obs = VideoEffect::ColorCorrection {
        gamma: correction.gamma,
        contrast: correction.contrast,
        brightness: correction.brightness,
        saturation: correction.saturation,
        hue_shift: correction.hue_shift,
        opacity: correction.opacity,
        color_multiply: correction.color_multiply,
        color_add: correction.color_add,
    };
    let obs_pixel = SourceVideoEffect::ColorCorrection(correction);
    let person = VideoEffect::PersonBackgroundRemoval {
        threshold: 0.43,
        softness: 0.8,
    };
    let mut outputs = Vec::new();
    for (chain, a, b) in [
        (vec![craft, person, obs], craft_pixel, obs_pixel),
        (vec![obs, person, craft], obs_pixel, craft_pixel),
    ] {
        let expected = manual_real_segments(&input, a, b);
        let (mut session, source) = owner();
        enable(&mut session, true);
        install(&mut session, source, &chain);
        let mut cache = FilterCache::default();
        let actual = filtered(
            &mut cache,
            &session,
            source,
            input.clone(),
            SourceBlend::LinearSrgb,
            FREE_BYTES,
            FREE_WORK,
        )
        .unwrap();
        assert_eq!(cache.neural_calls, 1);
        assert_eq!(actual.blend, expected.blend);
        assert_eq!(
            actual.image.pixels(),
            expected.image.pixels(),
            "ordered real NN segments must match manual media calls"
        );
        outputs.push(actual.image);
    }
    assert_ne!(
        outputs[0].pixels(),
        outputs[1].pixels(),
        "fixture must distinguish different filter order"
    );
}

#[test]
fn real_person_shared_budget_rejects_before_any_inference_or_cache_publication() {
    let (mut session, source) = owner();
    enable(&mut session, true);
    install(
        &mut session,
        source,
        &[VideoEffect::person_background_removal()],
    );
    let input = portrait(false);
    let work = 5 * 65536 + 5 * (input.byte_len() / 4);
    let peak = 2 * input.byte_len() + (196608 + 65536) * 4;
    for scenario in 0..4 {
        let mut cache = FilterCache::default();
        let (bytes, visits) = match scenario {
            0 => (peak - 1, work),
            1 => (peak, work - 1),
            2 => {
                cache.texel_visits = MAX_TEXEL_VISITS - work as u64 + 1;
                (peak, work)
            }
            3 => {
                cache.bytes = MAX_FILTERED_BYTES - input.byte_len() + 1;
                (peak, work)
            }
            _ => unreachable!(),
        };
        let prior = (cache.bytes, cache.texel_visits);
        let error = filtered(
            &mut cache,
            &session,
            source,
            input.clone(),
            SourceBlend::LinearSrgb,
            bytes,
            visits,
        )
        .err()
        .expect("budget must reject");
        assert_eq!(error.kind, crate::resource_issue::ResourceIssueKind::Limit);
        assert_eq!(
            cache.neural_calls, 0,
            "scenario{scenario} must reject before evaluate_wait"
        );
        assert!(cache.entries.is_empty());
        assert_eq!((cache.bytes, cache.texel_visits), prior);
    }
}

#[test]
fn real_person_cache_identity_includes_both_float_settings_ingress_and_new_frame() {
    let _serial = REAL_INFERENCE.lock().unwrap();
    let (mut session, source) = owner();
    enable(&mut session, true);
    let effect = |threshold, softness| VideoEffect::PersonBackgroundRemoval {
        threshold,
        softness,
    };
    install(&mut session, source, &[effect(0.5, 0.75)]);
    let input = portrait(false);
    let mut cache = FilterCache::default();
    let first = filtered(
        &mut cache,
        &session,
        source,
        input.clone(),
        SourceBlend::LinearSrgb,
        FREE_BYTES,
        FREE_WORK,
    )
    .unwrap();
    let cached = filtered(
        &mut cache,
        &session,
        source,
        input.clone(),
        SourceBlend::LinearSrgb,
        0,
        0,
    )
    .unwrap();
    assert!(Arc::ptr_eq(&first.image, &cached.image));
    assert_eq!(
        (cached.new_bytes, cached.pixel_stages, cache.neural_calls),
        (0, 0, 1)
    );
    let threshold = f32::from_bits(0.5_f32.to_bits() + 1);
    install(&mut session, source, &[effect(threshold, 0.75)]);
    let changed_threshold = filtered(
        &mut cache,
        &session,
        source,
        input.clone(),
        SourceBlend::LinearSrgb,
        FREE_BYTES,
        FREE_WORK,
    )
    .unwrap();
    assert!(!Arc::ptr_eq(&first.image, &changed_threshold.image));
    assert_eq!(cache.neural_calls, 2);
    install(
        &mut session,
        source,
        &[effect(threshold, f32::from_bits(0.75_f32.to_bits() + 1))],
    );
    let changed_softness = filtered(
        &mut cache,
        &session,
        source,
        input.clone(),
        SourceBlend::LinearSrgb,
        FREE_BYTES,
        FREE_WORK,
    )
    .unwrap();
    assert!(!Arc::ptr_eq(
        &changed_threshold.image,
        &changed_softness.image
    ));
    assert_eq!(cache.neural_calls, 3);
    let changed_ingress = filtered(
        &mut cache,
        &session,
        source,
        input.clone(),
        SourceBlend::Encoded,
        FREE_BYTES,
        FREE_WORK,
    )
    .unwrap();
    assert!(!Arc::ptr_eq(
        &changed_softness.image,
        &changed_ingress.image
    ));
    assert_eq!(changed_ingress.blend, SourceBlend::Encoded);
    assert_eq!(cache.neural_calls, 4);
    let changed_frame = Arc::new((*input).clone());
    let fresh = filtered(
        &mut cache,
        &session,
        source,
        changed_frame,
        SourceBlend::Encoded,
        FREE_BYTES,
        FREE_WORK,
    )
    .unwrap();
    assert!(!Arc::ptr_eq(&changed_ingress.image, &fresh.image));
    assert_eq!(cache.neural_calls, 5);
}

#[test]
fn real_person_runtime_failure_is_unavailable_without_silent_bypass_or_cached_pixels() {
    let _serial = REAL_INFERENCE.lock().unwrap();
    let (mut session, source) = owner();
    enable(&mut session, true);
    install(
        &mut session,
        source,
        &[VideoEffect::person_background_removal()],
    );
    let input = portrait(false);
    let mut cache = FilterCache::default();
    let detail = "ModelLoad fixture path=C:\\models\\selfie256.rten code=42";
    cache.neural_error = Some(skylive_person_matte::Error::ModelLoad(detail.into()));
    let error = filtered(
        &mut cache,
        &session,
        source,
        input.clone(),
        SourceBlend::LinearSrgb,
        FREE_BYTES,
        FREE_WORK,
    )
    .err()
    .expect("runtime failure must not become unfiltered success");
    assert_eq!(
        error.kind,
        crate::resource_issue::ResourceIssueKind::Unavailable
    );
    assert_eq!(error.code, "person_background_unavailable");
    assert_eq!(error.detail, detail);
    assert_eq!(cache.neural_calls, 1);
    assert!(
        cache.neural_error.is_none(),
        "fault is local and consumed once"
    );
    assert!(cache.entries.is_empty());
    assert_eq!(cache.bytes, 0);
    let work = (5 * 65536 + 5 * (input.byte_len() / 4)) as u64;
    assert_eq!(
        cache.texel_visits, work,
        "admitted failed execution stays charged"
    );
    // No global shutdown or fake replacement mask. A fresh call through the
    // same host owner now executes the actual fixed-model backend.
    let actual = filtered(
        &mut cache,
        &session,
        source,
        input.clone(),
        SourceBlend::LinearSrgb,
        FREE_BYTES,
        FREE_WORK,
    )
    .unwrap();
    assert_eq!(cache.neural_calls, 2);
    assert_eq!(cache.entries.len(), 1);
    assert_eq!(cache.bytes, input.byte_len());
    assert_eq!(cache.texel_visits, work * 2);
    assert!(!Arc::ptr_eq(&actual.image, &input));
    assert!(
        actual
            .image
            .pixels()
            .chunks_exact(4)
            .any(|pixel| pixel[3] == 0)
    );
    assert!(
        actual
            .image
            .pixels()
            .chunks_exact(4)
            .any(|pixel| pixel[3] >= 128)
    );
}

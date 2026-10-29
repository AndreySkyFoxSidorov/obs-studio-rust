//! Explicit CPU host-pipeline benchmark; never runs in ordinary test execution.
use super::*;
use image::{RgbaImage, imageops};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use skylive_core::{Command, PERSON_BACKGROUND_PLUGIN, Session, VideoFilter};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::PathBuf,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

const PORTRAIT: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../person-matte/tests/data/portrait-input.f32"
));
const MODEL: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../person-matte/models/selfie-segmentation.rten"
));
const HOST_PIPELINE: &[u8] = include_bytes!("../video_filters.rs");
const PERSON_PIPELINE: &[u8] = include_bytes!("person_background.rs");
const PIXEL_PIPELINE: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../media/src/person_matte.rs"
));
const FREE_BYTES: usize = 64 * 1024 * 1024;
const FREE_WORK: usize = 64 * 1024 * 1024;
const MEASURED: usize = 3;
const EVALUATIONS: usize = MEASURED + 1;

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn portrait() -> RgbaImage {
    assert_eq!(PORTRAIT.len(), 256 * 256 * 3 * 4);
    let mut rgba = Vec::with_capacity(256 * 256 * 4);
    for source in PORTRAIT.chunks_exact(12) {
        for channel in source.chunks_exact(4) {
            let value = f32::from_le_bytes(channel.try_into().unwrap());
            assert!(value.is_finite() && (0.0..=1.0).contains(&value));
            rgba.push((value * 255.0).round() as u8);
        }
        rgba.push(255);
    }
    RgbaImage::from_raw(256, 256, rgba).unwrap()
}

fn fixture_owner(mixed: bool) -> (Session, skylive_core::Id) {
    let mut session = Session::new();
    session
        .apply(Command::AddColorSource {
            scene: session.preview(),
            name: "Benchmark filter owner; portrait pixels injected at FilterCache".into(),
            rgba: [255; 4],
        })
        .unwrap();
    let source = session.snapshot().sources()[0].id();
    assert!(
        session
            .snapshot()
            .source(source)
            .unwrap()
            .video_filters()
            .is_empty()
    );
    assert!(
        !session
            .snapshot()
            .preferences()
            .plugin_enabled(PERSON_BACKGROUND_PLUGIN)
    );
    session
        .apply(Command::SetPluginEnabled {
            plugin: PERSON_BACKGROUND_PLUGIN.into(),
            enabled: true,
        })
        .unwrap();
    let mut filters = Vec::new();
    if mixed {
        filters.push(
            VideoFilter::new(VideoEffect::ColorCorrection {
                gamma: 0.2,
                contrast: 0.1,
                brightness: 0.025,
                saturation: 0.1,
                hue_shift: 5.0,
                opacity: 1.0,
                color_multiply: [255, 247, 239],
                color_add: [0, 0, 0],
            })
            .unwrap(),
        );
    }
    filters.push(VideoFilter::new(VideoEffect::person_background_removal()).unwrap());
    session
        .apply(Command::SetSourceVideoFilters { source, filters })
        .unwrap();
    (session, source)
}

fn run_case(input: &RgbaFrame, blend: SourceBlend, blend_name: &str, mixed: bool) -> Value {
    let (session, source) = fixture_owner(mixed);
    let model = session.snapshot();
    let owner = model.source(source).unwrap();
    let providers = FilterProviders::from_snapshot(&model);
    let native = crate::storage::encode(&model).unwrap();
    // Each frame owns distinct bytes. All deep copies and cache construction
    // occur outside the timed host calls; no cache/output is reused.
    let frames: Vec<_> = (0..EVALUATIONS).map(|_| Arc::new(input.clone())).collect();
    for pair in frames.windows(2) {
        assert_ne!(pair[0].pixels().as_ptr(), pair[1].pixels().as_ptr());
    }
    let pixels = input.byte_len() / 4;
    let expected_work = 5 * 65536 + 5 * pixels + usize::from(mixed) * pixels;
    assert!(expected_work <= FREE_WORK);
    let input_hash = sha256(input.pixels());
    let mut durations = Vec::with_capacity(MEASURED);
    let mut samples = Vec::with_capacity(MEASURED);
    let mut warmup = Value::Null;
    let mut expected_output_hash: Option<String> = None;
    for (iteration, frame) in frames.into_iter().enumerate() {
        let mut cache = FilterCache::default();
        let began = Instant::now();
        let output =
            cache.apply_with_providers(owner, frame, blend, providers, FREE_BYTES, FREE_WORK);
        let elapsed = began.elapsed();
        let output =
            output.unwrap_or_else(|error| panic!("real neural benchmark failed: {error:?}"));
        // Hashes, inspection and destruction deliberately stay outside the clock.
        let host_ms = elapsed.as_secs_f64() * 1000.0;
        assert!(
            host_ms.is_finite() && host_ms > 0.0,
            "host clock must record positive finite elapsed time"
        );
        let output_hash = sha256(output.image.pixels());
        assert_eq!(
            cache.neural_calls, 1,
            "must evaluate the actual model on every new frame"
        );
        assert_eq!(cache.entries.len(), 1);
        assert_eq!(output.new_bytes, input.byte_len());
        assert_eq!(output.pixel_stages, expected_work);
        assert_eq!(
            (output.image.width(), output.image.height()),
            (input.width(), input.height())
        );
        if let Some(expected) = &expected_output_hash {
            assert_eq!(
                &output_hash, expected,
                "same real fixture must produce stable output bytes"
            );
        } else {
            expected_output_hash = Some(output_hash.clone());
        }
        let record = json!({
            "host_call_ms": host_ms,
            "host_call_ns": u64::try_from(elapsed.as_nanos()).unwrap(),
            "output_rgba_sha256": output_hash,
            "output_blend": format!("{:?}", output.blend),
            "output_bytes": output.new_bytes,
            "pixel_stages_charged": output.pixel_stages,
            "texel_visits_charged": cache.texel_visits,
            "actual_neural_calls": cache.neural_calls,
            "cache_entries": cache.entries.len(),
        });
        if iteration == 0 {
            warmup = record;
        } else {
            durations.push(host_ms);
            samples.push(record);
        }
        drop(output);
        drop(cache);
    }
    durations.sort_by(f64::total_cmp);
    let mean = durations.iter().sum::<f64>() / MEASURED as f64;
    let median = durations[MEASURED / 2];
    json!({
        "width": input.width(), "height": input.height(),
        "ingress": blend_name,
        "chain": if mixed { "obs_color_correction_then_person" } else { "person_only" },
        "source_owner_id": source,
        "source_owner_kind": owner.kind(),
        "source_owner_is_standin": true,
        "source_revision": model.revision(),
        "native_document_sha256": sha256(&native),
        "authored_filters": owner.video_filters(),
        "input_rgba_sha256": input_hash,
        "input_bytes": input.byte_len(),
        "input_alpha": "opaque255; identical valid stored bytes in all tested policies",
        "warmup": warmup,
        "measured_frames": MEASURED,
        "samples": samples,
        "host_min_ms": durations[0], "host_median_ms": median,
        "host_max_ms": durations[MEASURED-1], "host_mean_ms": mean,
        "host_cpu_filter_evaluations_per_second_from_median": 1000.0 / median,
        "rate_scope": "inverse synthetic CPU FilterCache duration; excludes capture/composite/encode/presentation",
    })
}

#[test]
#[ignore = "explicit opt-in Release CPU neural host benchmark; no GUI/capture/devices"]
fn real_person_filter_performance_release() {
    if cfg!(debug_assertions) {
        panic!("run this ignored performance test with cargo test --release");
    }
    // Match the actual persistent Preview/Program/recording/streaming worker
    // lifetime instead of retiring the model after each direct FilterCache call.
    let _render_services = skylive_person_matte::retain_background_owner().unwrap();
    let output_dir = std::env::var_os("SKYLIVE_PERSON_PERFORMANCE_DIR")
        .expect("set SKYLIVE_PERSON_PERFORMANCE_DIR to a new evidence directory");
    assert!(
        !output_dir.is_empty(),
        "SKYLIVE_PERSON_PERFORMANCE_DIR must not be empty"
    );
    let output_dir = PathBuf::from(output_dir);
    fs::create_dir_all(&output_dir).unwrap();
    let output_path = output_dir.join("person-filter-performance-v1.json");
    assert!(
        !output_path.exists(),
        "benchmark evidence already exists; use a new directory"
    );
    let started = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    assert_eq!(
        sha256(PORTRAIT),
        "1a915a5073d37cb9cadf853b69b3946d884afb5770fee7387bf09177aae13089"
    );
    let model_hash = sha256(MODEL);
    assert_eq!(model_hash, skylive_person_matte::MODEL_SHA256);
    let original = portrait();
    // Pure fixture stretch-resize only. Native capture and source acquisition
    // are intentionally outside this isolated filter benchmark.
    let fixtures: Vec<_> = [(640, 480), (1920, 1080)]
        .into_iter()
        .map(|(width, height)| {
            let resized =
                imageops::resize(&original, width, height, imageops::FilterType::Triangle);
            assert!(resized.pixels().all(|pixel| pixel[3] == 255));
            RgbaFrame::new(width, height, resized.into_raw()).unwrap()
        })
        .collect();
    let mut cases = Vec::with_capacity(12);
    for input in &fixtures {
        for (blend, name) in [
            (SourceBlend::LinearSrgb, "straight_linear_srgb"),
            (SourceBlend::Encoded, "straight_encoded"),
            (
                SourceBlend::PremultipliedLinearSrgb,
                "premultiplied_linear_srgb",
            ),
        ] {
            for mixed in [false, true] {
                cases.push(run_case(input, blend, name, mixed));
            }
        }
    }
    assert_eq!(cases.len() * EVALUATIONS, 48);
    let report = json!({
        "schema": "skylive.person-filter-performance.v1",
        "scope": "actual CPU FilterCache + fixed-model inference on injected real portrait; no GUI, device, camera, GPU, composite or encoder measurement",
        "build": { "debug_assertions": cfg!(debug_assertions), "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH, "host_package_version": env!("CARGO_PKG_VERSION"),
            "declared_minimum_rust_version": env!("CARGO_PKG_RUST_VERSION") },
        "began_unix_ms": u64::try_from(started.as_millis()).unwrap(),
        "native_schema": crate::storage::CURRENT_SCHEMA,
        "model": { "id": skylive_person_matte::MODEL_ID, "sha256": model_hash,
            "original_onnx_sha256": skylive_person_matte::ORIGINAL_ONNX_SHA256,
            "runtime_thread_limit": skylive_person_matte::THREAD_LIMIT,
            "model_only_timing": "not observed at FilterCache boundary; do not infer from host totals" },
        "source_sha256": { "host_filter_cache": sha256(HOST_PIPELINE),
            "host_person_pipeline": sha256(PERSON_PIPELINE), "media_person_pixels": sha256(PIXEL_PIPELINE) },
        "fixture": { "path": "crates/person-matte/tests/data/portrait-input.f32",
            "sha256": sha256(PORTRAIT), "tensor_shape": [1,256,256,3],
            "encoding": "little-endian f32 encoded RGB/255",
            "resize": "image0.25.10 Triangle stretch from opaque256x256; outside measured clock" },
        "clock_includes": "actual host settings/admission/cache insertion, point effects, RGB preprocessing, real NN inference and full-resolution matte application",
        "clock_excludes": "fixture conversion/resize/deep copies, SHA/statistics/assertions, serialization/file I/O and final output/cache destruction",
        "warmup_scope": "one call per case; global model may already be initialized; no shutdown or cold-start claim",
        "limits_per_call": { "free_output_scratch_bytes": FREE_BYTES, "free_pixel_work": FREE_WORK },
        "total_evaluations": cases.len() * EVALUATIONS,
        "cases": cases,
    });
    let mut bytes = serde_json::to_vec_pretty(&report).unwrap();
    bytes.push(b'\n');
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&output_path)
        .unwrap();
    output.write_all(&bytes).unwrap();
    output.sync_all().unwrap();
    eprintln!("real CPU person-filter evidence: {}", output_path.display());
}

//! Run as a separate process to include the one-time transfer lookup build.
//! cargo run --release -p skylive-media --example chroma_lookup_cold
use skylive_media::{
    ChromaKeySettings, RgbaFrame, SourceBlend, SourceEffectsLimits, SourceVideoEffect,
    source_effect_requirements, source_video_effects,
};
use std::{hint::black_box, time::Instant};

fn main() {
    let input = RgbaFrame::new(1, 1, vec![217, 81, 19, 255]).unwrap();
    let effects = [SourceVideoEffect::ChromaKey(ChromaKeySettings::default())];
    let required = source_effect_requirements(&input, &effects).unwrap();
    let limits = SourceEffectsLimits {
        max_output_bytes: required.output_bytes,
        max_workspace_bytes: required.workspace_bytes,
        max_pixel_stages: required.pixel_stages,
        max_texel_visits: required.texel_visits,
    };
    let start = Instant::now();
    let output = source_video_effects(
        black_box(&input),
        SourceBlend::LinearSrgb,
        black_box(&effects),
        limits,
    )
    .unwrap();
    let cold_ms = start.elapsed().as_secs_f64() * 1000.0;
    // This is the declared logical LUT payload, not an allocator/RSS measure.
    println!(
        "{{\"fixture\":\"1x1-opaque\",\"cold_ms\":{cold_ms:.6},\"lookup_payload_bytes\":262144,\"workspace_bytes\":{},\"output\":{:?}}}",
        required.workspace_bytes,
        black_box(output.image.pixels()),
    );
}

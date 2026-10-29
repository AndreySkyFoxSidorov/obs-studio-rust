//! Run before and after integration with the same release toolchain and host.
//! cargo run --release -p skylive-media --example filter_performance -- --label baseline --frames 5
use skylive_media::{
    ChromaKeySettings, ColorCorrectionSettings, RgbaFrame, SourceBlend, SourceEffectsLimits,
    SourceVideoEffect, source_effect_requirements, source_video_effects,
};
use std::{hint::black_box, time::Instant};

fn fixture(width: u32, height: u32, frame: u32, noisy: bool) -> RgbaFrame {
    let mut pixels = Vec::with_capacity(width as usize * height as usize * 4);
    let center = width / 4 + (frame * 29) % (width / 2);
    let mut noise = 0x8b5a_273du32 ^ frame;
    for y in 0..height {
        for x in 0..width {
            noise ^= noise << 13;
            noise ^= noise >> 17;
            noise ^= noise << 5;
            let foreground = x.abs_diff(center) < width / 8 && y.abs_diff(height / 2) < height / 3;
            let mut p = if foreground {
                [
                    (x * 37 + y * 11) as u8,
                    (x * 5 + y * 13) as u8,
                    (x * 17 + y * 7) as u8,
                    255,
                ]
            } else {
                [
                    ((x + y) % 13) as u8,
                    220 + ((x * 3 + y * 7) % 36) as u8,
                    ((x * 7 + y) % 19) as u8,
                    255,
                ]
            };
            if noisy {
                p = [
                    noise as u8,
                    (noise >> 8) as u8,
                    (noise >> 16) as u8,
                    [0, 1, 128, 254, 255][(x + y) as usize % 5],
                ];
            }
            pixels.extend_from_slice(&p);
        }
    }
    RgbaFrame::new(width, height, pixels).unwrap()
}

fn hash(bytes: &[u8], mut state: u64) -> u64 {
    for &byte in bytes {
        state = (state ^ u64::from(byte)).wrapping_mul(0x100_0000_01b3);
    }
    state
}

fn main() {
    let args: Vec<_> = std::env::args().collect();
    let value = |name: &str| {
        args.windows(2)
            .find(|p| p[0] == name)
            .map(|p| p[1].as_str())
    };
    let label = value("--label").unwrap_or("current");
    let frames: u32 = value("--frames").unwrap_or("5").parse().unwrap();
    assert!((1..=120).contains(&frames));
    println!("label,width,height,fixture,encoding,chain,frames,total_ms,ms_per_frame,fps,hash");
    for (width, height) in [(640, 480), (1920, 1080)] {
        for noisy in [false, true] {
            let inputs: Vec<_> = (0..frames)
                .map(|n| fixture(width, height, n, noisy))
                .collect();
            for (encoding_name, ingress) in [
                ("linear", SourceBlend::LinearSrgb),
                ("encoded", SourceBlend::Encoded),
                ("premultiplied", SourceBlend::PremultipliedLinearSrgb),
            ] {
                for chained in [false, true] {
                    let mut effects =
                        vec![SourceVideoEffect::ChromaKey(ChromaKeySettings::default())];
                    if chained {
                        effects.insert(
                            0,
                            SourceVideoEffect::ColorCorrection(ColorCorrectionSettings {
                                gamma: 0.31,
                                contrast: 0.13,
                                brightness: 0.02,
                                ..Default::default()
                            }),
                        );
                        effects.push(SourceVideoEffect::ChromaKey(ChromaKeySettings {
                            gamma: -0.27,
                            similarity: 130,
                            smoothness: 370,
                            spill: 280,
                            ..Default::default()
                        }));
                    }
                    let required = source_effect_requirements(&inputs[0], &effects).unwrap();
                    let limits = SourceEffectsLimits {
                        max_output_bytes: required.output_bytes,
                        max_workspace_bytes: required.workspace_bytes,
                        max_pixel_stages: required.pixel_stages,
                        max_texel_visits: required.texel_visits,
                    };
                    black_box(source_video_effects(&inputs[0], ingress, &effects, limits).unwrap());
                    let mut elapsed = std::time::Duration::ZERO;
                    let mut digest = 0xcbf2_9ce4_8422_2325;
                    for input in &inputs {
                        let start = Instant::now();
                        let output = source_video_effects(
                            black_box(input),
                            ingress,
                            black_box(&effects),
                            limits,
                        )
                        .unwrap();
                        elapsed += start.elapsed();
                        digest = hash(black_box(output.image.pixels()), digest);
                    }
                    let ms = elapsed.as_secs_f64() * 1000.0;
                    println!(
                        "{label},{width},{height},{},{encoding_name},{},{frames},{ms:.3},{:.3},{:.3},{digest:016x}",
                        if noisy { "noise-alpha" } else { "moving-green" },
                        if chained {
                            "correction-chroma-chroma"
                        } else {
                            "chroma"
                        },
                        ms / f64::from(frames),
                        f64::from(frames) / elapsed.as_secs_f64()
                    );
                }
            }
        }
    }
}

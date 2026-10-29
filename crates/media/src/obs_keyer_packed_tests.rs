//! Compare disjoint striped execution with the independently frozen renderer.
use super::*;
use crate::obs_keyer::packed::{TestMode, test_apply};

// Forced scoped fixtures share production admission. Serialize those fixtures
// rather than asserting availability while another fixture occupies both slots.
static SCOPED_FIXTURES: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn boundary_fixture(width: usize, height: usize, opaque: bool) -> Vec<u8> {
    let mut input = pixels(width, height, 0x813f_2bc7);
    let boundary = height / 2;
    for y in [boundary.saturating_sub(1), boundary.min(height - 1)] {
        for x in 0..width {
            let alpha = if opaque {
                255
            } else {
                [0, 1, 128, 254, 255][x % 5]
            };
            let pixel = if y < boundary {
                [255, (x * 37) as u8, 0, alpha]
            } else {
                [(x * 17) as u8, 255, (x * 73) as u8, alpha]
            };
            input[(y * width + x) * 4..(y * width + x + 1) * 4].copy_from_slice(&pixel);
        }
    }
    if opaque {
        for pixel in input.chunks_exact_mut(4) {
            pixel[3] = 255;
        }
    }
    input
}

fn reference(
    input: &[u8],
    width: usize,
    height: usize,
    encoding: ColorCorrectionInput,
    settings: ChromaKeySettings,
) -> Vec<u8> {
    let mut output = input.to_vec();
    baseline_keyer::chroma_in_place(
        &mut output,
        width,
        height,
        encoding,
        baseline_keyer::PreparedKeyer::chroma(settings),
        &mut vec![[0.0; 4]; width * 3],
    );
    output
}

fn checked(
    input: &[u8],
    expected: &[u8],
    width: usize,
    height: usize,
    encoding: ColorCorrectionInput,
    settings: ChromaKeySettings,
    mode: TestMode,
) {
    const CANARY: u32 = 0xdea1_beef;
    let mut actual = input.to_vec();
    let mut workspace = vec![CANARY; width * 12 + 7];
    test_apply(
        &mut actual,
        width,
        height,
        encoding,
        PreparedKeyer::chroma(settings),
        &mut workspace,
        mode,
    )
    .unwrap();
    assert_eq!(
        actual, expected,
        "{width}x{height} {mode:?} {encoding:?} {settings:?}"
    );
    assert!(workspace[width * 12..].iter().all(|word| *word == CANARY));
    assert_eq!(width * 12 * std::mem::size_of::<u32>(), width * 48);
}

#[test]
fn copied_halos_match_frozen_bytes_in_both_stripe_orders_at_thin_odd_borders() {
    for (width, height) in [
        (1, 2),
        (1, 19),
        (2, 2),
        (2, 13),
        (23, 2),
        (3, 7),
        (31, 23),
        (193, 37),
    ] {
        for opaque in [false, true] {
            let input = boundary_fixture(width, height, opaque);
            for encoding in ENCODINGS {
                for settings in chroma_settings() {
                    let expected = reference(&input, width, height, encoding, settings);
                    for mode in [TestMode::SequentialStripes, TestMode::ReverseStripes] {
                        checked(&input, &expected, width, height, encoding, settings, mode);
                    }
                }
            }
        }
    }
}

#[test]
fn real_scoped_helpers_match_the_frozen_renderer_with_global_permits() {
    let _guard = SCOPED_FIXTURES.lock().unwrap();
    let settings = chroma_settings();
    for (width, height) in [(1, 2), (17, 3), (63, 18), (193, 37)] {
        for opaque in [false, true] {
            let input = boundary_fixture(width, height, opaque);
            for encoding in ENCODINGS {
                for settings in [settings[0], settings[3], settings[4]] {
                    let expected = reference(&input, width, height, encoding, settings);
                    checked(
                        &input,
                        &expected,
                        width,
                        height,
                        encoding,
                        settings,
                        TestMode::Scoped,
                    );
                }
            }
        }
    }
}

#[test]
fn busy_and_rejected_spawn_fallbacks_complete_the_same_original_frame() {
    let _guard = SCOPED_FIXTURES.lock().unwrap();
    let (width, height) = (37, 11);
    for opaque in [false, true] {
        let input = boundary_fixture(width, height, opaque);
        for encoding in ENCODINGS {
            for settings in [
                chroma_settings()[0],
                chroma_settings()[3],
                chroma_settings()[4],
            ] {
                let expected = reference(&input, width, height, encoding, settings);
                for mode in [TestMode::Serial, TestMode::Busy, TestMode::SpawnFailure] {
                    checked(&input, &expected, width, height, encoding, settings, mode);
                }
            }
        }
    }
}

#[test]
fn exact_workspace_is_accepted_and_one_missing_word_rejects_before_any_write() {
    let _guard = SCOPED_FIXTURES.lock().unwrap();
    let (width, height) = (17, 7);
    let input = boundary_fixture(width, height, false);
    for mode in [
        TestMode::Serial,
        TestMode::SequentialStripes,
        TestMode::Scoped,
        TestMode::SpawnFailure,
    ] {
        let mut actual = input.clone();
        let mut short = vec![0xdea1_beefu32; width * 12 - 1];
        let original_scratch = short.clone();
        assert_eq!(
            test_apply(
                &mut actual,
                width,
                height,
                ColorCorrectionInput::LinearAlpha,
                PreparedKeyer::chroma(ChromaKeySettings::default()),
                &mut short,
                mode
            ),
            Err(MediaError::InvalidBuffer)
        );
        assert_eq!(actual, input);
        assert_eq!(short, original_scratch);
        let mut exact = vec![0u32; width * 12];
        test_apply(
            &mut actual,
            width,
            height,
            ColorCorrectionInput::LinearAlpha,
            PreparedKeyer::chroma(ChromaKeySettings::default()),
            &mut exact,
            mode,
        )
        .unwrap();
        assert_eq!(
            actual,
            reference(
                &input,
                width,
                height,
                ColorCorrectionInput::LinearAlpha,
                ChromaKeySettings::default()
            )
        );
    }
}

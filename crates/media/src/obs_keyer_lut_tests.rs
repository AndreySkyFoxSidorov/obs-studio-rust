//! These expected expressions intentionally do not call production transfer code.
use super::super::{
    ChromaKeySettings, ColorCorrectionInput, MediaError, PreparedKeyer, chroma_in_place, row_lp,
    sample_lp, sampled_bytes,
};
use super::{PAIR_BYTES, PAIR_ENTRIES, get};

fn original_decode(byte: u8) -> f32 {
    let value = f32::from(byte) / 255.0;
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

fn original_nonlinear(value: f32) -> f32 {
    if value <= 0.0031308 {
        value * 12.92
    } else {
        1.055 * value.powf(1.0 / 2.4) - 0.055
    }
}

#[test]
fn every_ordered_lookup_pair_matches_the_original_expression_bits() {
    let lookup = get().unwrap();
    assert_eq!(PAIR_ENTRIES, 65_536);
    assert_eq!(PAIR_BYTES, 262_144);
    assert_eq!(lookup.values.len(), PAIR_ENTRIES);
    assert_eq!(std::mem::size_of_val(lookup.values.as_ref()), PAIR_BYTES);
    for left in 0..=255u8 {
        for right in 0..=255u8 {
            let expected =
                original_nonlinear((original_decode(left) + original_decode(right)) * 0.5);
            assert_eq!(
                lookup.pair(left, right).to_bits(),
                expected.to_bits(),
                "pair {left},{right}"
            );
        }
    }
}

#[test]
fn every_diagonal_entry_matches_the_original_opaque_center_bits() {
    let lookup = get().unwrap();
    let opaque_alpha = f32::from(255u8) / 255.0;
    for byte in 0..=255u8 {
        // The old center path multiplies sampled LP by reciprocal opaque alpha.
        let expected = original_nonlinear(original_decode(byte) * (1.0 / opaque_alpha));
        assert_eq!(
            lookup.opaque(byte).to_bits(),
            expected.to_bits(),
            "opaque {byte}"
        );
    }
}

#[test]
fn byte_index_rows_recover_every_channel_alpha_and_encoding_exactly() {
    // sample_lp is independently compared with the frozen original in the
    // unchanged performance oracle; this checks the new row representation.
    for alpha in 0..=255u8 {
        for byte in 0..=255u8 {
            let pixel = [byte, byte.wrapping_add(77), 255 - byte, alpha];
            for encoding in [
                ColorCorrectionInput::LinearAlpha,
                ColorCorrectionInput::EncodedAlpha,
                ColorCorrectionInput::PremultipliedLinearSrgb,
            ] {
                let raw = sampled_bytes(pixel, encoding);
                let row = [
                    f32::from(raw[0]),
                    f32::from(raw[1]),
                    f32::from(raw[2]),
                    f32::from(raw[3]) / 255.0,
                ];
                assert_eq!(
                    row_lp(row).map(f32::to_bits),
                    sample_lp(pixel, encoding).map(f32::to_bits),
                    "row {pixel:?} {encoding:?}"
                );
            }
        }
    }
}

#[test]
fn malformed_shape_and_scratch_reject_before_rewriting_pixels() {
    let prepared = PreparedKeyer::chroma(ChromaKeySettings::default());
    let fixtures = [
        (0, 1, 3, false),
        (1, 0, 3, false),
        (2, 1, 6, false),
        (1, 1, 2, false),
        (usize::MAX, 1, 0, true),
        (1, usize::MAX, 0, true),
    ];
    for (width, height, scratch, overflow) in fixtures {
        let mut pixels = [17, 201, 83, 255];
        let original = pixels;
        let mut workspace = vec![0u32; scratch * 4];
        let result = chroma_in_place(
            &mut pixels,
            width,
            height,
            ColorCorrectionInput::LinearAlpha,
            prepared,
            &mut workspace,
        );
        let expected_error = if overflow {
            MediaError::Overflow
        } else {
            MediaError::InvalidBuffer
        };
        assert_eq!(
            result,
            Err(expected_error),
            "shape {width}x{height} scratch={scratch}: {result:?}"
        );
        assert_eq!(pixels, original);
    }
}

#[test]
fn concurrent_borrowers_share_the_same_immutable_payload() {
    let addresses = std::thread::scope(|scope| {
        let workers: Vec<_> = (0..4)
            .map(|_| scope.spawn(|| get().unwrap().values.as_ptr() as usize))
            .collect();
        workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert!(addresses.iter().all(|address| *address == addresses[0]));
}

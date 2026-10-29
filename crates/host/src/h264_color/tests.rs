use super::*;
use skylive_codec_bits::{BitWriter, escape_rbsp, unescape_rbsp};
use skylive_h264::params::Sps;

// Real Microsoft64×48 software MFT fixture. Provenance/hashes in module card.
const NATIVE_AVCC: &[u8] = &[
    1, 66, 192, 20, 255, 225, 0, 24, 103, 66, 192, 20, 149, 161, 30, 192, 68, 0, 0, 3, 0, 4, 0, 0,
    3, 0, 240, 54, 130, 33, 26, 128, 1, 0, 4, 104, 206, 56, 128,
];
fn native_sps() -> &'static [u8] {
    &NATIVE_AVCC[8..32]
}
fn parsed(nal: &[u8]) -> Sps {
    Sps::parse(&unescape_rbsp(&nal[1..])).unwrap()
}
fn bits(bytes: &[u8], start: usize, end: usize) -> Vec<bool> {
    (start..end)
        .map(|n| bytes[n / 8] & (1 << (7 - n % 8)) != 0)
        .collect()
}
fn assert_only_color_changed(before: &[u8], after: &[u8]) {
    let mut old = parsed(before);
    let mut new = parsed(after);
    let mut expected = old.vui.take().unwrap_or_default();
    expected.full_range = false;
    expected.colour_primaries = 1;
    expected.transfer_characteristics = 1;
    expected.matrix_coefficients = 1;
    assert_eq!(
        new.vui.take(),
        Some(expected),
        "all aspect/overscan/chroma/timing/HRD/restriction fields survive"
    );
    // Sps derives Debug for every field, including its private scaling lists.
    // This independent decoder comparison therefore covers all parsed coding,
    // geometry, scaling, POC/reference/crop fields, not a handpicked subset.
    assert_eq!(format!("{old:?}"), format!("{new:?}"));
    let before = unescape(&before[1..]).unwrap();
    let after = unescape(&after[1..]).unwrap();
    let a = parse(&before).unwrap();
    let b = parse(&after).unwrap();
    assert_eq!(
        bits(&before, 0, a.replace_start),
        bits(&after, 0, a.replace_start)
    );
    if a.vui_present {
        assert_eq!(
            bits(&before, a.replace_end, a.syntax_end),
            bits(&after, b.replace_end, b.syntax_end)
        );
    }
}

#[test]
fn actual_native_sps_preserves_all_parsed_fields_and_adds_709_limited() {
    let before = parsed(native_sps());
    assert_eq!(before.crop_rect(), (0, 0, 64, 48));
    let original = before.vui.as_ref().unwrap();
    assert_eq!(
        (
            original.colour_primaries,
            original.transfer_characteristics,
            original.matrix_coefficients
        ),
        (2, 2, 2)
    );
    let changed = ensure_bt709_limited_sps(native_sps()).unwrap();
    assert_eq!(changed.changed_sps, 1);
    assert_eq!(changed.origin, MetadataOrigin::RewrittenFromInputContract);
    assert_only_color_changed(native_sps(), &changed.bytes);
    let repeated = ensure_bt709_limited_sps(&changed.bytes).unwrap();
    assert_eq!(repeated.bytes, changed.bytes);
    assert_eq!(repeated.changed_sps, 0);
    assert_eq!(repeated.origin, MetadataOrigin::InputAlreadyMatchesContract);
}
fn hrd_bits(w: &mut BitWriter, variant: u32) {
    w.write_ue(1);
    w.write_bits(variant, 4);
    w.write_bits(2, 4);
    for n in 0..2 {
        w.write_ue(10 + variant + n);
        w.write_ue(20 + n);
        w.write_bit(n == 0);
    }
    for value in [23, 22, 21, 20] {
        w.write_bits(value, 5);
    }
}
fn synthetic(profile: u8, vui: bool, signal: Option<VideoSignal>, hrd: bool) -> Vec<u8> {
    let mut w = BitWriter::new();
    w.write_bits(u32::from(profile), 8);
    w.write_bits(0, 8);
    w.write_bits(30, 8);
    w.write_ue(0);
    if profile == 100 {
        w.write_ue(1);
        w.write_ue(0);
        w.write_ue(0);
        w.write_bit(false);
        w.write_bit(true);
        // All eight scaling lists use their explicit default at the first
        // delta. No missing delta fields are guessed by the rewriter.
        for _ in 0..8 {
            w.write_bit(true);
            w.write_se(-8);
        }
    }
    w.write_ue(0);
    w.write_ue(1);
    w.write_bit(false);
    w.write_se(-2);
    w.write_se(1);
    w.write_ue(2);
    w.write_se(-1);
    w.write_se(3);
    w.write_ue(2);
    w.write_bit(true);
    w.write_ue(3);
    w.write_ue(2);
    w.write_bit(true);
    w.write_bit(true);
    w.write_bit(true);
    for crop in [0, 1, 0, 1] {
        w.write_ue(crop);
    }
    w.write_bit(vui);
    if vui {
        w.write_bit(true);
        w.write_bits(255, 8);
        w.write_bits(16, 16);
        w.write_bits(15, 16);
        w.write_bit(true);
        w.write_bit(true);
        w.write_bit(signal.is_some());
        if let Some(signal) = signal {
            w.write_bits(u32::from(signal.format), 3);
            w.write_bit(signal.full_range);
            w.write_bit(signal.color.is_some());
            if let Some(color) = signal.color {
                for byte in color {
                    w.write_bits(u32::from(byte), 8);
                }
            }
        }
        w.write_bit(true);
        w.write_ue(0);
        w.write_ue(1);
        w.write_bit(true);
        w.write_bits(1001, 32);
        w.write_bits(60000, 32);
        w.write_bit(true);
        w.write_bit(hrd);
        if hrd {
            hrd_bits(&mut w, 0);
        }
        w.write_bit(hrd);
        if hrd {
            hrd_bits(&mut w, 1);
        }
        if hrd {
            w.write_bit(false);
        }
        w.write_bit(true);
        w.write_bit(true);
        w.write_bit(true);
        for value in [2, 1, 10, 10, 0, 4] {
            w.write_ue(value);
        }
    }
    w.rbsp_trailing();
    let mut nal = vec![0x67];
    nal.extend(escape_rbsp(&w.finish()));
    nal
}
#[test]
fn absent_vui_signal_or_description_and_existing_dual_hrd_remain_structurally_exact() {
    for profile in [66, 77, 88, 100] {
        for (vui, signal) in [
            (false, None),
            (true, None),
            (
                true,
                Some(VideoSignal {
                    format: 0,
                    full_range: true,
                    color: None,
                }),
            ),
            (
                true,
                Some(VideoSignal {
                    format: 4,
                    full_range: true,
                    color: Some([9, 16, 9]),
                }),
            ),
        ] {
            for hrd in [false, true] {
                let input = synthetic(profile, vui, signal, hrd);
                let changed = ensure_bt709_limited_sps(&input).unwrap();
                assert_only_color_changed(&input, &changed.bytes);
                let fields = parsed(&changed.bytes).vui.unwrap();
                assert_eq!(fields.video_format, signal.map_or(5, |s| s.format));
                assert!(!fields.full_range);
                if vui && hrd {
                    assert!(fields.nal_hrd.is_some() && fields.vcl_hrd.is_some());
                    assert!(fields.bitstream_restriction.is_some());
                }
            }
        }
    }
}
#[test]
fn already_matching_signal_is_bit_identical_including_original_epbs() {
    let input = ensure_bt709_limited_sps(native_sps()).unwrap().bytes;
    assert!(
        input.windows(3).any(|w| w == [0, 0, 3]),
        "exercise EPBs in real syntax"
    );
    let output = ensure_bt709_limited_sps(&input).unwrap();
    assert_eq!(output.bytes, input);
    assert_eq!(output.changed_sps, 0);
    let custom = synthetic(
        66,
        true,
        Some(VideoSignal {
            format: 2,
            full_range: false,
            color: Some([1, 1, 1]),
        }),
        true,
    );
    assert_eq!(ensure_bt709_limited_sps(&custom).unwrap().bytes, custom);
}
fn sample(nals: &[&[u8]]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for nal in nals {
        bytes.extend_from_slice(&(nal.len() as u32).to_be_bytes());
        bytes.extend_from_slice(nal);
    }
    bytes
}
#[test]
fn config_and_in_band_sps_match_and_all_non_sps_nals_are_unchanged() {
    let original = NATIVE_AVCC.to_vec();
    let out = rewrite_baseline_avcc(&original).unwrap();
    assert_eq!(out.changed_sps, 1);
    assert_eq!(&out.bytes[..6], &original[..6]);
    let mut at = 6;
    let changed_sps = nal_at(&out.bytes, &mut at, 2).unwrap();
    assert_only_color_changed(native_sps(), changed_sps);
    assert_eq!(
        &out.bytes[at..],
        &original[32..],
        "PPS count and bytes unchanged"
    );
    let pps = &original[35..];
    let sei = &[6, 5, 1, 0x80];
    let slice = &[0x65, 0x88, 0x84];
    let packet = sample(&[native_sps(), pps, sei, slice]);
    let changed = rewrite_length_prefixed_sample(&packet).unwrap();
    let mut at = 0;
    assert_eq!(nal_at(&changed.bytes, &mut at, 4).unwrap(), changed_sps);
    for expected in [pps, sei, slice] {
        assert_eq!(nal_at(&changed.bytes, &mut at, 4).unwrap(), expected);
    }
    let without = sample(&[pps, sei, slice]);
    let unchanged = rewrite_length_prefixed_sample(&without).unwrap();
    assert_eq!(unchanged.bytes, without);
    assert_eq!(unchanged.origin, MetadataOrigin::NoSpsInSample);
    assert_eq!(rewrite_baseline_avcc(&out.bytes).unwrap().bytes, out.bytes);
}
#[test]
fn strict_malformed_epb_trailing_bits_and_budget_rejections_never_return_partial_output() {
    for removed in 1..native_sps().len() {
        assert!(
            ensure_bt709_limited_sps(&native_sps()[..native_sps().len() - removed]).is_err(),
            "truncated {removed}"
        );
    }
    for bad in [
        vec![0x67, 0, 0, 3],
        vec![0x67, 0, 0, 3, 4],
        vec![0x67, 0, 0, 1],
        vec![0x67, 66, 0, 30, 0, 0, 2],
    ] {
        assert!(ensure_bt709_limited_sps(&bad).is_err());
    }
    let mut missing_stop = native_sps().to_vec();
    *missing_stop.last_mut().unwrap() = 0;
    assert!(ensure_bt709_limited_sps(&missing_stop).is_err());
    let mut extra = native_sps().to_vec();
    extra.push(0);
    assert!(ensure_bt709_limited_sps(&extra).is_err());
    assert!(matches!(
        ensure_bt709_limited_sps(&vec![0x67; MAX_SPS_BYTES + 1]),
        Err(Error::Limit(_))
    ));
    assert!(matches!(
        rewrite_baseline_avcc(&vec![0; MAX_CONFIG_BYTES + 1]),
        Err(Error::Limit(_))
    ));
    assert!(matches!(
        rewrite_length_prefixed_sample(&vec![0; MAX_SAMPLE_BYTES + 1]),
        Err(Error::Limit(_))
    ));
    for bad in [&[0, 0, 0][..], &[0, 0, 0, 4, 0x65][..], &[0, 0, 0, 0][..]] {
        assert!(rewrite_length_prefixed_sample(bad).is_err());
    }
    let mut mismatch = NATIVE_AVCC.to_vec();
    mismatch[1] = 77;
    assert!(rewrite_baseline_avcc(&mismatch).is_err());
    let too_many = sample(&vec![&[0x65, 0x80][..]; 129]);
    assert!(matches!(
        rewrite_length_prefixed_sample(&too_many),
        Err(Error::Limit(_))
    ));
    // The native fixture remains untouched by every rejected candidate.
    assert_eq!(native_sps(), &NATIVE_AVCC[8..32]);
}

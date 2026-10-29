use super::*;
fn bits(samples: &[f32]) -> Vec<u32> {
    samples.iter().map(|x| x.to_bits()).collect()
}
fn signal() -> Vec<f32> {
    let rows = [
        [1., 0.],
        [0., 0.5],
        [0., 0.],
        [0.125, -0.25],
        [-0.9, 0.1],
        [0.001, -0.002],
        [0., -0.],
        [0.7, -0.8],
    ];
    (0..64).flat_map(|i| rows[i % 8]).collect()
}
#[test]
fn literal_limiter_threshold_microsecond_attack_and_block_linking_are_not_hard_clip_aliases() {
    let mut limiter = ObsLimiter::new(8000, 2, ObsLimiterParameters::default()).unwrap();
    let mut input = [1., -1., 0., -0.];
    limiter
        .process_callback(&mut input, &mut [0.; 2], DynamicsLimits::default())
        .unwrap();
    assert!((input[0] - 0.5011872).abs() < 0.0000001);
    assert_eq!(input[0], -input[1]);
    assert_eq!(input[3].to_bits(), (-0.0f32).to_bits());
    let mut one = ObsLimiter::new(8000, 2, ObsLimiterParameters::default()).unwrap();
    let mut split = one;
    let mut a = [1., 0., 0., 0.5];
    let mut b = a;
    one.process_callback(&mut a, &mut [0.; 2], DynamicsLimits::default())
        .unwrap();
    for f in b.chunks_exact_mut(2) {
        split
            .process_callback(f, &mut [0.; 1], DynamicsLimits::default())
            .unwrap();
    }
    assert!((a[3] - 0.2511162).abs() < 0.000002);
    assert!((b[3] - 0.2508547).abs() < 0.000002);
    assert!(
        a[3] > b[3] + 0.0002,
        "callback boundary must remain observable"
    );
    let mut highrate = ObsLimiter::new(192000, 1, ObsLimiterParameters::default()).unwrap();
    let mut transient = [1.];
    highrate
        .process_callback(&mut transient, &mut [0.], DynamicsLimits::default())
        .unwrap();
    assert!(
        transient[0] > 0.5011872,
        "source attack is finite, not hard clipping"
    );
    assert!(transient[0] < 0.505);
}
#[test]
fn source_compressor_attack_hard_knee_and_ratio_are_literal() {
    let p = ObsCompressorParameters::default();
    assert_eq!(p.ratio(), 10.);
    assert_eq!(p.attack_ms(), 6);
    assert_eq!(p.release_ms(), 60);
    assert!((p.transfer_db(0.).unwrap() + 16.2).abs() < 0.00001);
    let mut c = ObsCompressor::new(48000, 2, p).unwrap();
    let mut x = vec![1.; 8192];
    c.process_callback(&mut x, &mut [0.; 4096], DynamicsLimits::default())
        .unwrap();
    assert_eq!(x[0], 1., "no lookahead or fake instant clamp");
    assert!((x[8190] - 0.15488166).abs() < 0.0001);
    assert_eq!(x[8190], x[8191]);
    let before = c.envelope();
    c.set_parameters(ObsCompressorParameters::new(-12., 4., 20, 200, 3.).unwrap());
    assert_eq!(c.envelope(), before, "parameter updates retain envelope");
    c.reset();
    assert_eq!(c.envelope(), 0.);
}
#[test]
fn expander_per_channel_detector_and_attenuation_floor_differ_from_craft_linked_gate() {
    let p = ObsExpanderParameters::new(-40., 2., 10, 50, 0., ObsDetector::Peak).unwrap();
    let mut e = ObsExpander::new(8000, 2, p).unwrap();
    let mut x = [0.5, 0.001].repeat(4096);
    e.process_interleaved(&mut x, DynamicsLimits::default())
        .unwrap();
    assert_eq!(x[8190], 0.5);
    assert!(
        (x[8191] - 0.0001).abs() < 0.0000001,
        "quiet channel is not held open by loud channel"
    );
    let mut gate = ObsExpander::new(
        8000,
        1,
        ObsExpanderParameters::new(-40., 10., 10, 125, 0., ObsDetector::Peak).unwrap(),
    )
    .unwrap();
    let mut quiet = vec![0.001; 4096];
    for _ in 0..3 {
        quiet.fill(0.001);
        gate.process_interleaved(&mut quiet, DynamicsLimits::default())
            .unwrap();
    }
    assert!(
        (quiet[4095] - 0.000001).abs() < 0.00000001,
        "literal60dB attenuation floor"
    );
    assert_eq!(ObsExpanderParameters::gate_preset().release_ms(), 125);
    assert_eq!(ObsExpanderParameters::gate_preset().ratio(), 10.);
}
#[test]
fn expander_partition_invariance_all_channels_and_reset_are_bit_exact() {
    for channels in 1..=8 {
        for detector in [ObsDetector::Peak, ObsDetector::Rms] {
            let p = ObsExpanderParameters::new(-20., 3., 7, 81, -2., detector).unwrap();
            let mut whole = ObsExpander::new(44100, channels, p).unwrap();
            let mut split = whole;
            let mut a: Vec<f32> = (0..257 * usize::from(channels))
                .map(|i| ((i % 31) as f32 - 15.) / 97.)
                .collect();
            let original = a.clone();
            let mut b = a.clone();
            whole
                .process_interleaved(&mut a, DynamicsLimits::default())
                .unwrap();
            for chunk in b.chunks_mut(7 * usize::from(channels)) {
                split
                    .process_interleaved(chunk, DynamicsLimits::default())
                    .unwrap();
            }
            assert_eq!(bits(&a), bits(&b));
            whole.reset();
            let mut again = original;
            whole
                .process_interleaved(&mut again, DynamicsLimits::default())
                .unwrap();
            assert_eq!(bits(&a), bits(&again));
        }
    }
}
#[test]
fn full_validation_and_work_rejection_preserve_samples_and_history() {
    for value in [f32::NAN, f32::INFINITY, -60.01, 0.01] {
        assert!(ObsLimiterParameters::new(value, 60).is_err());
    }
    for ms in [0, 1001, u32::MAX] {
        assert!(ObsLimiterParameters::new(-6., ms).is_err());
    }
    assert!(ObsCompressorParameters::new(-18., 33., 6, 60, 0.).is_err());
    assert!(ObsCompressorParameters::new(-18., 10., 501, 60, 0.).is_err());
    assert!(ObsExpanderParameters::new(-40., 2., 101, 50, 0., ObsDetector::Rms).is_err());
    assert!(ObsExpander::new(7999, 2, Default::default()).is_err());
    let mut l = ObsLimiter::new(48000, 2, Default::default()).unwrap();
    let mut x = [1., 0., 0., 0.5];
    l.process_callback(&mut x, &mut [0.; 2], Default::default())
        .unwrap();
    let prior = l.envelope();
    for limits in [
        DynamicsLimits {
            max_frames: 1,
            ..Default::default()
        },
        DynamicsLimits {
            max_bytes: 23,
            ..Default::default()
        },
        DynamicsLimits {
            max_sample_visits: 47,
            ..Default::default()
        },
    ] {
        let mut data = [0.25; 4];
        assert!(l.process_callback(&mut data, &mut [0.; 2], limits).is_err());
        assert_eq!(data, [0.25; 4]);
        assert_eq!(l.envelope(), prior);
    }
    let mut bad = [0.25, 0.125, 0.5, f32::NAN];
    let before = bits(&bad);
    assert!(
        l.process_callback(&mut bad, &mut [0.; 2], Default::default())
            .is_err()
    );
    assert_eq!(bits(&bad), before);
    assert_eq!(l.envelope(), prior);
    let mut e = ObsExpander::new(48000, 2, Default::default()).unwrap();
    let baseline = e;
    let mut bad = [0.125, 0.25, 0.1, f32::MAX];
    let before = bad;
    assert_eq!(
        e.process_interleaved(&mut bad, Default::default()),
        Err(ObsDynamicsError::NonRepresentable)
    );
    assert_eq!(bad, before);
    let mut a = [0.02, 0.03].repeat(40);
    let mut b = a.clone();
    e.process_interleaved(&mut a, Default::default()).unwrap();
    let mut control = baseline;
    control
        .process_interleaved(&mut b, Default::default())
        .unwrap();
    assert_eq!(bits(&a), bits(&b));
}
#[test]
fn zero_callbacks_do_not_consume_time_or_clear_state() {
    let mut l = ObsLimiter::new(48000, 1, Default::default()).unwrap();
    l.process_callback(&mut [1.], &mut [0.], Default::default())
        .unwrap();
    let old = l.envelope();
    l.process_callback(&mut [], &mut [], Default::default())
        .unwrap();
    assert_eq!(l.envelope(), old);
}
#[test]
#[ignore = "explicit source-C reference fixture; no C compiler or process is run by ordinary tests"]
fn independently_compiled_source_c_callback_oracles_match() {
    use std::io::Read;
    let path = std::env::var_os("SKYLIVE_OBS_DYNAMICS_REFERENCE_PATH")
        .expect("run staged reference/run_reference.py first");
    let mut bytes = vec![];
    std::fs::File::open(path)
        .unwrap()
        .take(65537)
        .read_to_end(&mut bytes)
        .unwrap();
    assert!(bytes.len() <= 65536);
    let text = std::str::from_utf8(&bytes).unwrap();
    let mut names = std::collections::HashSet::new();
    for line in text.lines() {
        let mut values = line.split_whitespace();
        let name = values.next().unwrap();
        assert!(names.insert(name));
        assert_eq!(values.next(), Some("128"));
        let expected: Vec<f32> = values
            .map(|s| f32::from_bits(u32::from_str_radix(s, 16).unwrap()))
            .collect();
        assert_eq!(expected.len(), 128);
        let mut data = signal();
        let pieces: Vec<usize> = if name.ends_with("_split") {
            vec![1, 1, 7, 11, 23, 21]
        } else if name.ends_with("_updated") {
            vec![32, 32]
        } else {
            vec![64]
        };
        if name.starts_with("limiter_") {
            let mut p = ObsLimiter::new(48000, 2, Default::default()).unwrap();
            let mut offset = 0;
            for frames in pieces {
                if offset == 32 && name.ends_with("_updated") {
                    p.set_parameters(ObsLimiterParameters::new(-12., 200).unwrap());
                }
                p.process_callback(
                    &mut data[offset * 2..(offset + frames) * 2],
                    &mut [0.; 64],
                    Default::default(),
                )
                .unwrap();
                offset += frames;
            }
        } else if name.starts_with("compressor_") {
            let mut p = ObsCompressor::new(48000, 2, Default::default()).unwrap();
            let mut offset = 0;
            for frames in pieces {
                if offset == 32 && name.ends_with("_updated") {
                    p.set_parameters(ObsCompressorParameters::new(-12., 4., 20, 200, 3.).unwrap());
                }
                p.process_callback(
                    &mut data[offset * 2..(offset + frames) * 2],
                    &mut [0.; 64],
                    Default::default(),
                )
                .unwrap();
                offset += frames;
            }
        } else {
            assert!(name.starts_with("expander_"));
            let detector = if name.contains("_peak_") {
                ObsDetector::Peak
            } else {
                ObsDetector::Rms
            };
            let mut p = ObsExpander::new(
                48000,
                2,
                ObsExpanderParameters::new(-40., 2., 10, 50, 0., detector).unwrap(),
            )
            .unwrap();
            let mut offset = 0;
            for frames in pieces {
                if offset == 32 && name.ends_with("_updated") {
                    p.set_parameters(
                        ObsExpanderParameters::new(-24., 4., 20, 100, 3., ObsDetector::Peak)
                            .unwrap(),
                    );
                }
                p.process_interleaved(
                    &mut data[offset * 2..(offset + frames) * 2],
                    Default::default(),
                )
                .unwrap();
                offset += frames;
            }
        }
        for (i, (a, b)) in data.iter().zip(&expected).enumerate() {
            assert!(
                (a - b).abs() <= 0.000003 * a.abs().max(b.abs()).max(0.001),
                "{name}[{i}] Rust{a:?} C{b:?}"
            );
        }
    }
    assert_eq!(names.len(), 12);
}

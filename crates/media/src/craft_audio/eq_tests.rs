use super::*;

fn params(low: f32, mid: f32, high: f32, output: f32) -> SimpleEqParameters {
    SimpleEqParameters::new(low, 200.0, mid, 1000.0, 0.7, high, 5000.0, output).unwrap()
}
fn tone(frequency: f64, frames: usize, rate: u32) -> Vec<f32> {
    (0..frames)
        .map(|frame| (0.1 * (2.0 * PI * frequency * frame as f64 / f64::from(rate)).sin()) as f32)
        .collect()
}
fn measured_gain_db(eq: &mut SimpleEq, frequency: f64) -> f64 {
    eq.reset();
    let rate = eq.sample_rate;
    let mut input = tone(frequency, rate as usize, rate);
    for chunk in input.chunks_mut(997) {
        eq.process_interleaved(chunk).unwrap();
    }
    let mut sine = 0.0;
    let mut cosine = 0.0;
    for (index, &sample) in input.iter().enumerate().skip(input.len() / 2) {
        let phase = 2.0 * PI * frequency * index as f64 / f64::from(rate);
        sine += f64::from(sample) * phase.sin();
        cosine += f64::from(sample) * phase.cos();
    }
    let amplitude = 2.0 * sine.hypot(cosine) / (input.len() / 2) as f64;
    20.0 * (amplitude / 0.1).log10()
}

#[test]
fn flat_is_exact_and_literal_band_center_and_asymptote_oracles_hold() {
    let mut eq = SimpleEq::new(48000, 1, SimpleEqParameters::default()).unwrap();
    let mut samples = [0.0, -0.0, 0.125, -0.5, 1.0, f32::MIN_POSITIVE, f32::MAX];
    let before = samples;
    eq.process_interleaved(&mut samples).unwrap();
    assert_eq!(samples, before);
    for frequency in [0.0, 30.0, 1000.0, 8000.0, 24000.0] {
        assert!(eq.response_db(frequency).unwrap().abs() < 1e-9);
    }
    for (parameters, frequency, gain) in [
        (params(0.0, 6.0, 0.0, 0.0), 1000.0, 6.0),
        (params(6.0, 0.0, 0.0, 0.0), 200.0, 3.0),
        (params(0.0, 0.0, -6.0, 0.0), 5000.0, -3.0),
        (params(0.0, 0.0, 0.0, -12.0), 1000.0, -12.0),
    ] {
        let mut eq = SimpleEq::new(48000, 1, parameters).unwrap();
        // Exact RBJ shelf half-gain at its corner, peaking full-gain at center,
        // and an independent sinusoidal projection of actual processed PCM.
        assert!((eq.response_db(frequency).unwrap() - gain).abs() < 0.00001);
        assert!((measured_gain_db(&mut eq, frequency) - gain).abs() < 0.01);
    }
    let low = SimpleEq::new(48000, 1, params(6.0, 0.0, 0.0, 0.0)).unwrap();
    let high = SimpleEq::new(48000, 1, params(0.0, 0.0, -6.0, 0.0)).unwrap();
    assert!((low.response_db(0.0).unwrap() - 6.0).abs() < 1e-8);
    assert!((high.response_db(24000.0).unwrap() + 6.0).abs() < 1e-8);
    assert_eq!(eq.latency_frames(), 0);
}

#[test]
fn parameter_ramps_interleaving_and_block_partition_are_exact() {
    let initial = params(6.0, -3.0, 9.0, -2.0);
    let changed =
        SimpleEqParameters::new(-9.0, 60.0, 12.0, 2400.0, 3.0, -12.0, 13000.0, -6.0).unwrap();
    let input: Vec<_> = (0..10000)
        .map(|frame| (frame as f32 * 0.017).sin() * 0.2)
        .collect();
    let mut left = input.clone();
    let mut right: Vec<_> = input.iter().map(|sample| -sample * 0.25).collect();
    let mut packed: Vec<_> = left
        .iter()
        .zip(&right)
        .flat_map(|(&l, &r)| [l, r])
        .collect();
    let mut whole = SimpleEq::new(44100, 2, initial).unwrap();
    let mut split = whole.clone();
    for range in [0..1337, 1337..10000] {
        if range.start != 0 {
            whole.set_parameters(changed);
            split.set_parameters(changed);
        }
        for (l, r) in left[range.clone()]
            .chunks_mut(4096)
            .zip(right[range.clone()].chunks_mut(4096))
        {
            whole.process_planar(&mut [l, r]).unwrap();
        }
        for chunk in packed[range.start * 2..range.end * 2].chunks_mut(34) {
            split.process_interleaved(chunk).unwrap();
        }
    }
    for (frame, (&l, &r)) in packed.chunks_exact(2).zip(left.iter().zip(&right)) {
        assert_eq!(frame, &[l, r]);
        assert_eq!(r, -l * 0.25);
    }
    assert_eq!(whole, split);
    assert_eq!(whole.parameters(), changed);
    // Output gain uses a linear-amplitude ramp, not a dB interpolation.
    let mut ramp = SimpleEq::new(48000, 1, SimpleEqParameters::default()).unwrap();
    ramp.set_parameters(params(0.0, 0.0, 0.0, -20.0));
    let mut block = [1.0; 1440];
    ramp.process_interleaved(&mut block).unwrap();
    assert!((block[719] - 0.55).abs() < 1e-6);
    assert!((block[1439] - 0.1).abs() < 1e-7);
}

#[test]
fn state_tail_survives_parameter_change_and_reset_snaps_and_clears_it() {
    let mut eq = SimpleEq::new(48000, 1, params(0.0, 12.0, 0.0, 0.0)).unwrap();
    eq.process_interleaved(&mut [1.0]).unwrap();
    let mut candidate = eq.clone();
    let mut tail = [0.0; 32];
    candidate.process_interleaved(&mut tail).unwrap();
    assert!(tail.iter().any(|sample| sample.abs() > 0.0001));
    let changed = params(0.0, -6.0, 0.0, -3.0);
    eq.set_parameters(changed);
    eq.process_interleaved(&mut [0.0; 7]).unwrap();
    eq.reset();
    let mut fresh = SimpleEq::new(48000, 1, changed).unwrap();
    let mut a = tone(1000.0, 1024, 48000);
    let mut b = a.clone();
    eq.process_interleaved(&mut a).unwrap();
    fresh.process_interleaved(&mut b).unwrap();
    assert_eq!(a, b);
    // A reset never erases authored target settings.
    assert_eq!(eq.parameters(), changed);
    assert_eq!(eq.response_db(1000.0), fresh.response_db(1000.0));
}

#[test]
fn invalid_settings_pcm_and_overflow_reject_before_state_or_output_publication() {
    let defaults = [0.0, 200.0, 0.0, 1000.0, 0.7, 0.0, 5000.0, 0.0];
    for (index, invalid) in [
        (0, -24.01),
        (1, 19.99),
        (2, 24.01),
        (3, 10000.01),
        (4, 0.099),
        (5, f32::NAN),
        (6, 20000.01),
        (7, f32::INFINITY),
    ] {
        let mut p = defaults;
        p[index] = invalid;
        assert!(SimpleEqParameters::new(p[0], p[1], p[2], p[3], p[4], p[5], p[6], p[7]).is_err());
    }
    for (rate, channels) in [(7999, 2), (192001, 2), (48000, 0), (48000, 9)] {
        assert!(SimpleEq::new(rate, channels, SimpleEqParameters::default()).is_err());
    }
    let mut eq = SimpleEq::new(48000, 2, params(24.0, 24.0, 24.0, 24.0)).unwrap();
    let before = eq.clone();
    for mut samples in [[0.1, 0.2, f32::NAN, 0.3], [0.0, 0.0, f32::MAX, f32::MAX]] {
        let bits = samples.map(f32::to_bits);
        assert_eq!(
            eq.process_interleaved(&mut samples),
            Err(EqError::InvalidSample)
        );
        assert_eq!(samples.map(f32::to_bits), bits);
        assert_eq!(eq, before);
    }
    assert_eq!(
        eq.process_interleaved(&mut [0.0]),
        Err(EqError::InvalidBlock)
    );
    assert_eq!(
        eq.process_planar(&mut [&mut [0.0; 2], &mut [0.0; 3]]),
        Err(EqError::InvalidBlock)
    );
    assert_eq!(
        eq.process_interleaved(&mut vec![0.0; 8194]),
        Err(EqError::InvalidBlock)
    );
    assert_eq!(eq, before);
    for frequency in [-1.0, 24000.1, f64::NAN, f64::INFINITY] {
        assert!(eq.response_db(frequency).is_err());
    }
}

#[test]
fn supported_rate_edges_keep_clamped_centers_and_high_q_processing_finite() {
    for rate in [8000, 44100, 48000, 192000] {
        let settings =
            SimpleEqParameters::new(24.0, 20.0, 24.0, 10000.0, 10.0, 24.0, 20000.0, 24.0).unwrap();
        let mut eq = SimpleEq::new(rate, 8, settings).unwrap();
        let mut block = [0.0; 8192];
        block[..8].fill(0.001);
        eq.process_interleaved(&mut block).unwrap();
        assert!(block.iter().all(|sample| sample.is_finite()));
        assert!(eq.response_db(f64::from(rate) * 0.49).unwrap().is_finite());
    }
}

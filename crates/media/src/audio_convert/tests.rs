use super::*;
fn format(rate: u32, channels: u16, mask: u32) -> AudioFormat {
    AudioFormat::new(rate, channels, mask).unwrap()
}
fn drain(converter: &mut AudioConverter, result: &mut Vec<f32>) {
    let channels = usize::from(converter.output_format().channels());
    let mut packet = vec![0.0; 1024 * channels];
    loop {
        let frames = converter.pull(&mut packet).unwrap();
        if frames == 0 {
            break;
        }
        result.extend_from_slice(&packet[..frames * channels]);
    }
}
fn convert(
    input: AudioFormat,
    output: AudioFormat,
    source: &[f32],
    packet_frames: usize,
) -> (Vec<f32>, AudioConversionStats) {
    let mut converter = AudioConverter::new(input, output, AudioConvertLimits::default()).unwrap();
    let mut result = Vec::new();
    for packet in source.chunks(packet_frames * usize::from(input.channels())) {
        converter.push(packet).unwrap();
        drain(&mut converter, &mut result);
    }
    converter.finish().unwrap();
    for _ in 0..1000 {
        if converter.stats().drained {
            break;
        }
        drain(&mut converter, &mut result);
    }
    assert!(converter.stats().drained);
    (result, converter.stats())
}
fn tone(rate: u32, frequency: f64, frames: usize) -> Vec<f32> {
    (0..frames)
        .map(|index| {
            (0.5 * (std::f64::consts::TAU * frequency * index as f64 / f64::from(rate)).sin())
                as f32
        })
        .collect()
}
fn rms(values: &[f32]) -> f64 {
    (values.iter().map(|x| f64::from(*x).powi(2)).sum::<f64>() / values.len() as f64).sqrt()
}

#[test]
fn identity_is_exact_has_no_delay_and_never_clips() {
    let source = [0.25, -0.5, 1.5, -2.0, -0.0, f32::MIN_POSITIVE];
    let (output, stats) = convert(format(48000, 2, 3), format(48000, 2, 3), &source, 2);
    assert_eq!(output, source);
    assert_eq!(stats.delay_output_frames, 0);
    assert_eq!(
        output
            .iter()
            .map(|sample| sample.to_bits())
            .collect::<Vec<_>>(),
        source
            .iter()
            .map(|sample| sample.to_bits())
            .collect::<Vec<_>>()
    );
    assert_eq!(stats.accepted_input_frames, 3);
    assert_eq!(stats.consumed_input_frames, 3);
    assert_eq!(stats.produced_output_frames, 3);
    assert_eq!(stats.delivered_output_frames, 3);
    assert_eq!(stats.padded_input_frames, 0);
}

#[test]
fn literal_obs_mono_override_and_ffmpeg_surround_matrix_without_normalization() {
    let (stereo, _) = convert(format(48000, 1, 4), format(48000, 2, 3), &[0.75], 1);
    assert_eq!(stereo, [0.75, 0.75]);
    let (six, _) = convert(format(48000, 1, 4), format(48000, 6, 0x3f), &[0.5], 1);
    assert_eq!(six, [0.5; 6]); // OBS's unusual six-channel mono→LFE row.
    let (eight, _) = convert(format(48000, 1, 4), format(48000, 8, 0x63f), &[0.5], 1);
    assert_eq!(eight, [0.5, 0.5, 0.5, 0.0, 0.5, 0.5, 0.5, 0.5]);
    // The authored OBS override is indexed by count, even for other valid
    // speaker masks: it does not reinterpret the muted column as "LFE".
    let (three, _) = convert(format(48000, 1, 4), format(48000, 3, 7), &[0.5], 1);
    assert_eq!(three, [0.5, 0.5, 0.0]);
    let (five, _) = convert(format(48000, 1, 4), format(48000, 5, 0x37), &[0.5], 1);
    assert_eq!(five, [0.5, 0.5, 0.5, 0.0, 0.5]);
    let c = 0.7071067690849304f64;
    let (output, _) = convert(
        format(48000, 6, 0x3f),
        format(48000, 2, 3),
        &[1.0, 2.0, 3.0, 99.0, 5.0, 7.0],
        1,
    );
    assert_eq!(output, [(1.0 + 8.0 * c) as f32, (2.0 + 10.0 * c) as f32]);
    assert!(output[0] > 1.0); // no row normalization or intermediate clipping.
    let (mono, _) = convert(format(48000, 2, 3), format(48000, 1, 4), &[1.0, 0.0], 1);
    assert_eq!(mono, [0.70710677]);
    let (back, _) = convert(
        format(48000, 8, 0x63f),
        format(48000, 6, 0x3f),
        &[0.0, 0.0, 0.0, 0.0, 1.0, 2.0, 3.0, 4.0],
        1,
    );
    assert_eq!(
        back,
        [
            0.0,
            0.0,
            0.0,
            0.0,
            (1.0 + 3.0 * std::f64::consts::FRAC_1_SQRT_2) as f32,
            (2.0 + 4.0 * std::f64::consts::FRAC_1_SQRT_2) as f32
        ]
    );
}

#[test]
fn unknown_layout_is_never_assigned_multichannel_speaker_roles() {
    let same = AudioConverter::new(
        format(48000, 8, 0),
        format(48000, 8, 0),
        AudioConvertLimits::default(),
    );
    assert!(same.is_ok());
    assert!(
        AudioConverter::new(
            format(48000, 8, 0),
            format(48000, 8, 0x63f),
            AudioConvertLimits::default()
        )
        .is_err()
    );
    assert!(
        AudioConverter::new(
            format(48000, 8, 0),
            format(48000, 2, 3),
            AudioConvertLimits::default()
        )
        .is_err()
    );
    let (stereo, _) = convert(format(48000, 1, 0), format(48000, 2, 3), &[0.25], 1);
    assert_eq!(stereo, [0.25, 0.25]);
    assert!(AudioFormat::new(48000, 3, 3).is_err());
    assert!(AudioFormat::new(7999, 1, 4).is_err());
    assert!(AudioFormat::new(192001, 1, 4).is_err());
    assert!(AudioFormat::new(48000, 9, 0).is_err());
}

#[test]
fn all_modern_obs_multichannel_layouts_preserve_literal_speaker_order() {
    let c = 0.7071067690849304f64;
    let h = 0.4999999914428645f64;
    for (channels, mask, source, expected) in [
        (3, 0x0b, vec![1.0, 2.0, 99.0], [1.0, 2.0]),
        (
            4,
            0x107,
            vec![1.0, 2.0, 3.0, 4.0],
            [
                (1.0 + 3.0 * c + 4.0 * h) as f32,
                (2.0 + 3.0 * c + 4.0 * h) as f32,
            ],
        ),
        (
            5,
            0x10f,
            vec![1.0, 2.0, 3.0, 99.0, 4.0],
            [
                (1.0 + 3.0 * c + 4.0 * h) as f32,
                (2.0 + 3.0 * c + 4.0 * h) as f32,
            ],
        ),
        (
            6,
            0x3f,
            vec![1.0, 2.0, 3.0, 99.0, 5.0, 7.0],
            [(1.0 + 8.0 * c) as f32, (2.0 + 10.0 * c) as f32],
        ),
        (
            7,
            0x70f,
            vec![1.0, 2.0, 3.0, 99.0, 4.0, 5.0, 7.0],
            [
                (1.0 + 8.0 * c + 4.0 * h) as f32,
                (2.0 + 10.0 * c + 4.0 * h) as f32,
            ],
        ),
        (
            8,
            0x63f,
            vec![1.0, 2.0, 3.0, 99.0, 4.0, 5.0, 6.0, 7.0],
            [(1.0 + 13.0 * c) as f32, (2.0 + 15.0 * c) as f32],
        ),
    ] {
        let (output, _) = convert(
            format(48000, channels, mask),
            format(48000, 2, 3),
            &source,
            1,
        );
        assert_eq!(output, expected, "layout {mask:x}");
    }
}

#[test]
fn finite_conversion_has_exact_counts_and_packet_partition_independence() {
    let source = tone(44100, 1000.0, 44101);
    let (large, a) = convert(format(44100, 1, 4), format(48000, 1, 4), &source, 4096);
    let (small, b) = convert(format(44100, 1, 4), format(48000, 1, 4), &source, 113);
    assert_eq!(large, small);
    assert_eq!(large.len(), 48002);
    assert_eq!(a.accepted_input_frames, 44101);
    assert_eq!(a.consumed_input_frames, 44101);
    assert_eq!(a.delivered_output_frames, 48002);
    assert_eq!(a.produced_output_frames, 48002);
    assert!(a.delay_output_frames > 0);
    assert!(a.padded_input_frames > 0);
    assert_eq!(a, b);
    let (tiny, stats) = convert(format(192000, 1, 4), format(44100, 1, 4), &[0.5], 1);
    assert_eq!(tiny.len(), 1);
    assert_eq!(stats.consumed_input_frames, 1);
}

#[test]
fn passband_dc_and_alias_rejection_have_independent_signal_oracles() {
    let (pass, _) = convert(
        format(192000, 1, 4),
        format(44100, 1, 4),
        &tone(192000, 1000.0, 192000),
        4096,
    );
    assert!((rms(&pass[1024..pass.len() - 1024]) - 0.3535533905932738).abs() < 0.001);
    let (high_pass, _) = convert(
        format(192000, 1, 4),
        format(48000, 1, 4),
        &tone(192000, 12000.0, 192000),
        997,
    );
    assert!((rms(&high_pass[1024..high_pass.len() - 1024]) - 0.3535533905932738).abs() < 0.003);
    let (alias, _) = convert(
        format(192000, 1, 4),
        format(44100, 1, 4),
        &tone(192000, 60000.0, 192000),
        4096,
    );
    assert!(rms(&alias[1024..alias.len() - 1024]) < 0.0001);
    let (dc, _) = convert(
        format(8000, 1, 4),
        format(48000, 1, 4),
        &vec![0.25; 8000],
        257,
    );
    assert!(
        dc[1024..dc.len() - 1024]
            .iter()
            .all(|sample| (*sample - 0.25).abs() < 0.0001)
    );
}

#[test]
fn passband_waveforms_are_on_the_claimed_integer_output_grid() {
    for (input_rate, output_rate) in [
        (16000, 44100),
        (8000, 48000),
        (44100, 48000),
        (192000, 44100),
    ] {
        let (output, _) = convert(
            format(input_rate, 1, 4),
            format(output_rate, 1, 4),
            &tone(input_rate, 1000.0, input_rate as usize),
            997,
        );
        let mut signal = 0.0;
        let mut error = 0.0;
        for (index, sample) in output
            .iter()
            .enumerate()
            .take(output.len() - 1024)
            .skip(1024)
        {
            let expected = 0.5
                * (std::f64::consts::TAU * 1000.0 * index as f64 / f64::from(output_rate)).sin();
            signal += expected * expected;
            error += (f64::from(*sample) - expected).powi(2);
        }
        let snr = 10.0 * (signal / error).log10();
        assert!(
            snr > 65.0,
            "{input_rate}→{output_rate} integer-grid waveform SNR {snr}dB"
        );
    }
}

#[test]
fn impulse_energy_has_no_fractional_timestamp_advance() {
    let mut input = vec![0.0; 8000];
    input[4000] = 1.0;
    let (output, _) = convert(format(16000, 1, 4), format(44100, 1, 4), &input, 997);
    let center = 11025usize; // 4000/16000 seconds on the44.1kHz grid.
    let mut moment = 0.0;
    let mut energy = 0.0;
    for (index, sample) in output
        .iter()
        .enumerate()
        .take(center + 513)
        .skip(center - 512)
    {
        let power = f64::from(*sample).powi(2);
        energy += power;
        moment += (index as f64 - center as f64) * power;
    }
    assert!(
        (moment / energy).abs() < 0.001,
        "fractional impulse energy centroid {}",
        moment / energy
    );
    assert_eq!(
        output
            .iter()
            .enumerate()
            .max_by(|(_, a), (_, b)| a.abs().total_cmp(&b.abs()))
            .unwrap()
            .0,
        center
    );
}

#[test]
fn rejection_preserves_admission_and_reset_discards_filter_history() {
    let mut converter = AudioConverter::new(
        format(44100, 2, 3),
        format(48000, 2, 3),
        AudioConvertLimits {
            max_buffered_input_frames: 8192,
            ..AudioConvertLimits::default()
        },
    )
    .unwrap();
    let before = converter.stats();
    assert_eq!(
        converter.push(&[f32::NAN, 0.0]),
        Err(MediaError::InvalidAudio)
    );
    assert_eq!(converter.stats(), before);
    assert_eq!(converter.push(&[1.0]), Err(MediaError::InvalidBuffer));
    assert_eq!(converter.stats(), before);
    assert_eq!(
        converter.push(&vec![0.0; 8194]),
        Err(MediaError::BudgetExceeded)
    );
    assert_eq!(converter.stats(), before);
    converter.push(&vec![0.25; 8192]).unwrap();
    converter.push(&vec![0.5; 8192]).unwrap();
    let full = converter.stats();
    assert_eq!(converter.input_capacity_frames(), 0);
    assert_eq!(converter.push(&[0.0, 0.0]), Err(MediaError::BudgetExceeded));
    assert_eq!(converter.stats(), full);
    assert_eq!(
        converter.pull(&mut [0.0; 3]),
        Err(MediaError::InvalidBuffer)
    );
    assert_eq!(converter.stats(), full);
    converter.reset();
    assert_eq!(converter.stats(), before);
    converter.push(&[1.0, 0.0]).unwrap();
    converter.finish().unwrap();
    assert_eq!(converter.push(&[0.0, 0.0]), Err(MediaError::Closed));
    let mut result = Vec::new();
    drain(&mut converter, &mut result);
    let (fresh, _) = convert(format(44100, 2, 3), format(48000, 2, 3), &[1.0, 0.0], 1);
    assert_eq!(result, fresh);
    assert!(
        AudioConverter::new(
            format(44100, 1, 4),
            format(48000, 1, 4),
            AudioConvertLimits {
                max_bytes: 1,
                ..AudioConvertLimits::default()
            }
        )
        .is_err()
    );
    assert!(
        AudioConverter::new(
            format(192000, 1, 4),
            format(44100, 1, 4),
            AudioConvertLimits {
                max_buffered_input_frames: 8192,
                ..AudioConvertLimits::default()
            }
        )
        .is_err()
    );
}

#[test]
fn bounded_drift_preserves_dc_and_identity_refuses_hidden_filter_creation() {
    let mut converter = AudioConverter::new(
        format(44100, 1, 4),
        format(48000, 1, 4),
        AudioConvertLimits::default(),
    )
    .unwrap();
    for relative in [f64::NAN, 0.9, 1.02] {
        assert!(converter.set_ratio_adjustment(relative).is_err());
    }
    let mut output = Vec::new();
    for packet in 0..16 {
        if packet == 8 {
            converter.set_ratio_adjustment(1.001).unwrap();
        }
        converter.push(&vec![0.25; 1024]).unwrap();
        drain(&mut converter, &mut output);
    }
    converter.finish().unwrap();
    drain(&mut converter, &mut output);
    assert!(converter.stats().drained);
    assert!(
        output[1024..output.len() - 1024]
            .iter()
            .all(|value| (*value - 0.25).abs() < 0.0001)
    );
    assert_eq!(converter.stats().consumed_input_frames, 16384);
    let mut identity = AudioConverter::new(
        format(48000, 1, 4),
        format(48000, 1, 4),
        AudioConvertLimits::default(),
    )
    .unwrap();
    assert!(identity.set_ratio_adjustment(1.001).is_err());
    assert!(identity.set_ratio_adjustment(1.0).is_ok());
}

#[test]
fn rematrix_overflow_and_counter_overflow_are_rejected_before_publication() {
    let mut converter = AudioConverter::new(
        format(48000, 2, 3),
        format(48000, 1, 4),
        AudioConvertLimits::default(),
    )
    .unwrap();
    let before = converter.stats();
    assert!(converter.push(&[f32::MAX, f32::MAX]).is_err());
    assert_eq!(converter.stats(), before);
    converter.accepted = u64::MAX;
    assert_eq!(converter.push(&[0.0, 0.0]), Err(MediaError::Overflow));
    assert_eq!(converter.input_fifo.frames, 0);
}

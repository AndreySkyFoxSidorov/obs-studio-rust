use super::*;
fn fixture(name: &str, extension: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/media")
        .join(format!("{name}.{extension}"))
}
#[test]
fn cavlc_and_cabac_b_frames_match_independent_ffmpeg_cropped_yuv_exactly() {
    for name in ["cavlc", "cabac_b"] {
        let mut reader = MediaFileReader::open(&fixture(name, "mp4"), Default::default()).unwrap();
        let expected = std::fs::read(fixture(name, "yuv")).unwrap();
        let mut got = Vec::new();
        let mut pts = Vec::new();
        assert_eq!(
            (
                reader.info().video.as_ref().unwrap().width,
                reader.info().video.as_ref().unwrap().height
            ),
            (64, 48)
        );
        while let Some(picture) = reader.next_video().unwrap() {
            assert!(!picture.draft);
            pts.push(picture.pts);
            got.extend_from_slice(&picture.y);
            got.extend_from_slice(&picture.u);
            got.extend_from_slice(&picture.v);
        }
        assert_eq!(
            pts,
            vec![
                0,
                100_000_000,
                200_000_000,
                300_000_000,
                400_000_000,
                500_000_000
            ],
            "{name}: exact display PTS after edit list"
        );
        assert_eq!(got.len(), 64 * 48 * 3 / 2 * 6);
        assert_eq!(
            got, expected,
            "{name}: decoded plane bytes differ from external oracle"
        );
    }
}
#[test]
fn aac_lc_pcm_matches_external_decoder_and_trims_container_priming_and_tail() {
    for name in ["cavlc", "cabac_b", "no_pns/cavlc", "no_pns/cabac_b"] {
        let mut reader = MediaFileReader::open(&fixture(name, "mp4"), Default::default()).unwrap();
        let bytes = std::fs::read(fixture(name, "f32le")).unwrap();
        let reference: Vec<f32> = bytes
            .chunks_exact(4)
            .map(|p| f32::from_le_bytes(p.try_into().unwrap()))
            .collect();
        let mut samples = Vec::new();
        let mut timestamps = Vec::new();
        while let Some(block) = reader.next_audio().unwrap() {
            assert_eq!(
                (block.sample_rate, block.channels, block.channel_mask),
                (48000, 1, 4)
            );
            timestamps.push(block.presentation_ns);
            samples.extend(block.samples);
        }
        assert_eq!(timestamps[0], 0);
        assert!(timestamps.windows(2).all(|w| w[0] < w[1]));
        // FFmpeg's raw AAC decode includes the final coded padding frame. The
        // MP4 track's0.600s presentation duration, not raw padding, is authored.
        assert_eq!(samples.len(), 28_800);
        assert!(reference.len() >= samples.len());
        let mut square = 0.0;
        let mut peak = 0.0f64;
        let mut energy = 0.0;
        for (&a, &b) in samples.iter().zip(&reference) {
            assert!(a.is_finite());
            let error = f64::from(a) - f64::from(b);
            square += error * error;
            peak = peak.max(error.abs());
            energy += f64::from(b) * f64::from(b);
        }
        let rmse = (square / samples.len() as f64).sqrt();
        let max_index = samples
            .iter()
            .zip(&reference)
            .enumerate()
            .max_by(|(_, a), (_, b)| (a.0 - a.1).abs().total_cmp(&(b.0 - b.1).abs()))
            .unwrap()
            .0;
        let pns = reader.audio.as_ref().unwrap().uses_perceptual_noise();
        let spectrum = |data: &[f32], hz: f64| {
            let (mut re, mut im) = (0.0, 0.0);
            for (i, &v) in data.iter().enumerate() {
                let phase = std::f64::consts::TAU * hz * i as f64 / 48000.0;
                re += f64::from(v) * phase.cos();
                im += f64::from(v) * phase.sin();
            }
            (
                (re * re + im * im).sqrt() * 2.0 / data.len() as f64,
                re.atan2(im),
            )
        };
        let reference = &reference[..samples.len()];
        let errors: Vec<_> = samples.iter().zip(reference).map(|(a, b)| a - b).collect();
        let bins: Vec<_> = [500.0, 1000.0, 2000.0, 6000.0, 12000.0, 18000.0, 22000.0]
            .into_iter()
            .map(|hz| {
                (
                    hz,
                    spectrum(&samples, hz).0,
                    spectrum(reference, hz).0,
                    spectrum(&errors, hz).0,
                )
            })
            .collect();
        let lags: Vec<_> = (-3i32..=3)
            .map(|lag| {
                let mut sum = 0.0;
                let mut count = 0;
                for (i, &a) in samples.iter().enumerate() {
                    let j = i as i64 + i64::from(lag);
                    if j >= 0 && (j as usize) < reference.len() {
                        sum += (f64::from(a) - f64::from(reference[j as usize])).powi(2);
                        count += 1;
                    }
                }
                (lag, (sum / count as f64).sqrt())
            })
            .collect();
        eprintln!(
            "AAC_DIAGNOSTIC {name} pns={pns} rmse={rmse} peak={peak} peak_index={max_index} ours={} reference={} snr_db={} bins={bins:?} lag_rmse={lags:?}",
            samples[max_index],
            reference[max_index],
            10.0 * (energy / square).log10()
        );
        if let Some(directory) = std::env::var_os("SKYLIVE_MEDIA_ORACLE_DUMP") {
            let path = std::path::PathBuf::from(directory);
            std::fs::create_dir_all(&path).unwrap();
            let bytes: Vec<_> = samples
                .iter()
                .flat_map(|sample| sample.to_le_bytes())
                .collect();
            std::fs::write(
                path.join(format!("{}.actual.f32le", name.replace('/', "--"))),
                bytes,
            )
            .unwrap();
        }
        assert!(rmse <= 0.00002, "{name}: RMS decoder difference{rmse}");
        if name.starts_with("no_pns/") {
            assert!(!pns, "deterministic spectral fixture must not use PNS");
            assert!(peak <= 0.0002, "{name}: peak decoder difference{peak}");
        } else {
            // ISO PNS signals normalized random-band energy, not a normative
            // random PCM sequence. Exact high-frequency samples are therefore
            // not an oracle. Keep the original whole-signal RMS bound; verify
            // energy,70dB SNR and the independently generated1kHz tone's phase.
            // Numeric FFT evidence is in evidence/aac-pns-spectral-analysis.json;
            // deterministic no-PNS fixtures above retain the strict peak gate.
            assert!(
                pns,
                "fixture must exercise real perceptual noise substitution"
            );
            let ours_energy = samples.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>();
            assert!((10.0 * (ours_energy / energy).log10()).abs() < 0.001);
            assert!(10.0 * (energy / square).log10() > 70.0);
            let ours_tone = spectrum(&samples, 1000.0);
            let reference_tone = spectrum(reference, 1000.0);
            assert!((ours_tone.0 - reference_tone.0).abs() < 0.00001);
            assert!((ours_tone.1 - reference_tone.1).abs() < 0.00001);
        }
        assert!(
            energy / samples.len() as f64 > 0.001,
            "the fixture must contain a real decoded tone, not silent placeholder PCM"
        );
    }
}
#[test]
fn access_unit_admission_rejects_expansion_before_decoder_allocation() {
    assert!(check_access_unit(&[0, 0, 0, 2, 0x65, 0], 4).is_ok());
    assert!(check_access_unit(&[0, 0, 0, 1, 0x65, 0, 0, 0, 1, 0x41], 4).is_err());
    assert!(check_access_unit(&[0, 0, 0, 200, 0x65], 4).is_err());
    assert!(check_access_unit(&[0, 0, 0, 0], 4).is_err());
    assert!(check_access_unit(&[1, 0x65], 3).is_err());
}
#[test]
fn external_data_reference_is_never_assumed_to_mean_the_current_file() {
    let mut bytes = std::fs::read(fixture("cavlc", "mp4")).unwrap();
    let marker = bytes
        .windows(8)
        .position(|w| w == b"url \0\0\0\x01")
        .unwrap();
    bytes[marker + 7] = 0;
    let file = skylive_mp4::open(&bytes).unwrap();
    assert!(file.tracks.iter().any(|t| !t.self_contained_data));
    assert!(
        file.tracks
            .iter()
            .filter(|t| !t.self_contained_data)
            .all(|t| matches!(check_track(t), Err(MediaFileError::Unsupported(_))))
    );
}
#[test]
fn rejected_limits_and_changed_file_cannot_publish_partial_decode() {
    assert!(matches!(
        MediaFileReader::open(
            &fixture("cavlc", "mp4"),
            MediaFileLimits {
                max_packet_bytes: 0,
                ..Default::default()
            }
        ),
        Err(MediaFileError::Limit(_))
    ));
    let mut reader = MediaFileReader::open(
        &fixture("cavlc", "mp4"),
        MediaFileLimits {
            max_packet_bytes: 1,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(matches!(reader.next_video(), Err(MediaFileError::Limit(_))));
    assert_eq!(reader.video_index, 0);
}
#[test]
fn hostile_mp4_truncations_and_bit_mutations_return_without_panicking() {
    let bytes = std::fs::read(fixture("cavlc", "mp4")).unwrap();
    for length in [0, 1, 7, 8, 16, 32, 128, 512, 1024] {
        let result = catch_unwind(|| skylive_mp4::open(&bytes[..length]));
        assert!(result.is_ok(), "truncation{length}");
        assert!(result.unwrap().is_err());
    }
    for index in (0..bytes.len()).step_by(257) {
        let mut changed = bytes.clone();
        changed[index] ^= 0x80;
        assert!(
            catch_unwind(|| skylive_mp4::open(&changed)).is_ok(),
            "bit mutation at{index}"
        );
    }
}

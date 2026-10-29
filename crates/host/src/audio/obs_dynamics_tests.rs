//! Literal PCM, callback partitions, retained history, native round-trip. No devices.
use super::plugin_tests::{close, filters, input, request, setup, view};
use super::*;
use skylive_core::{AudioSlot, Command, ObsExpanderDetector};
use skylive_media::{MediaTime, obs_dynamics::*};
fn limiter(threshold_db: f32) -> AudioFilter {
    AudioFilter::ObsLimiter {
        threshold_db,
        release_ms: 60,
        enabled: true,
    }
}
#[test]
fn all_three_builtin_dynamics_persist_strictly_with_one_undo_and_no_provider_toggle() {
    let (mut session, _, source) = setup();
    let before = session.snapshot();
    let chain = vec![
        AudioFilter::obs_expander(),
        AudioFilter::obs_compressor(),
        AudioFilter::obs_limiter(),
    ];
    assert!(
        chain
            .iter()
            .all(|f| before.preferences().plugin_enabled(f.plugin_id()))
    );
    filters(&mut session, source, chain.clone());
    assert_eq!(session.snapshot().revision(), before.revision() + 1);
    let bytes = crate::storage::encode(&session.snapshot()).unwrap();
    let loaded = crate::storage::decode(&bytes, true).unwrap();
    assert_eq!(
        loaded.snapshot().source(source).unwrap().audio_filters(),
        chain
    );
    session.apply(Command::Undo).unwrap();
    assert!(
        session
            .snapshot()
            .source(source)
            .unwrap()
            .audio_filters()
            .is_empty()
    );
    session.apply(Command::Redo).unwrap();
    assert_eq!(
        session.snapshot().source(source).unwrap().audio_filters(),
        chain
    );
    for bad in [
        serde_json::json!({"kind":"obs_limiter","threshold_db":1,"release_ms":60}),
        serde_json::json!({"kind":"obs_limiter","threshold_db":-6,"release_ms":0}),
        serde_json::json!({"kind":"obs_compressor","threshold_db":-18,"ratio":33,"attack_ms":6,"release_ms":60,"output_db":0}),
        serde_json::json!({"kind":"obs_compressor","threshold_db":-18,"ratio":10,"attack_ms":6,"release_ms":60,"output_db":0,"sidechain":"Input"}),
        serde_json::json!({"kind":"obs_expander","threshold_db":-40,"ratio":2,"attack_ms":10,"release_ms":50,"output_db":0,"detector":"unknown"}),
    ] {
        assert!(
            serde_json::from_value::<AudioFilter>(bad.clone()).is_err(),
            "{bad}"
        );
    }
    let stable = session.snapshot();
    assert!(
        session
            .apply(Command::SetSourceAudioFilters {
                source,
                filters: vec![limiter(f32::NAN)]
            })
            .is_err()
    );
    assert!(std::sync::Arc::ptr_eq(&stable, &session.snapshot()));
}
#[test]
fn linked_detector_uses_one_source_callback_once_even_for_duplicate_references_and_global_root() {
    let (mut session, scene, source) = setup();
    filters(&mut session, source, vec![limiter(-6.)]);
    session
        .apply(Command::AddExistingSource { scene, source })
        .unwrap();
    session
        .apply(Command::SetGlobalAudio {
            slot: AudioSlot::Mic1,
            source: Some(source),
        })
        .unwrap();
    session
        .apply(Command::SetAudio {
            source,
            gain: 0.25,
            muted: false,
        })
        .unwrap();
    let plan = derive_audio_plan(&session, &view(scene)).unwrap();
    assert_eq!(plan.sources().len(), 1);
    let pcm = [1., 0., 0., 0.5].repeat(16);
    let mut direct = ObsLimiter::new(48000, 2, Default::default()).unwrap();
    let mut expected = pcm.clone();
    direct
        .process_callback(&mut expected, &mut [0.; 32], Default::default())
        .unwrap();
    for x in &mut expected {
        *x *= 0.25;
    }
    close(
        &plan
            .mix(request(32), &[input(source, &pcm)])
            .unwrap()
            .samples,
        &expected,
    );
    let mut fragmented = ObsLimiter::new(48000, 2, Default::default()).unwrap();
    let mut wrong = pcm.clone();
    for frame in wrong.chunks_exact_mut(2) {
        fragmented
            .process_callback(frame, &mut [0.], Default::default())
            .unwrap();
    }
    assert!(
        expected
            .iter()
            .zip(wrong)
            .any(|(a, b)| (a - b * 0.25).abs() > 0.00001)
    );
}
#[test]
fn forward_gain_limiter_order_and_fader_after_filters_are_not_interchangeable() {
    let (mut session, scene, source) = setup();
    let gain = AudioFilter::ObsGain {
        db: 20.,
        enabled: true,
    };
    session
        .apply(Command::SetAudio {
            source,
            gain: 0.05,
            muted: false,
        })
        .unwrap();
    let pcm = vec![0.25; 4096 * 2];
    filters(&mut session, source, vec![gain, limiter(-6.)]);
    let a = derive_audio_plan(&session, &view(scene))
        .unwrap()
        .mix(request(4096), &[input(source, &pcm)])
        .unwrap();
    filters(&mut session, source, vec![limiter(-6.), gain]);
    let b = derive_audio_plan(&session, &view(scene))
        .unwrap()
        .mix(request(4096), &[input(source, &pcm)])
        .unwrap();
    assert!((a.samples[8190] - 0.02505936).abs() < 0.000002);
    assert_eq!(b.samples[8190], 0.125);
    assert_eq!(a.clipped_samples, [0, 0]);
}
#[test]
fn disabled_linked_history_pauses_then_resumes_and_parameter_update_keeps_envelope() {
    let (mut session, scene, source) = setup();
    let f = AudioFilter::obs_compressor();
    filters(&mut session, source, vec![f]);
    let pcm = vec![0.8; 2048];
    let mut state = AudioDspState::default();
    let mut direct = ObsCompressor::new(48000, 2, Default::default()).unwrap();
    let mut expected = pcm.clone();
    direct
        .process_callback(&mut expected, &mut [0.; 1024], Default::default())
        .unwrap();
    close(
        &derive_audio_plan(&session, &view(scene))
            .unwrap()
            .mix_on_grid_with_dsp(request(1024), &[input(source, &pcm)], 0, &mut state)
            .unwrap()
            .samples,
        &expected,
    );
    let AudioFilter::ObsCompressor {
        threshold_db,
        ratio,
        attack_ms,
        release_ms,
        output_db,
        ..
    } = f
    else {
        unreachable!()
    };
    filters(
        &mut session,
        source,
        vec![AudioFilter::ObsCompressor {
            threshold_db,
            ratio,
            attack_ms,
            release_ms,
            output_db,
            sidechain: None,
            enabled: false,
        }],
    );
    close(
        &derive_audio_plan(&session, &view(scene))
            .unwrap()
            .mix_on_grid_with_dsp(request(1024), &[input(source, &pcm)], 1024, &mut state)
            .unwrap()
            .samples,
        &pcm,
    );
    let changed = AudioFilter::ObsCompressor {
        threshold_db: -12.,
        ratio: 4.,
        attack_ms: 20,
        release_ms: 200,
        output_db: 0.,
        sidechain: None,
            enabled: true,
    };
    filters(&mut session, source, vec![changed]);
    direct.set_parameters(obs_compressor_parameters(changed).unwrap());
    let mut expected = pcm.clone();
    direct
        .process_callback(&mut expected, &mut [0.; 1024], Default::default())
        .unwrap();
    let plan = derive_audio_plan(&session, &view(scene)).unwrap();
    close(
        &plan
            .mix_on_grid_with_dsp(request(1024), &[input(source, &pcm)], 2048, &mut state)
            .unwrap()
            .samples,
        &expected,
    );
    let mut control = state.clone();
    let mut bad = pcm.clone();
    bad[2047] = f32::NAN;
    assert!(
        plan.mix_on_grid_with_dsp(request(1024), &[input(source, &bad)], 3072, &mut state)
            .is_err()
    );
    close(
        &plan
            .mix_on_grid_with_dsp(request(1024), &[input(source, &pcm)], 3072, &mut state)
            .unwrap()
            .samples,
        &plan
            .mix_on_grid_with_dsp(request(1024), &[input(source, &pcm)], 3072, &mut control)
            .unwrap()
            .samples,
    );
}
#[test]
fn historic_parameter_cut_is_two_declared_callbacks_and_reset_is_explicit() {
    let (mut session, scene, source) = setup();
    filters(&mut session, source, vec![limiter(-6.)]);
    let mut timeline = AudioRouteTimeline::new(48000).unwrap();
    timeline.push(&session, &view(scene), MediaTime(0)).unwrap();
    filters(&mut session, source, vec![limiter(-12.)]);
    timeline
        .push(&session, &view(scene), MediaTime(1_000_000))
        .unwrap();
    let spans = timeline.segments(0, 96).unwrap();
    assert_eq!(spans.iter().map(|s| s.frames).collect::<Vec<_>>(), [48, 48]);
    let pcm = [1., 0., 0., 0.5].repeat(48);
    assert_eq!(pcm.len(), 96 * 2);
    let mut state = AudioDspState::default();
    let mut direct = ObsLimiter::new(48000, 2, Default::default()).unwrap();
    let mut offset = 0;
    for (span, threshold) in spans.into_iter().zip([-6., -12.]) {
        direct.set_parameters(ObsLimiterParameters::new(threshold, 60).unwrap());
        let block = &pcm[offset * 2..(offset + span.frames) * 2];
        let mut expected = block.to_vec();
        direct
            .process_callback(&mut expected, &mut [0.; 48], Default::default())
            .unwrap();
        let actual = span
            .plan
            .mix_on_grid_with_dsp(
                AudioMixRequest {
                    advance_ns: span.advance_ns,
                    ..request(span.frames)
                },
                &[input(source, block)],
                offset as u64,
                &mut state,
            )
            .unwrap();
        close(&actual.samples, &expected);
        offset += span.frames;
    }
    state.reset_source(source);
    let block = vec![0.25; 96];
    let plan = derive_audio_plan(&session, &view(scene)).unwrap();
    close(
        &plan
            .mix_on_grid_with_dsp(request(48), &[input(source, &block)], 96, &mut state)
            .unwrap()
            .samples,
        &plan
            .mix(request(48), &[input(source, &block)])
            .unwrap()
            .samples,
    );
}
#[test]
fn expander_is_channel_independent_after_source_conversion_and_uses_source_defaults() {
    let (mut session, scene, source) = setup();
    filters(
        &mut session,
        source,
        vec![AudioFilter::ObsExpander {
            threshold_db: -40.,
            ratio: 2.,
            attack_ms: 10,
            release_ms: 50,
            output_db: 0.,
            detector: ObsExpanderDetector::Peak,
            preset: skylive_core::ObsExpanderPreset::Expander,
            enabled: true,
        }],
    );
    let plan = derive_audio_plan(&session, &view(scene)).unwrap();
    let samples = [0.5, 0.001].repeat(4096);
    let mut state = AudioDspState::default();
    let mut actual = None;
    // Five 85.3 ms callbacks permit the source's actual 50 ms gain-release
    // envelope to settle. The detector is not an instant hard gate.
    for block in 0..5 {
        actual = Some(
            plan.mix_on_grid_with_dsp(
                request(4096),
                &[input(source, &samples)],
                block * 4096,
                &mut state,
            )
            .unwrap(),
        );
    }
    let actual = actual.unwrap();
    assert_eq!(actual.samples[8190], 0.5);
    assert!((actual.samples[8191] - 0.0001).abs() < 0.000001);
}
#[test]
fn reordered_anonymous_linked_slots_binding_and_rate_changes_reset_only_affected_history() {
    let (mut session, scene, source) = setup();
    let chain = vec![
        AudioFilter::ObsCompressor {
            threshold_db: -30.,
            ratio: 4.,
            attack_ms: 1,
            release_ms: 1000,
            output_db: 0.,
            sidechain: None,
            enabled: true,
        },
        AudioFilter::ObsCompressor {
            threshold_db: -40.,
            ratio: 6.,
            attack_ms: 1,
            release_ms: 1000,
            output_db: 0.,
            sidechain: None,
            enabled: true,
        },
    ];
    filters(&mut session, source, chain.clone());
    let hot = vec![0.8; 8192];
    let mut state = AudioDspState::default();
    derive_audio_plan(&session, &view(scene))
        .unwrap()
        .mix_on_grid_with_dsp(request(4096), &[input(source, &hot)], 0, &mut state)
        .unwrap();
    let quiet = vec![0.01; 128];
    let old = derive_audio_plan(&session, &view(scene))
        .unwrap()
        .mix_on_grid_with_dsp(
            request(64),
            &[input(source, &quiet)],
            4096,
            &mut state.clone(),
        )
        .unwrap();
    let mut swapped = chain;
    swapped.reverse();
    filters(&mut session, source, swapped);
    let plan = derive_audio_plan(&session, &view(scene)).unwrap();
    let reset = plan
        .mix_on_grid_with_dsp(request(64), &[input(source, &quiet)], 4096, &mut state)
        .unwrap();
    close(
        &reset.samples,
        &plan
            .mix(request(64), &[input(source, &quiet)])
            .unwrap()
            .samples,
    );
    assert!(
        old.samples[0] < reset.samples[0] * 0.1,
        "warmed old envelope cannot follow swapped anonymous slots"
    );
    // A genuinely different stable target resets that source; same SourceId is retained.
    let binding = skylive_core::AudioCaptureBinding::new(
        skylive_core::AudioFlow::Input,
        skylive_core::AudioTarget::WindowsEndpoint {
            id: "new-stable-endpoint".into(),
        },
        false,
        false,
    )
    .unwrap();
    session
        .apply(Command::RebindAudioCaptureSource { source, binding })
        .unwrap();
    let plan = derive_audio_plan(&session, &view(scene)).unwrap();
    close(
        &plan
            .mix_on_grid_with_dsp(request(64), &[input(source, &quiet)], 4160, &mut state)
            .unwrap()
            .samples,
        &plan
            .mix(request(64), &[input(source, &quiet)])
            .unwrap()
            .samples,
    );
    let req = AudioMixRequest {
        sample_rate: 44100,
        ..request(64)
    };
    let packet = PcmInput::Samples {
        source,
        sample_rate: 44100,
        channels: 2,
        samples: &quiet,
    };
    let actual = plan
        .mix_on_grid_with_dsp(req, &[packet], 4224, &mut state)
        .unwrap();
    let packet = PcmInput::Samples {
        source,
        sample_rate: 44100,
        channels: 2,
        samples: &quiet,
    };
    close(&actual.samples, &plan.mix(req, &[packet]).unwrap().samples);
}

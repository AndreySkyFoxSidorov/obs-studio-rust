//! Literal PCM, native persistence and historical-route tests; no devices.
use super::plugin_tests::{close, filters, input, request, setup, view};
use super::*;
use skylive_core::{AudioSlot, Command};
use skylive_media::MediaTime;

#[test]
fn builtin_gain_and_polarity_run_once_before_shared_source_fader_without_early_clip() {
    let (mut session, scene, source) = setup();
    let chain = vec![
        AudioFilter::ObsGain {
            db: 20.0,
            enabled: true,
        },
        AudioFilter::obs_polarity_invert(),
    ];
    filters(&mut session, source, chain.clone());
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
            gain: 0.1,
            muted: false,
        })
        .unwrap();
    let plan = derive_audio_plan(&session, &view(scene)).unwrap();
    assert_eq!(plan.sources().len(), 1);
    let pcm = [0.25, -0.25, 0.5, -0.5];
    let mut state = AudioDspState::default();
    let actual = plan
        .mix_on_grid_with_dsp(request(2), &[input(source, &pcm)], 0, &mut state)
        .unwrap();
    close(&actual.samples, &[-0.25, 0.25, -0.5, 0.5]);
    assert_eq!(actual.clipped_samples, [0, 0]);
    assert_eq!(
        session.snapshot().source(source).unwrap().audio_filters(),
        chain
    );
    assert_eq!(session.snapshot().source(source).unwrap().gain(), 0.1);
    let saved = crate::storage::encode(&session.snapshot()).unwrap();
    let loaded = crate::storage::decode(&saved, true).unwrap();
    assert_eq!(
        loaded.snapshot().source(source).unwrap().audio_filters(),
        chain
    );
    close(
        &derive_audio_plan(&loaded, &view(scene))
            .unwrap()
            .mix(request(2), &[input(source, &pcm)])
            .unwrap()
            .samples,
        &actual.samples,
    );
    session
        .apply(Command::SetAudio {
            source,
            gain: 0.1,
            muted: true,
        })
        .unwrap();
    assert_eq!(
        derive_audio_plan(&session, &view(scene))
            .unwrap()
            .mix(request(2), &[input(source, &pcm)])
            .unwrap()
            .samples,
        [0.0; 4]
    );
}

#[test]
fn builtin_parameters_are_captured_by_history_and_strictly_deserialized() {
    let (mut session, scene, source) = setup();
    filters(
        &mut session,
        source,
        vec![
            AudioFilter::ObsGain {
                db: 20.0,
                enabled: true,
            },
            AudioFilter::obs_polarity_invert(),
        ],
    );
    session
        .apply(Command::SetAudio {
            source,
            gain: 0.1,
            muted: false,
        })
        .unwrap();
    let mut timeline = AudioRouteTimeline::new(48000).unwrap();
    timeline.push(&session, &view(scene), MediaTime(0)).unwrap();
    filters(
        &mut session,
        source,
        vec![
            AudioFilter::ObsGain {
                db: 30.0,
                enabled: false,
            },
            AudioFilter::ObsPolarityInvert { enabled: false },
        ],
    );
    timeline
        .push(&session, &view(scene), MediaTime(1_000_000))
        .unwrap();
    let pcm = [0.25, -0.5].repeat(48);
    for (span, expected) in timeline
        .segments(0, 96)
        .unwrap()
        .into_iter()
        .zip([[-0.25, 0.5], [0.025, -0.05]])
    {
        let actual = span
            .plan
            .mix_on_grid(
                AudioMixRequest {
                    advance_ns: span.advance_ns,
                    ..request(span.frames)
                },
                &[input(source, &pcm)],
                span.offset as u64,
            )
            .unwrap();
        for frame in actual.samples.chunks_exact(2) {
            close(frame, &expected);
        }
    }
    assert_eq!(
        serde_json::from_str::<AudioFilter>(r#"{"kind":"obs_gain","db":0}"#).unwrap(),
        AudioFilter::obs_gain()
    );
    assert_eq!(
        serde_json::from_str::<AudioFilter>(r#"{"kind":"obs_polarity_invert"}"#).unwrap(),
        AudioFilter::obs_polarity_invert()
    );
    for data in [
        r#"{"kind":"obs_gain","db":31}"#,
        r#"{"kind":"obs_gain","db":0,"clamp":true}"#,
        r#"{"kind":"obs_polarity_invert","channels":2}"#,
    ] {
        assert!(serde_json::from_str::<AudioFilter>(data).is_err());
    }
}

#[test]
fn late_gain_overflow_leaves_previous_dsp_history_and_output_admission_intact() {
    let (mut session, scene, source) = setup();
    filters(
        &mut session,
        source,
        vec![AudioFilter::ObsGain {
            db: 20.0,
            enabled: true,
        }],
    );
    let plan = derive_audio_plan(&session, &view(scene)).unwrap();
    let mut state = AudioDspState::default();
    let good = [0.01, -0.01];
    plan.mix_on_grid_with_dsp(request(1), &[input(source, &good)], 0, &mut state)
        .unwrap();
    assert!(
        plan.mix_on_grid_with_dsp(
            request(2),
            &[input(source, &[0.01, 0.01, 0.01, f32::MAX])],
            1,
            &mut state
        )
        .is_err()
    );
    let actual = plan
        .mix_on_grid_with_dsp(request(1), &[input(source, &good)], 1, &mut state)
        .unwrap();
    close(&actual.samples, &[0.1, -0.1]);
}

//! Actual PCM and historical-route tests for the reviewed Pan provider.
use super::*;
use skylive_core::{AUDIO_PAN_PLUGIN, AudioFlow, AudioSlot, AudioTarget, Command};
use skylive_media::MediaTime;

#[test]
fn compressor_persistent_history_shared_leaf_and_snapshot_policy_are_explicit() {
    let (mut session, scene, source) = setup();
    filters(&mut session, source, vec![AudioFilter::compressor()]);
    session
        .apply(Command::AddExistingSource { scene, source })
        .unwrap();
    session
        .apply(Command::SetGlobalAudio {
            slot: AudioSlot::Mic1,
            source: Some(source),
        })
        .unwrap();
    let plan = derive_audio_plan(&session, &view(scene)).unwrap();
    assert_eq!(plan.sources().len(), 1);
    let samples = vec![0.8; 1024 * 2];
    let mut state = AudioDspState::default();
    let first = plan
        .mix_on_grid_with_dsp(request(1024), &[input(source, &samples)], 0, &mut state)
        .unwrap();
    let mut direct = skylive_media::craft_audio::Compressor::new(
        48000,
        2,
        compressor_parameters(AudioFilter::compressor()).unwrap(),
    )
    .unwrap();
    let mut oracle = samples.clone();
    direct.process_interleaved(&mut oracle).unwrap();
    close(&first.samples, &oracle);
    let second = plan
        .mix_on_grid_with_dsp(request(1024), &[input(source, &samples)], 1024, &mut state)
        .unwrap();
    assert!(second.samples[0] < first.samples[0]);
    let snapshot = plan.mix(request(1024), &[input(source, &samples)]).unwrap();
    close(&snapshot.samples, &first.samples);
    let before = state.gain_reduction_db(source, 0).unwrap();
    let mut bad = samples.clone();
    let last = bad.len() - 1;
    bad[last] = f32::NAN;
    assert!(
        plan.mix_on_grid_with_dsp(request(1024), &[input(source, &bad)], 2048, &mut state)
            .is_err()
    );
    assert_eq!(state.gain_reduction_db(source, 0), Some(before));
}

#[test]
fn compressor_chain_order_and_global_bypass_are_real_pcm_operations() {
    let (mut session, scene, source) = setup();
    let mut boost = AudioFilter::default();
    if let AudioFilter::Pan {
        gain_left_db,
        gain_right_db,
        ..
    } = &mut boost
    {
        *gain_left_db = 6.0;
        *gain_right_db = 6.0;
    }
    filters(&mut session, source, vec![boost, AudioFilter::compressor()]);
    let before = derive_audio_plan(&session, &view(scene)).unwrap();
    filters(&mut session, source, vec![AudioFilter::compressor(), boost]);
    let after = derive_audio_plan(&session, &view(scene)).unwrap();
    let samples = vec![0.5; 2048];
    let a = before
        .mix(request(1024), &[input(source, &samples)])
        .unwrap();
    let b = after
        .mix(request(1024), &[input(source, &samples)])
        .unwrap();
    assert!(
        b.samples[2046] > a.samples[2046] * 1.3,
        "OBS forward chain order changes actual compression"
    );
    session
        .apply(Command::SetPluginEnabled {
            plugin: skylive_core::CRAFT_AUDIO_PLUGIN.into(),
            enabled: false,
        })
        .unwrap();
    let bypass = derive_audio_plan(&session, &view(scene)).unwrap();
    assert_eq!(bypass.sources()[0].filters, vec![boost]);
    assert_eq!(
        session
            .snapshot()
            .source(source)
            .unwrap()
            .audio_filters()
            .len(),
        2
    );
    let plain = bypass
        .mix(request(1), &[input(source, &[0.1, 0.1])])
        .unwrap();
    assert!(plain.samples[0] > 0.19);
}

#[test]
fn compressor_native_schema_defaults_enabled_and_rejects_unknown_or_invalid_fields() {
    let source = r#"{"kind":"compressor","threshold_db":-20,"ratio":4,"knee_db":6,"attack_ms":10,"release_ms":100,"makeup_db":0,"detector":"peak"}"#;
    assert_eq!(
        serde_json::from_str::<AudioFilter>(source).unwrap(),
        AudioFilter::compressor()
    );
    for invalid in [
        source.replace("\"ratio\":4", "\"ratio\":0"),
        source.replace("\"detector\":\"peak\"", "\"detector\":\"true_peak\""),
        source.replace("\"ratio\":4", "\"ratio\":4,\"lookahead\":5"),
    ] {
        assert!(serde_json::from_str::<AudioFilter>(&invalid).is_err());
    }
}

pub(super) fn setup() -> (Session, Id, Id) {
    let mut session = Session::new();
    let scene = session.preview();
    let item = session
        .apply(Command::AddAudioCaptureSource {
            scene,
            name: "Filtered microphone".into(),
            binding: AudioCaptureBinding::new(AudioFlow::Input, AudioTarget::Default, false, false)
                .unwrap(),
        })
        .unwrap()
        .created
        .unwrap();
    let source = session
        .snapshot()
        .scene(scene)
        .unwrap()
        .items()
        .iter()
        .find(|entry| entry.id() == item)
        .unwrap()
        .source();
    enable(&mut session, true);
    (session, scene, source)
}
fn enable(session: &mut Session, enabled: bool) {
    session
        .apply(Command::SetPluginEnabled {
            plugin: AUDIO_PAN_PLUGIN.into(),
            enabled,
        })
        .unwrap();
}
pub(super) fn filters(session: &mut Session, source: Id, filters: Vec<AudioFilter>) {
    session
        .apply(Command::SetSourceAudioFilters { source, filters })
        .unwrap();
}
fn pan(left: f32, right: f32) -> AudioFilter {
    AudioFilter::Pan {
        pan_left: left,
        pan_right: right,
        gain_left_db: 0.0,
        gain_right_db: 0.0,
        invert_left: false,
        invert_right: false,
    }
}
pub(super) fn view(scene: Id) -> FrameComposition {
    FrameComposition::Scene(FrameScene::Live(scene))
}
pub(super) fn request(frames: usize) -> AudioMixRequest {
    AudioMixRequest {
        sample_rate: 48000,
        channels: 2,
        frames,
        advance_ns: 0,
    }
}
pub(super) fn input(source: Id, samples: &[f32]) -> PcmInput<'_> {
    PcmInput::Samples {
        source,
        sample_rate: 48000,
        channels: 2,
        samples,
    }
}
pub(super) fn close(actual: &[f32], expected: &[f32]) {
    assert_eq!(actual.len(), expected.len());
    for (actual, expected) in actual.iter().zip(expected) {
        assert!(
            (actual - expected).abs() < 0.000002,
            "{actual} != {expected}"
        );
    }
}

#[test]
fn pan_runs_once_for_shared_scene_and_global_source_before_fader_and_final_clipping() {
    let (mut session, scene, source) = setup();
    let other = session
        .apply(Command::AddScene {
            name: "Shared scene".into(),
        })
        .unwrap()
        .created
        .unwrap();
    session
        .apply(Command::AddExistingSource {
            scene: other,
            source,
        })
        .unwrap();
    session
        .apply(Command::SetGlobalAudio {
            slot: AudioSlot::Mic1,
            source: Some(source),
        })
        .unwrap();
    filters(
        &mut session,
        source,
        vec![AudioFilter::Pan {
            pan_left: 0.0,
            pan_right: 0.0,
            gain_left_db: 20.0,
            gain_right_db: 20.0,
            invert_left: false,
            invert_right: false,
        }],
    );
    session
        .apply(Command::SetAudio {
            source,
            gain: 0.1,
            muted: false,
        })
        .unwrap();
    let output = FrameComposition::Fade {
        from: FrameScene::Live(scene),
        to: FrameScene::Live(other),
        elapsed_ns: 50,
        duration_ns: 100,
        epoch: 2,
    };
    let plan = derive_audio_plan(&session, &output).unwrap();
    assert_eq!(plan.sources().len(), 1);
    assert_eq!(plan.sources()[0].filters.len(), 1);
    assert!(plan.sources()[0].routes.global);
    assert!(plan.sources()[0].routes.from && plan.sources()[0].routes.to);
    let mixed = plan
        .mix(request(2), &[input(source, &[0.25, 0.75, 0.5, -0.5])])
        .unwrap();
    // Pan creates +/-7.071... before the fader. Clipping or filtering a shared
    // source twice would both produce a different output from this oracle.
    close(&mixed.samples, &[0.70710677, 0.70710677, 0.0, 0.0]);
    assert_eq!(mixed.clipped_samples, [0, 0]);
    assert!((mixed.source_meters[0].channels[0].peak - 0.70710677).abs() < 0.000002);
    session
        .apply(Command::SetAudio {
            source,
            gain: 0.1,
            muted: true,
        })
        .unwrap();
    assert_eq!(
        derive_audio_plan(&session, &output)
            .unwrap()
            .mix(request(1), &[input(source, &[0.25, 0.75])])
            .unwrap()
            .samples,
        [0.0, 0.0]
    );
}

#[test]
fn filter_list_uses_obs_forward_order_and_module_disable_bypasses_without_erasing_authored_chain() {
    let (mut session, scene, source) = setup();
    let chain = vec![pan(-100.0, -100.0), pan(100.0, -100.0)];
    filters(&mut session, source, chain.clone());
    let enabled = derive_audio_plan(&session, &view(scene)).unwrap();
    // UI order [sum-left, swap] executes forward, ending at right. The literal
    // PCM oracle is preserved from the legacy fixture by reversing its authored list.
    close(
        &enabled
            .mix(request(1), &[input(source, &[0.25, 0.5])])
            .unwrap()
            .samples,
        &[0.0, 0.75],
    );
    enable(&mut session, false);
    assert_eq!(
        session.snapshot().source(source).unwrap().audio_filters(),
        chain
    );
    let disabled = derive_audio_plan(&session, &view(scene)).unwrap();
    assert!(disabled.sources()[0].filters.is_empty());
    assert_eq!(
        disabled
            .mix(request(1), &[input(source, &[0.25, 0.5])])
            .unwrap()
            .samples,
        [0.25, 0.5]
    );
    close(
        &enabled
            .mix(request(1), &[input(source, &[0.25, 0.5])])
            .unwrap()
            .samples,
        &[0.0, 0.75],
    );
    enable(&mut session, true);
    assert_eq!(
        session.snapshot().source(source).unwrap().audio_filters(),
        chain
    );
    close(
        &derive_audio_plan(&session, &view(scene))
            .unwrap()
            .mix(request(1), &[input(source, &[0.25, 0.5])])
            .unwrap()
            .samples,
        &[0.0, 0.75],
    );
}

#[test]
fn historical_route_changes_and_global_activation_capture_filters_at_exact_sample_boundaries() {
    let (mut session, scene, source) = setup();
    filters(&mut session, source, vec![pan(-100.0, -100.0)]);
    let mut timeline = AudioRouteTimeline::new(48000).unwrap();
    assert!(timeline.push(&session, &view(scene), MediaTime(0)).unwrap());
    filters(&mut session, source, vec![pan(100.0, -100.0)]);
    assert!(
        timeline
            .push(&session, &view(scene), MediaTime(1_000_000))
            .unwrap(),
        "filter settings are part of semantic route identity"
    );
    enable(&mut session, false);
    assert!(
        timeline
            .push(&session, &view(scene), MediaTime(2_000_000))
            .unwrap(),
        "module activation is captured in the effective chain"
    );
    assert_eq!(timeline.event_count(), 3);
    enable(&mut session, true);
    let spans = timeline.segments(0, 144).unwrap();
    assert_eq!(
        spans
            .iter()
            .map(|span| (span.offset, span.frames))
            .collect::<Vec<_>>(),
        [(0, 48), (48, 48), (96, 48)]
    );
    let pcm: Vec<_> = (0..48).flat_map(|_| [0.25, 0.5]).collect();
    for (span, expected) in spans.iter().zip([[0.75, 0.0], [0.5, 0.25], [0.25, 0.5]]) {
        let mixed = span
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
        for frame in mixed.samples.chunks_exact(2) {
            close(frame, &expected);
        }
    }
    assert!(
        spans[2].plan.sources()[0].filters.is_empty(),
        "reenabling the current module cannot rewrite retained historical PCM semantics"
    );
}

#[test]
fn duplicate_source_metadata_detects_filter_conflict_and_invalid_plans_fail_admission() {
    let (mut session, scene, source) = setup();
    filters(&mut session, source, vec![pan(-100.0, -100.0)]);
    let original = session.snapshot().source(source).unwrap().clone();
    let mut sources = Vec::new();
    insert_source(
        &original,
        &session.snapshot(),
        AudioRoutes {
            from: true,
            ..Default::default()
        },
        &mut sources,
    )
    .unwrap();
    filters(&mut session, source, vec![pan(100.0, -100.0)]);
    let snapshot = session.snapshot();
    assert_eq!(
        insert_source(
            snapshot.source(source).unwrap(),
            &snapshot,
            AudioRoutes {
                to: true,
                ..Default::default()
            },
            &mut sources
        ),
        Err(AudioError::InconsistentSource(source))
    );
    let mut invalid = derive_audio_plan(&session, &view(scene)).unwrap();
    invalid.sources[0].filters = vec![pan(f32::NAN, 0.0)];
    assert_eq!(
        invalid.mix_with_limits(
            request(1),
            &[input(source, &[0.25, 0.5])],
            AudioMixLimits { max_total_bytes: 1 }
        ),
        // Typed filter admission rejects NaN before allocating a mix buffer,
        // even when the separately supplied memory budget is insufficient.
        Err(AudioError::InvalidRequest("invalid source audio filter"))
    );
    invalid.sources[0].filters = vec![pan(0.0, 0.0); MAX_SOURCE_AUDIO_FILTERS + 1];
    assert!(matches!(
        invalid.mix(request(1), &[input(source, &[0.25, 0.5])]),
        Err(AudioError::Limit(_))
    ));
    invalid.sources[0].filters = vec![pan(0.0, 0.0)];
    invalid.sources[0].muted = true;
    assert_eq!(
        invalid.mix(request(1), &[input(source, &[0.25, f32::NAN])]),
        Err(AudioError::NonFiniteSample { source, index: 1 })
    );
}

#[test]
fn same_kind_compressor_reorder_resets_anonymous_position_history() {
    let (mut session, scene, source) = setup();
    let mut first = AudioFilter::compressor();
    let mut second = first;
    if let AudioFilter::Compressor {
        threshold_db,
        attack_ms,
        ..
    } = &mut first
    {
        *threshold_db = -40.0;
        *attack_ms = 2.0;
    }
    if let AudioFilter::Compressor {
        threshold_db,
        ratio,
        attack_ms,
        ..
    } = &mut second
    {
        *threshold_db = -10.0;
        *ratio = 2.0;
        *attack_ms = 5.0;
    }
    // Preserve the original executed sequence after the schema3/UI-order fix:
    // the gentler -10dB stage must precede -40dB so both carry attenuation.
    filters(&mut session, source, vec![second, first]);
    let plan = derive_audio_plan(&session, &view(scene)).unwrap();
    let samples = vec![0.8; 2048];
    let mut state = AudioDspState::default();
    for block in 0..8 {
        plan.mix_on_grid_with_dsp(
            request(1024),
            &[input(source, &samples)],
            block * 1024,
            &mut state,
        )
        .unwrap();
    }
    for index in 0..2 {
        assert!(
            state.gain_reduction_db(source, index).unwrap() < -1.0,
            "both compressor positions must contain real attenuation history"
        );
    }
    let mut retained_history = state.clone();
    filters(&mut session, source, vec![first, second]);
    let reordered = derive_audio_plan(&session, &view(scene)).unwrap();
    let actual = reordered
        .mix_on_grid_with_dsp(request(1024), &[input(source, &samples)], 8192, &mut state)
        .unwrap();
    let fresh = reordered
        .mix_on_grid_with_dsp(
            request(1024),
            &[input(source, &samples)],
            8192,
            &mut AudioDspState::default(),
        )
        .unwrap();
    close(&actual.samples, &fresh.samples);
    // Two ordinary parameter edits keep positional histories. This control
    // proves the warm fixture distinguishes that behavior from a reorder reset.
    let mut parameter_edit = reordered.clone();
    if let AudioFilter::Compressor { threshold_db, .. } = &mut parameter_edit.sources[0].filters[0]
    {
        *threshold_db -= 0.001;
    }
    retained_history
        .prepare(&parameter_edit, request(1024), 8192)
        .unwrap();
    let continued = reordered
        .mix_on_grid_with_dsp(
            request(1024),
            &[input(source, &samples)],
            8192,
            &mut retained_history,
        )
        .unwrap();
    assert!(
        (continued.samples[0] - fresh.samples[0]).abs() > 0.01,
        "the fresh-state oracle must detect incorrectly retained positional histories"
    );
}

#[test]
fn gate_shared_global_routes_filter_before_fader_and_bypass_without_losing_authored_settings() {
    let (mut session, scene, source) = setup();
    let gate = AudioFilter::Gate {
        threshold_db: -20.0,
        ratio: 2.0,
        range_db: 60.0,
        attack_ms: 1.0,
        hold_ms: 0.0,
        release_ms: 1.0,
        enabled: true,
    };
    filters(&mut session, source, vec![gate]);
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
            gain: 0.5,
            muted: false,
        })
        .unwrap();
    let plan = derive_audio_plan(&session, &view(scene)).unwrap();
    assert_eq!(plan.sources().len(), 1);
    let pcm = [0.01; 2048];
    let mut state = AudioDspState::default();
    let output = plan
        .mix_on_grid_with_dsp(request(1024), &[input(source, &pcm)], 0, &mut state)
        .unwrap();
    // Input=-40dB, threshold=-20, ratio=2 => -60dB before the source
    // half-gain fader. Duplicated/global references are one processor/mix root.
    assert!((output.samples[2046] - 0.0005).abs() < 0.000001);
    assert!((state.gain_reduction_db(source, 0).unwrap() + 20.0).abs() < 0.001);
    let mut disabled = gate;
    if let AudioFilter::Gate { enabled, .. } = &mut disabled {
        *enabled = false;
    }
    filters(&mut session, source, vec![disabled]);
    let bypass = derive_audio_plan(&session, &view(scene))
        .unwrap()
        .mix(request(1024), &[input(source, &pcm)])
        .unwrap();
    assert!(bypass.samples.iter().all(|&sample| sample == 0.005));
    session
        .apply(Command::SetPluginEnabled {
            plugin: skylive_core::CRAFT_AUDIO_PLUGIN.into(),
            enabled: false,
        })
        .unwrap();
    let bypass = derive_audio_plan(&session, &view(scene)).unwrap();
    assert!(bypass.sources()[0].filters.is_empty());
    assert_eq!(
        session.snapshot().source(source).unwrap().audio_filters(),
        &[disabled]
    );
    let unfiltered = bypass.mix(request(1024), &[input(source, &pcm)]).unwrap();
    assert!(unfiltered.samples.iter().all(|&sample| sample == 0.005));
}

#[test]
fn simple_eq_actual_project_pcm_has_literal_six_db_tone_and_post_filter_source_meter() {
    let (mut session, scene, source) = setup();
    let mut eq = AudioFilter::simple_eq();
    if let AudioFilter::SimpleEq { mid_db, .. } = &mut eq {
        *mid_db = 6.0;
    }
    filters(&mut session, source, vec![eq]);
    session
        .apply(Command::SetAudio {
            source,
            gain: 0.5,
            muted: false,
        })
        .unwrap();
    let plan = derive_audio_plan(&session, &view(scene)).unwrap();
    let mut state = AudioDspState::default();
    let mut sine = 0.0f64;
    let mut cosine = 0.0f64;
    let mut count = 0usize;
    for block in 0..8 {
        let start = block * 1024;
        let pcm: Vec<_> = (start..start + 1024)
            .flat_map(|frame| {
                let sample = (0.1
                    * (2.0 * std::f64::consts::PI * 1000.0 * frame as f64 / 48000.0).sin())
                    as f32;
                [sample, sample]
            })
            .collect();
        let mixed = plan
            .mix_on_grid_with_dsp(request(1024), &[input(source, &pcm)], start, &mut state)
            .unwrap();
        if block >= 4 {
            for (offset, frame) in mixed.samples.chunks_exact(2).enumerate() {
                let phase =
                    2.0 * std::f64::consts::PI * 1000.0 * (start + offset as u64) as f64 / 48000.0;
                sine += f64::from(frame[0]) * phase.sin();
                cosine += f64::from(frame[0]) * phase.cos();
                count += 1;
                assert_eq!(frame[0], frame[1]);
            }
            // One post-filter source meter, after the canonical half fader.
            assert_eq!(mixed.source_meters.len(), 1);
            assert!((mixed.source_meters[0].channels[0].peak - 0.0997631).abs() < 0.00001);
        }
    }
    // Sinusoidal projection over 4096 samples has bounded endpoint leakage.
    let amplitude = 2.0 * sine.hypot(cosine) / count as f64;
    assert!((amplitude - 0.09976311575).abs() < 0.0003);
    let mut disabled = eq;
    if let AudioFilter::SimpleEq { enabled, .. } = &mut disabled {
        *enabled = false;
    }
    filters(&mut session, source, vec![disabled]);
    let pcm = [0.125, -0.25];
    let bypass = derive_audio_plan(&session, &view(scene))
        .unwrap()
        .mix(request(1), &[input(source, &pcm)])
        .unwrap();
    assert_eq!(bypass.samples, vec![0.0625, -0.125]);
}

#[test]
fn gate_eq_serde_and_atomic_source_history_keep_validation_and_default_enabled() {
    let (mut session, _, source) = setup();
    for filter in [AudioFilter::gate(), AudioFilter::simple_eq()] {
        let value = serde_json::to_value(filter).unwrap();
        assert_eq!(
            serde_json::from_value::<AudioFilter>(value.clone()).unwrap(),
            filter
        );
        let mut legacy = value.clone();
        legacy.as_object_mut().unwrap().remove("enabled");
        assert_eq!(
            serde_json::from_value::<AudioFilter>(legacy).unwrap(),
            filter
        );
        let mut unknown = value;
        unknown
            .as_object_mut()
            .unwrap()
            .insert("lookahead".into(), serde_json::json!(1));
        assert!(serde_json::from_value::<AudioFilter>(unknown).is_err());
    }
    let original = session.snapshot().source(source).unwrap().clone();
    let revision = session.snapshot().revision();
    let mut invalid = AudioFilter::simple_eq();
    if let AudioFilter::SimpleEq { mid_q, .. } = &mut invalid {
        *mid_q = 0.0;
    }
    assert!(
        session
            .apply(Command::SetSourceAudioFilters {
                source,
                filters: vec![invalid]
            })
            .is_err()
    );
    assert_eq!(session.snapshot().revision(), revision);
    assert_eq!(session.snapshot().source(source).unwrap(), &original);
    let chain = vec![AudioFilter::gate(), AudioFilter::simple_eq()];
    filters(&mut session, source, chain.clone());
    let changed = session.snapshot().revision();
    filters(&mut session, source, chain.clone());
    assert_eq!(session.snapshot().revision(), changed);
    session.apply(Command::Undo).unwrap();
    assert_eq!(session.snapshot().source(source).unwrap(), &original);
    session.apply(Command::Redo).unwrap();
    assert_eq!(
        session.snapshot().source(source).unwrap().audio_filters(),
        chain
    );
}

#[test]
fn eq_output_overflow_rejects_without_advancing_other_source_filter_history() {
    let (mut session, scene, source) = setup();
    filters(&mut session, source, vec![AudioFilter::gate()]);
    let plan = derive_audio_plan(&session, &view(scene)).unwrap();
    let mut state = AudioDspState::default();
    plan.mix_on_grid_with_dsp(request(1), &[input(source, &[0.001, 0.001])], 0, &mut state)
        .unwrap();
    let before = state.gain_reduction_db(source, 0);
    let mut bad = plan.clone();
    let mut eq = AudioFilter::simple_eq();
    if let AudioFilter::SimpleEq { output_db, .. } = &mut eq {
        *output_db = 24.0;
    }
    bad.sources[0].filters = vec![eq];
    bad.sources[0].muted = true;
    let pcm = [f32::MAX; 2];
    assert!(matches!(
        bad.mix_on_grid_with_dsp(request(1), &[input(source, &pcm)], 1, &mut state),
        Err(AudioError::Eq(
            skylive_media::craft_audio::EqError::InvalidSample
        ))
    ));
    assert_eq!(state.gain_reduction_db(source, 0), before);
    assert_eq!(pcm, [f32::MAX; 2]);
    let resumed = plan
        .mix_on_grid_with_dsp(request(1), &[input(source, &[0.001, 0.001])], 1, &mut state)
        .unwrap();
    assert!(resumed.samples[0] < 0.001);
}

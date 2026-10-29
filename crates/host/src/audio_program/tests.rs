use super::*;
use crate::transition::FrameScene;
use skylive_core::{AudioFlow, AudioSlot, AudioTarget, Command, SpeakerLayout};

#[test]
fn historical_block_union_accepts_128_sources_rejects_129_routes_and_reserves_metadata() {
    use crate::audio::MAX_AUDIO_BLOCK_SOURCES;
    let mut session = Session::new();
    let a = session.preview();
    let b = scene(&mut session, "Union B");
    let c = scene(&mut session, "Union C");
    let shared = binding("Union128");
    for (target, label) in [(a, "A"), (b, "B")] {
        for index in 0..64 {
            add(&mut session, target, &format!("{label}{index}"), &shared);
        }
    }
    add(&mut session, c, "One more", &shared);
    let mut program = engine(&session);
    program.route(&session, &view(a), MediaTime(0)).unwrap();
    program
        .route(&session, &view(b), MediaTime(10_000_000))
        .unwrap();
    assert_eq!(program.required_bindings().len(), MAX_AUDIO_BLOCK_SOURCES);
    let before = status(&program);
    assert!(matches!(
        program.route(&session, &view(c), MediaTime(20_000_000)),
        Err(ProgramAudioError::Routing(crate::audio::AudioError::Limit(
            _
        )))
    ));
    assert_eq!(
        status(&program),
        before,
        "129th retained source is rejected before timeline publication"
    );
    push(&mut program, &shared, 1, 0, stereo(48000), &[0.01; 2048]);
    let block = program.render(0, 1024).unwrap();
    assert_eq!(block.source_meters.len(), MAX_AUDIO_BLOCK_SOURCES);
    assert_eq!(block.source_meters.capacity(), block.source_meters.len());
    assert_eq!(block.gaps.capacity(), block.gaps.len());
    assert!(
        block
            .samples
            .iter()
            .all(|&sample| (sample - 0.64).abs() < 0.000001),
        "each half mixes64 actual sources, never128simultaneously"
    );
    assert!(block.gaps.is_empty());
    let settings = session.snapshot().audio();
    let reserved = AudioProgram::new(settings, ProgramAudioLimits::default())
        .unwrap()
        .status()
        .accounted_bytes;
    assert!(render_metadata_bytes(2).unwrap() >= 128 * std::mem::size_of::<SourceMeter>());
    assert_eq!(
        render_metadata_bytes(8).unwrap() - render_metadata_bytes(2).unwrap(),
        128 * 6 * std::mem::size_of::<ChannelMeter>()
    );
    assert!(matches!(
        AudioProgram::new(
            settings,
            ProgramAudioLimits {
                max_bytes: reserved - 1,
                ..Default::default()
            }
        ),
        Err(ProgramAudioError::Limit(_))
    ));
    assert!(
        AudioProgram::new(
            settings,
            ProgramAudioLimits {
                max_bytes: reserved,
                ..Default::default()
            }
        )
        .is_ok()
    );
}

#[test]
fn gate_eq_historical_module_controls_partition_and_native_reset_are_source_owned() {
    use skylive_core::AudioFilter;
    let mut session = Session::new();
    let scene = session.preview();
    let binding = binding("Gate EQ history");
    let source = add(&mut session, scene, "Craft voice", &binding);
    let gate = AudioFilter::Gate {
        threshold_db: -20.0,
        ratio: 2.0,
        range_db: 60.0,
        attack_ms: 1.0,
        hold_ms: 0.0,
        release_ms: 1.0,
        enabled: true,
    };
    let mut eq = AudioFilter::simple_eq();
    if let AudioFilter::SimpleEq { mid_db, .. } = &mut eq {
        *mid_db = 6.0;
    }
    session
        .apply(Command::SetSourceAudioFilters {
            source,
            filters: vec![eq, gate],
        })
        .unwrap();
    let mut one = engine(&session);
    let mut split = engine(&session);
    for program in [&mut one, &mut split] {
        program.route(&session, &view(scene), MediaTime(0)).unwrap();
    }
    // Historical route metadata must keep the first interval enabled even when
    // the current canonical module preference has already changed.
    session
        .apply(Command::SetPluginEnabled {
            plugin: skylive_core::CRAFT_AUDIO_PLUGIN.into(),
            enabled: false,
        })
        .unwrap();
    for program in [&mut one, &mut split] {
        program
            .route(&session, &view(scene), MediaTime(20_000_000))
            .unwrap();
    }
    let pcm: Vec<_> = (0..1920)
        .flat_map(|frame| {
            let value = (0.01
                * (2.0 * std::f64::consts::PI * 1000.0 * frame as f64 / 48000.0).sin())
                as f32;
            [value, value]
        })
        .collect();
    for program in [&mut one, &mut split] {
        push(program, &binding, 1, 0, stereo(48000), &pcm);
    }
    let mut whole = one.render(0, 1024).unwrap().samples;
    whole.extend(one.render(1024, 896).unwrap().samples);
    let mut chunks = Vec::new();
    for start in (0..1920).step_by(120) {
        chunks.extend(split.render(start, 120).unwrap().samples);
    }
    assert_eq!(whole, chunks);
    assert_eq!(
        &whole[1920..],
        &pcm[1920..],
        "module-off future route bypasses both kernels exactly"
    );
    assert!(
        whole[..1920]
            .iter()
            .zip(&pcm[..1920])
            .any(|(actual, input)| (actual - input).abs() > 0.001)
    );
    session
        .apply(Command::SetPluginEnabled {
            plugin: skylive_core::CRAFT_AUDIO_PLUGIN.into(),
            enabled: true,
        })
        .unwrap();
    split
        .route(&session, &view(scene), MediaTime(40_000_000))
        .unwrap();
    let following = vec![0.01; 960];
    push(
        &mut split,
        &binding,
        2,
        40_000_000,
        stereo(48000),
        &following,
    );
    let restarted = split.render(1920, 480).unwrap().samples;
    let mut direct_eq = skylive_media::craft_audio::SimpleEq::new(
        48000,
        2,
        crate::audio::simple_eq_parameters(eq).unwrap(),
    )
    .unwrap();
    let mut direct_gate = skylive_media::craft_audio::Gate::new(
        48000,
        2,
        crate::audio::gate_parameters(gate).unwrap(),
    )
    .unwrap();
    let mut expected = following;
    direct_eq.process_interleaved(&mut expected).unwrap();
    direct_gate.process_interleaved(&mut expected).unwrap();
    assert_eq!(
        restarted, expected,
        "generation change starts one fresh top-to-bottom filter chain"
    );
    assert!(
        restarted[0] > 0.009,
        "fresh gate has no hidden attenuation carried across native restart"
    );
    assert!(
        restarted[958] < 0.002,
        "fixture must contain warm attenuation before the next native restart"
    );
    let following = vec![0.01; 200];
    push(
        &mut split,
        &binding,
        3,
        50_000_000,
        stereo(48000),
        &following,
    );
    let restarted = split.render(2400, 100).unwrap().samples;
    direct_eq.reset();
    direct_gate.reset();
    let mut expected = following;
    direct_eq.process_interleaved(&mut expected).unwrap();
    direct_gate.process_interleaved(&mut expected).unwrap();
    assert_eq!(
        restarted, expected,
        "native generation alone must clear the now-warm Gate and EQ histories"
    );
}

#[test]
fn compressor_keeps_state_across_blocks_historical_controls_and_resets_on_native_generation() {
    let mut session = Session::new();
    let scene = session.preview();
    let binding = binding("Compressor history");
    let source = add(&mut session, scene, "Voice", &binding);
    session
        .apply(Command::SetSourceAudioFilters {
            source,
            filters: vec![skylive_core::AudioFilter::compressor()],
        })
        .unwrap();
    let mut one = engine(&session);
    let mut split = engine(&session);
    one.route(&session, &view(scene), MediaTime(0)).unwrap();
    split.route(&session, &view(scene), MediaTime(0)).unwrap();
    let mut changed = skylive_core::AudioFilter::compressor();
    if let skylive_core::AudioFilter::Compressor { threshold_db, .. } = &mut changed {
        *threshold_db = -30.0;
    }
    session
        .apply(Command::SetSourceAudioFilters {
            source,
            filters: vec![changed],
        })
        .unwrap();
    one.route(&session, &view(scene), MediaTime(10_000_000))
        .unwrap();
    split
        .route(&session, &view(scene), MediaTime(10_000_000))
        .unwrap();
    let pcm = vec![0.5; 960 * 2];
    push(&mut one, &binding, 1, 0, stereo(48000), &pcm);
    push(&mut split, &binding, 1, 0, stereo(48000), &pcm);
    let whole = one.render(0, 960).unwrap();
    let mut chunks = split.render(0, 480).unwrap().samples;
    chunks.extend(split.render(480, 480).unwrap().samples);
    assert_eq!(whole.samples, chunks);
    assert!(whole.samples[958] < whole.samples[0]);
    assert!(whole.samples[1918] < whole.samples[958]);
    let following = vec![0.5; 100 * 2];
    push(
        &mut split,
        &binding,
        2,
        20_000_000,
        stereo(48000),
        &following,
    );
    let restarted = split.render(960, 100).unwrap();
    let mut direct = skylive_media::craft_audio::Compressor::new(
        48000,
        2,
        crate::audio::compressor_parameters(changed).unwrap(),
    )
    .unwrap();
    let mut oracle = following;
    direct.process_interleaved(&mut oracle).unwrap();
    assert_eq!(
        restarted.samples, oracle,
        "new native generation starts one fresh compressor history"
    );
}

fn binding(name: &str) -> AudioCaptureBinding {
    AudioCaptureBinding::new(
        AudioFlow::Input,
        AudioTarget::WindowsEndpoint { id: name.into() },
        true,
        false,
    )
    .unwrap()
}
fn add(session: &mut Session, scene: Id, name: &str, binding: &AudioCaptureBinding) -> Id {
    let created = session
        .apply(Command::AddAudioCaptureSource {
            scene,
            name: name.into(),
            binding: binding.clone(),
        })
        .unwrap()
        .created
        .unwrap();
    session
        .snapshot()
        .scene(scene)
        .unwrap()
        .items()
        .iter()
        .find(|item| item.id() == created)
        .unwrap()
        .source()
}
fn scene(session: &mut Session, name: &str) -> Id {
    session
        .apply(Command::AddScene { name: name.into() })
        .unwrap()
        .created
        .unwrap()
}
fn view(scene: Id) -> FrameComposition {
    FrameComposition::Scene(FrameScene::Live(scene))
}
fn mono(rate: u32) -> NativeAudioFormat {
    NativeAudioFormat {
        sample_rate: rate,
        channels: 1,
        channel_mask: 4,
    }
}
fn stereo(rate: u32) -> NativeAudioFormat {
    NativeAudioFormat {
        sample_rate: rate,
        channels: 2,
        channel_mask: 3,
    }
}
fn engine(session: &Session) -> AudioProgram {
    AudioProgram::new(session.snapshot().audio(), ProgramAudioLimits::default()).unwrap()
}
fn push(
    program: &mut AudioProgram,
    binding: &AudioCaptureBinding,
    generation: u64,
    time: i128,
    format: NativeAudioFormat,
    samples: &[f32],
) -> AudioIngestReport {
    program
        .ingest(ProgramInputPacket {
            identity: program.identity().unwrap(),
            binding,
            stream_generation: generation,
            sequence: program.status().data_generation + 1,
            first_media_ns: time,
            received_media_ns: time,
            format,
            samples,
            flags: NativePacketFlags::default(),
        })
        .unwrap()
}
fn status(program: &AudioProgram) -> serde_json::Value {
    serde_json::to_value(program.status()).unwrap()
}
fn missing(block: &ProgramAudioBlock, source: Id) -> usize {
    block
        .gaps
        .iter()
        .find(|gap| gap.source == source)
        .map_or(0, |gap| gap.missing_frames)
}

#[test]
fn shared_native_binding_keeps_source_gains_independent_and_does_not_clip_intermediate_audio() {
    let mut session = Session::new();
    let live = session.preview();
    let binding = binding("Shared");
    let a = add(&mut session, live, "A audio", &binding);
    let b = add(&mut session, live, "B audio", &binding);
    session
        .apply(Command::SetAudio {
            source: a,
            gain: 1.0,
            muted: false,
        })
        .unwrap();
    session
        .apply(Command::SetAudio {
            source: b,
            gain: 0.5,
            muted: false,
        })
        .unwrap();
    let mut program = engine(&session);
    program.route(&session, &view(live), MediaTime(0)).unwrap();
    let report = push(
        &mut program,
        &binding,
        1,
        0,
        stereo(48000),
        &[0.25, -0.5, 1.0, 0.0],
    );
    assert_eq!(report.output_frames, 2);
    assert_eq!(program.status().input_groups, 1);
    let block = program.render(0, 2).unwrap();
    assert_eq!(block.samples, [0.375, -0.75, 1.0, 0.0]);
    assert_eq!(block.clipped_samples, [1, 0]);
    assert!(block.gaps.is_empty());
    let a_meter = block
        .source_meters
        .iter()
        .find(|meter| meter.source == a)
        .unwrap();
    let b_meter = block
        .source_meters
        .iter()
        .find(|meter| meter.source == b)
        .unwrap();
    assert_eq!(a_meter.channels[0].peak, 1.0);
    assert_eq!(b_meter.channels[0].peak, 0.5);
    assert_eq!(block.program_meters[1].rms, (0.75f64.powi(2) / 2.0).sqrt());
    assert!(!program.status().continuous_native_transport);
}

#[test]
fn timestamped_cut_and_gain_changes_take_effect_inside_one_1024_frame_block() {
    let mut session = Session::new();
    let a_scene = session.preview();
    let b_scene = scene(&mut session, "B scene");
    let a_bind = binding("A");
    let b_bind = binding("B");
    let a = add(&mut session, a_scene, "A audio", &a_bind);
    let b = add(&mut session, b_scene, "B audio", &b_bind);
    let mut program = engine(&session);
    program
        .route(&session, &view(a_scene), MediaTime(0))
        .unwrap();
    program
        .route(&session, &view(b_scene), MediaTime(10_000_001))
        .unwrap(); // sample 481, never sample 480
    session
        .apply(Command::SetAudio {
            source: b,
            gain: 0.5,
            muted: false,
        })
        .unwrap();
    program
        .route(&session, &view(b_scene), MediaTime(15_000_000))
        .unwrap(); // sample 720
    push(&mut program, &a_bind, 1, 0, mono(48000), &vec![0.25; 1024]);
    push(&mut program, &b_bind, 1, 0, mono(48000), &vec![0.75; 1024]);
    let block = program.render(0, 1024).unwrap();
    for (index, frame) in block.samples.chunks_exact(2).enumerate() {
        let expected = if index < 481 {
            0.25
        } else if index < 720 {
            0.75
        } else {
            0.375
        };
        assert_eq!(frame, [expected, expected], "frame {index}");
    }
    assert!(block.gaps.is_empty());
    assert_eq!(block.clipped_samples, [0, 0]);
    assert_eq!(
        block
            .source_meters
            .iter()
            .find(|m| m.source == a)
            .unwrap()
            .channels[0]
            .rms,
        0.25 * (481.0f64 / 1024.0).sqrt()
    );
    assert_eq!(program.status().cursor, Some(1024));
}

#[test]
fn mono_to_stereo_is_literal_obs_duplication_and_explicit_silence_is_valid_data() {
    let mut session = Session::new();
    let live = session.preview();
    let binding = binding("Mono");
    let source = add(&mut session, live, "Mono audio", &binding);
    let mut program = engine(&session);
    program.route(&session, &view(live), MediaTime(0)).unwrap();
    push(
        &mut program,
        &binding,
        1,
        0,
        mono(48000),
        &[0.25, -0.5, 0.0, 0.75],
    );
    let block = program.render(0, 4).unwrap();
    assert_eq!(
        block.samples,
        [0.25, 0.25, -0.5, -0.5, 0.0, 0.0, 0.75, 0.75]
    );
    assert_eq!(missing(&block, source), 0);
    program
        .ingest(ProgramInputPacket {
            identity: program.identity().unwrap(),
            binding: &binding,
            stream_generation: 1,
            sequence: program.status().data_generation + 1,
            first_media_ns: 83_333,
            received_media_ns: 83_333,
            format: mono(48000),
            samples: &[0.0; 4],
            flags: NativePacketFlags {
                silent: true,
                ..Default::default()
            },
        })
        .unwrap();
    let block = program.render(4, 4).unwrap();
    assert_eq!(block.samples, [0.0; 8]);
    assert_eq!(missing(&block, source), 0);
}

#[test]
fn resampled_16k_tone_has_exact_44100_count_and_independent_1khz_waveform_oracle() {
    let mut session = Session::new();
    session
        .apply(Command::SetAudioSettings {
            settings: AudioSettings::new(44100, SpeakerLayout::Mono).unwrap(),
        })
        .unwrap();
    let live = session.preview();
    let binding = binding("Tone");
    let source = add(&mut session, live, "Tone audio", &binding);
    let mut program = engine(&session);
    program.route(&session, &view(live), MediaTime(0)).unwrap();
    let samples: Vec<_> = (0..16000)
        .map(|index| (0.5 * (std::f64::consts::TAU * 1000.0 * index as f64 / 16000.0).sin()) as f32)
        .collect();
    let mut produced = 0;
    let mut consumed = 0;
    for packet in samples.chunks(997) {
        let at = consumed as i128 * 1_000_000_000 / 16000;
        produced += push(&mut program, &binding, 1, at, mono(16000), packet).output_frames;
        consumed += packet.len();
    }
    let cursor = program.input_cursor(&binding).unwrap();
    produced += program
        .finish_input(
            program.identity().unwrap(),
            &binding,
            cursor.generation,
            cursor.sequence,
        )
        .unwrap()
        .output_frames;
    assert_eq!(produced, 44100);
    let mut output = Vec::new();
    let mut start = 0;
    while start < 44100 {
        let frames = (44100 - start).min(1024);
        let block = program.render(start as u64, frames).unwrap();
        assert_eq!(missing(&block, source), 0);
        output.extend(block.samples);
        start += frames;
    }
    assert_eq!(output.len(), 44100);
    let mut signal = 0.0;
    let mut error = 0.0;
    for (index, value) in output.iter().enumerate().take(43076).skip(1024) {
        let expected = 0.5 * (std::f64::consts::TAU * 1000.0 * index as f64 / 44100.0).sin();
        signal += expected * expected;
        error += (f64::from(*value) - expected).powi(2);
    }
    assert!(
        10.0 * (signal / error).log10() > 65.0,
        "independent passband SNR {} dB",
        10.0 * (signal / error).log10()
    );
    let tail = program.render(44100, 1).unwrap();
    assert_eq!(tail.samples, [0.0]);
    assert_eq!(
        missing(&tail, source),
        1,
        "sealed filter must not fabricate extra samples"
    );
}

#[test]
fn shared_source_bypasses_fade_while_unique_source_fades_and_old_deleted_metadata_remains_valid() {
    let mut session = Session::new();
    let a = session.preview();
    let b = scene(&mut session, "B scene");
    let shared_binding = binding("Shared");
    let unique_binding = binding("Unique");
    let shared = add(&mut session, a, "Shared audio", &shared_binding);
    let unique = add(&mut session, a, "Unique audio", &unique_binding);
    session
        .apply(Command::AddExistingSource {
            scene: b,
            source: shared,
        })
        .unwrap();
    let mut program = engine(&session);
    program
        .route(
            &session,
            &FrameComposition::Fade {
                from: FrameScene::Live(a),
                to: FrameScene::Live(b),
                elapsed_ns: 0,
                duration_ns: 1_000_000,
                epoch: 20,
            },
            MediaTime(0),
        )
        .unwrap();
    program
        .route(&session, &view(b), MediaTime(1_000_000))
        .unwrap();
    session.apply(Command::RemoveScene { scene: a }).unwrap();
    assert!(session.snapshot().source(unique).is_none());
    push(
        &mut program,
        &shared_binding,
        1,
        0,
        mono(48000),
        &[0.125; 64],
    );
    push(&mut program, &unique_binding, 1, 0, mono(48000), &[0.5; 64]);
    let block = program.render(0, 64).unwrap();
    for (index, frame) in block.samples.chunks_exact(2).enumerate() {
        let elapsed = index as u64 * 1_000_000_000 / 48000;
        let expected = 0.125 + 0.5 * (1.0 - (elapsed.min(1_000_000) as f32 / 1_000_000.0));
        assert_eq!(frame, [expected, expected], "frame {index}");
    }
    assert!(block.gaps.is_empty());
}

#[test]
fn fade_to_black_ends_in_real_silence_but_global_audio_remains_a_root() {
    let mut session = Session::new();
    let a = session.preview();
    let b = scene(&mut session, "B scene");
    let scene_binding = binding("Scene");
    let global_binding = binding("Global");
    add(&mut session, a, "Scene audio", &scene_binding);
    session
        .apply(Command::AddGlobalAudioCaptureSource {
            slot: AudioSlot::Mic1,
            name: "Global audio".into(),
            binding: global_binding.clone(),
        })
        .unwrap();
    let mut program = engine(&session);
    program
        .route(
            &session,
            &FrameComposition::SingleFade {
                scene: FrameScene::Live(a),
                logical_scene: b,
                incoming: false,
                elapsed_ns: 0,
                duration_ns: 1_000_000,
                epoch: 30,
            },
            MediaTime(0),
        )
        .unwrap();
    program
        .route(
            &session,
            &FrameComposition::Black {
                logical_scene: b,
                epoch: 30,
            },
            MediaTime(1_000_000),
        )
        .unwrap();
    push(&mut program, &scene_binding, 1, 0, mono(48000), &[0.5; 64]);
    push(
        &mut program,
        &global_binding,
        1,
        0,
        mono(48000),
        &[0.125; 64],
    );
    let block = program.render(0, 64).unwrap();
    assert_eq!(&block.samples[96..], &[0.125; 32]);
    assert!(block.gaps.is_empty());
}

#[test]
fn missing_frames_are_diagnostic_gaps_not_claimed_captured_silence_and_final_clip_counts_are_per_channel()
 {
    let mut session = Session::new();
    let live = session.preview();
    let binding = binding("Sparse");
    let source = add(&mut session, live, "Sparse audio", &binding);
    let mut program = engine(&session);
    program.route(&session, &view(live), MediaTime(0)).unwrap();
    push(
        &mut program,
        &binding,
        1,
        41_667,
        stereo(48000),
        &[2.0, -2.0, 0.0, 0.0],
    );
    let block = program.render(0, 6).unwrap();
    assert_eq!(
        block.samples,
        [0.0, 0.0, 0.0, 0.0, 1.0, -1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]
    );
    assert_eq!(missing(&block, source), 4);
    assert_eq!(block.clipped_samples, [1, 1]);
    assert_eq!(block.source_meters[0].channels[0].peak, 2.0);
    assert_eq!(block.program_meters[0].rms, (1.0f64 / 6.0).sqrt());
}

#[test]
fn strict_70ms_smoothing_boundary_preserves_small_jitter_and_places_medium_jump_as_gap() {
    for (jitter, smoothed, gap) in [(69_999_999, true, 0), (70_000_000, false, 3360)] {
        let mut session = Session::new();
        let live = session.preview();
        let binding = binding("Clock");
        let source = add(&mut session, live, "Clock audio", &binding);
        let mut program = engine(&session);
        program.route(&session, &view(live), MediaTime(0)).unwrap();
        push(&mut program, &binding, 1, 0, mono(48000), &[0.25; 480]);
        let report = push(
            &mut program,
            &binding,
            1,
            10_000_000 + jitter,
            mono(48000),
            &[0.5; 2],
        );
        assert_eq!(report.timestamp_smoothed, smoothed);
        assert_eq!(report.gap_frames, gap);
        let first = program.render(0, 480).unwrap();
        assert!(first.samples.iter().all(|v| *v == 0.25));
        if smoothed {
            let next = program.render(480, 2).unwrap();
            assert_eq!(next.samples, [0.5; 4]);
            assert_eq!(missing(&next, source), 0);
        } else {
            let next = program.render(480, 1024).unwrap();
            assert_eq!(missing(&next, source), 1024);
            assert!(next.samples.iter().all(|v| *v == 0.0));
        }
    }
}

#[test]
fn medium_overlap_overwrites_and_truncates_old_tail_and_generation_change_starts_fresh_continuity()
{
    let mut session = Session::new();
    let live = session.preview();
    let binding = binding("Overlap");
    let source = add(&mut session, live, "Overlap audio", &binding);
    let mut program = engine(&session);
    program.route(&session, &view(live), MediaTime(0)).unwrap();
    push(&mut program, &binding, 1, 0, mono(48000), &[0.25; 4096]);
    let report = push(&mut program, &binding, 1, 0, mono(48000), &[0.75; 2]);
    assert!(!report.timestamp_smoothed);
    assert_eq!(report.overlap_frames, 2);
    let block = program.render(0, 4).unwrap();
    assert_eq!(block.samples, [0.75, 0.75, 0.75, 0.75, 0.0, 0.0, 0.0, 0.0]);
    assert_eq!(missing(&block, source), 2);
    let report = push(&mut program, &binding, 2, 83_333, mono(48000), &[0.5; 2]);
    assert!(report.discontinuity);
    let next = program.render(4, 2).unwrap();
    assert_eq!(next.samples, [0.5; 4]);
    assert_eq!(missing(&next, source), 0);
}

#[test]
fn indirect_two_second_clock_jump_reanchors_and_clears_stale_timeline() {
    let mut session = Session::new();
    let live = session.preview();
    let binding = binding("Indirect");
    let source = add(&mut session, live, "Indirect audio", &binding);
    let mut program = engine(&session);
    program.route(&session, &view(live), MediaTime(0)).unwrap();
    program
        .ingest(ProgramInputPacket {
            identity: program.identity().unwrap(),
            binding: &binding,
            stream_generation: 1,
            sequence: program.status().data_generation + 1,
            first_media_ns: 10_000_000_000,
            received_media_ns: 0,
            format: mono(48000),
            samples: &[0.25; 480],
            flags: Default::default(),
        })
        .unwrap();
    let report = program
        .ingest(ProgramInputPacket {
            identity: program.identity().unwrap(),
            binding: &binding,
            stream_generation: 1,
            sequence: program.status().data_generation + 1,
            first_media_ns: 20_000_000_000,
            received_media_ns: 5_000_000,
            format: mono(48000),
            samples: &[0.75; 2],
            flags: Default::default(),
        })
        .unwrap();
    assert!(report.timing_reanchored);
    let block = program.render(0, 242).unwrap();
    assert!(block.samples[..480].iter().all(|v| *v == 0.0));
    assert_eq!(&block.samples[480..], &[0.75; 4]);
    assert_eq!(missing(&block, source), 240);
}

#[test]
fn malformed_native_packets_reject_before_mutating_public_state() {
    let mut session = Session::new();
    let live = session.preview();
    let binding = binding("Validated");
    add(&mut session, live, "Validated audio", &binding);
    let mut program = engine(&session);
    program.route(&session, &view(live), MediaTime(0)).unwrap();
    push(&mut program, &binding, 1, 0, mono(48000), &[0.25; 4]);
    let before = status(&program);
    let huge = vec![0.0; 4097];
    for (generation, format, samples, flags) in [
        (0, mono(48000), &[0.0][..], NativePacketFlags::default()),
        (
            1,
            mono(48000),
            &[f32::NAN][..],
            NativePacketFlags::default(),
        ),
        (
            1,
            mono(48000),
            &[f32::INFINITY][..],
            NativePacketFlags::default(),
        ),
        (1, stereo(48000), &[0.0][..], NativePacketFlags::default()),
        (1, mono(48000), &huge[..], NativePacketFlags::default()),
        (
            1,
            mono(48000),
            &[0.25][..],
            NativePacketFlags {
                silent: true,
                ..Default::default()
            },
        ),
    ] {
        assert!(
            program
                .ingest(ProgramInputPacket {
                    identity: program.identity().unwrap(),
                    binding: &binding,
                    stream_generation: generation,
                    sequence: program.status().data_generation + 1,
                    first_media_ns: 0,
                    received_media_ns: 0,
                    format,
                    samples,
                    flags
                })
                .is_err()
        );
        assert_eq!(status(&program), before);
    }
    let block = program.render(0, 4).unwrap();
    assert_eq!(block.samples, [0.25; 8]);
}

#[test]
fn native_discontinuity_and_queue_loss_reset_input_epoch_without_destroying_prior_samples() {
    let mut session = Session::new();
    let live = session.preview();
    let binding = binding("Flags");
    add(&mut session, live, "Flags audio", &binding);
    let mut program = engine(&session);
    program.route(&session, &view(live), MediaTime(0)).unwrap();
    push(&mut program, &binding, 1, 0, mono(48000), &[0.25; 4]);
    let report = program
        .ingest(ProgramInputPacket {
            identity: program.identity().unwrap(),
            binding: &binding,
            stream_generation: 1,
            sequence: program.status().data_generation + 1,
            first_media_ns: 83_333,
            received_media_ns: 83_333,
            format: mono(48000),
            samples: &[0.5; 4],
            flags: NativePacketFlags {
                discontinuity: true,
                queue_dropped: 2,
                timestamp_error: true,
                ..Default::default()
            },
        })
        .unwrap();
    assert!(report.discontinuity);
    let block = program.render(0, 8).unwrap();
    assert_eq!(&block.samples[..8], &[0.25; 8]);
    assert_eq!(&block.samples[8..], &[0.5; 8]);
    assert!(block.gaps.is_empty());
}

#[test]
fn cursor_format_changes_and_explicit_reset_have_distinct_checked_lifecycles() {
    let mut session = Session::new();
    let live = session.preview();
    let mut program = engine(&session);
    program.route(&session, &view(live), MediaTime(0)).unwrap();
    program.render(0, 4).unwrap();
    let before = status(&program);
    assert_eq!(
        program.render(0, 1).unwrap_err(),
        ProgramAudioError::ClockRewind
    );
    assert_eq!(
        program.render(5, 1).unwrap_err(),
        ProgramAudioError::ClockRewind
    );
    assert_eq!(
        program.route(&session, &view(live), MediaTime(0)),
        Err(ProgramAudioError::ClockRewind)
    );
    assert_eq!(status(&program), before);
    session
        .apply(Command::SetAudioSettings {
            settings: AudioSettings::new(44100, SpeakerLayout::Mono).unwrap(),
        })
        .unwrap();
    assert_eq!(
        program.route(&session, &view(live), MediaTime(1_000_000)),
        Err(ProgramAudioError::FormatChanged)
    );
    program.reset().unwrap();
    let after = program.status();
    assert_eq!(after.epoch, before["epoch"].as_u64().unwrap() + 1);
    assert_eq!(after.cursor, None);
    assert_eq!(after.session, None);
    assert_eq!(after.input_groups, 0);
    assert!(program.render(0, 1).is_err());
    let mut replacement = engine(&session);
    replacement
        .route(&session, &view(live), MediaTime(0))
        .unwrap();
    assert_eq!(
        replacement.render(0, 1).unwrap().settings.sample_rate(),
        44100
    );
}

#[test]
fn async_pcm_identity_rejects_wrong_owner_session_and_pre_reset_epoch_before_dsp() {
    let mut session = Session::new();
    let live = session.preview();
    let binding = binding("Identity");
    add(&mut session, live, "Identity audio", &binding);
    let mut program = engine(&session);
    assert!(program.identity().is_err());
    program.route(&session, &view(live), MediaTime(0)).unwrap();
    let original = program.identity().unwrap();
    push(&mut program, &binding, 1, 0, mono(48000), &[0.25; 2]);
    let foreign_session = Session::new();
    let mut foreign_program = engine(&session);
    foreign_program
        .route(&session, &view(live), MediaTime(0))
        .unwrap();
    let wrong_owner = foreign_program.identity().unwrap();
    let wrong_session = ProgramInputIdentity {
        session: foreign_session.identity(),
        ..original
    };
    for identity in [wrong_owner, wrong_session] {
        let before = status(&program);
        assert!(
            program
                .ingest(ProgramInputPacket {
                    identity,
                    binding: &binding,
                    stream_generation: 1,
                    sequence: program.status().data_generation + 1,
                    first_media_ns: 41_667,
                    received_media_ns: 41_667,
                    format: mono(48000),
                    samples: &[0.5],
                    flags: Default::default()
                })
                .is_err()
        );
        assert_eq!(status(&program), before);
    }
    program.reset().unwrap();
    program.route(&session, &view(live), MediaTime(0)).unwrap();
    let before = status(&program);
    assert_ne!(program.identity().unwrap().epoch, original.epoch);
    assert!(
        program
            .ingest(ProgramInputPacket {
                identity: original,
                binding: &binding,
                stream_generation: 1,
                sequence: program.status().data_generation + 1,
                first_media_ns: 0,
                received_media_ns: 0,
                format: mono(48000),
                samples: &[0.75],
                flags: Default::default()
            })
            .is_err()
    );
    assert_eq!(status(&program), before);
    push(&mut program, &binding, 1, 0, mono(48000), &[0.5]);
    assert_eq!(program.render(0, 1).unwrap().samples, [0.5; 2]);
}

#[test]
fn native_generation_sequence_and_eof_fences_reject_late_work_without_replacing_current_pcm() {
    let mut session = Session::new();
    let live = session.preview();
    let binding = binding("Ordering");
    add(&mut session, live, "Ordering audio", &binding);
    let mut program = engine(&session);
    program.route(&session, &view(live), MediaTime(0)).unwrap();
    let identity = program.identity().unwrap();
    let admit = |program: &mut AudioProgram, generation, sequence, time, samples: &[f32]| {
        program.ingest(ProgramInputPacket {
            identity,
            binding: &binding,
            stream_generation: generation,
            sequence,
            first_media_ns: time,
            received_media_ns: time,
            format: mono(48000),
            samples,
            flags: Default::default(),
        })
    };
    admit(&mut program, 1, 10, 0, &[0.25; 2]).unwrap();
    let old_cursor = program.input_cursor(&binding).unwrap();
    // A newly opened generation may restart its sequence at one.
    admit(&mut program, 2, 1, 41_667, &[0.5; 2]).unwrap();
    let current = program.input_cursor(&binding).unwrap();
    assert_eq!((current.generation, current.sequence), (2, 1));
    let before = status(&program);
    for (generation, sequence) in [(1, 11), (2, 1), (2, 0)] {
        assert!(admit(&mut program, generation, sequence, 83_333, &[0.75]).is_err());
        assert_eq!(status(&program), before);
        let after = program.input_cursor(&binding).unwrap();
        assert_eq!((after.generation, after.sequence), (2, 1));
    }
    assert!(
        program
            .finish_input(
                identity,
                &binding,
                old_cursor.generation,
                old_cursor.sequence
            )
            .is_err()
    );
    assert_eq!(status(&program), before);
    assert!(program.finish_input(identity, &binding, 2, 0).is_err());
    assert_eq!(status(&program), before);
    admit(&mut program, 2, 3, 83_333, &[0.75; 2]).unwrap();
    let before = status(&program);
    assert!(admit(&mut program, 2, 2, 125_000, &[1.0]).is_err());
    assert_eq!(status(&program), before);
    program.finish_input(identity, &binding, 2, 3).unwrap();
    let sealed = status(&program);
    assert!(admit(&mut program, 2, 4, 125_000, &[1.0]).is_err());
    assert_eq!(status(&program), sealed);
    assert!(
        program
            .finish_input(
                ProgramInputIdentity {
                    epoch: identity.epoch + 1,
                    ..identity
                },
                &binding,
                2,
                3
            )
            .is_err()
    );
    assert_eq!(status(&program), sealed);
    let block = program.render(0, 6).unwrap();
    assert_eq!(
        block.samples,
        [
            0.25, 0.25, 0.25, 0.25, 0.5, 0.5, 0.5, 0.5, 0.75, 0.75, 0.75, 0.75
        ]
    );
    assert!(block.gaps.is_empty());
}

#[test]
fn group_memory_ring_and_block_limits_are_explicit_and_do_not_grow_unbounded() {
    let mut session = Session::new();
    let live = session.preview();
    let bindings = [binding("A"), binding("B"), binding("C")];
    for (index, binding) in bindings.iter().enumerate() {
        add(&mut session, live, &format!("Audio {index}"), binding);
    }
    let mut program = AudioProgram::new(
        session.snapshot().audio(),
        ProgramAudioLimits {
            max_groups: 2,
            ..Default::default()
        },
    )
    .unwrap();
    program.route(&session, &view(live), MediaTime(0)).unwrap();
    push(&mut program, &bindings[0], 1, 0, mono(48000), &[0.25]);
    push(&mut program, &bindings[1], 1, 0, mono(48000), &[0.25]);
    let before = status(&program);
    assert!(matches!(
        program.ingest(ProgramInputPacket {
            identity: program.identity().unwrap(),
            binding: &bindings[2],
            stream_generation: 1,
            sequence: program.status().data_generation + 1,
            first_media_ns: 0,
            received_media_ns: 0,
            format: mono(48000),
            samples: &[0.25],
            flags: Default::default()
        }),
        Err(ProgramAudioError::Limit(_))
    ));
    assert_eq!(status(&program), before);
    assert!(program.render(0, 1025).is_err());
    assert!(program.status().accounted_bytes <= 128 * 1024 * 1024);
    assert!(
        AudioProgram::new(
            session.snapshot().audio(),
            ProgramAudioLimits {
                max_bytes: 1024,
                ..Default::default()
            }
        )
        .is_err()
    );
    let mut short = AudioProgram::new(
        session.snapshot().audio(),
        ProgramAudioLimits {
            max_buffer_ms: 40,
            ..Default::default()
        },
    )
    .unwrap();
    short.route(&session, &view(live), MediaTime(0)).unwrap();
    let before = status(&short);
    assert!(
        short
            .ingest(ProgramInputPacket {
                identity: short.identity().unwrap(),
                binding: &bindings[0],
                stream_generation: 1,
                sequence: short.status().data_generation + 1,
                first_media_ns: 0,
                received_media_ns: 0,
                format: mono(48000),
                samples: &[0.25],
                flags: Default::default()
            })
            .is_err()
    );
    assert_eq!(status(&short), before);
    let mut full = AudioProgram::new(
        session.snapshot().audio(),
        ProgramAudioLimits {
            max_buffer_ms: 50,
            ..Default::default()
        },
    )
    .unwrap();
    full.route(&session, &view(live), MediaTime(0)).unwrap();
    assert!(matches!(
        full.ingest(ProgramInputPacket {
            identity: full.identity().unwrap(),
            binding: &bindings[0],
            stream_generation: 1,
            sequence: full.status().data_generation + 1,
            first_media_ns: 0,
            received_media_ns: 0,
            format: mono(48000),
            samples: &[0.25; 4096],
            flags: Default::default()
        }),
        Err(ProgramAudioError::Backpressure(_))
    ));
    assert!(full.status().accounted_bytes <= 128 * 1024 * 1024);
}

#[test]
fn binding_admission_preserves_retiring_groups_but_fences_do_not_authorize_unrouted_recreation() {
    let mut session = Session::new();
    let audible = session.preview();
    let silent = scene(&mut session, "Silent route");
    let routed = binding("Retiring input");
    let unrelated = binding("Never routed");
    add(&mut session, audible, "Routed audio", &routed);
    let mut program = engine(&session);
    program
        .route(&session, &view(audible), MediaTime(0))
        .unwrap();
    let before = status(&program);
    assert!(
        program
            .ingest(ProgramInputPacket {
                identity: program.identity().unwrap(),
                binding: &unrelated,
                stream_generation: 1,
                sequence: 1,
                first_media_ns: 0,
                received_media_ns: 0,
                format: mono(48000),
                samples: &[0.75; 4],
                flags: Default::default(),
            })
            .is_err()
    );
    assert_eq!(status(&program), before);
    assert_eq!(program.input_cursor(&unrelated), None);

    push(&mut program, &routed, 1, 0, mono(48000), &[0.25; 8]);
    // Ceil event quantization places this cut at project sample4.
    program
        .route(&session, &view(silent), MediaTime(83_333))
        .unwrap();
    assert_eq!(program.render(0, 4).unwrap().samples, [0.25; 8]);
    assert!(program.timeline.required_bindings().is_empty());
    assert_eq!(
        program.status().input_groups,
        1,
        "future queued PCM retains the retiring group"
    );
    let retired_report = push(&mut program, &routed, 1, 166_666, mono(48000), &[0.5; 4]);
    assert_eq!(retired_report.accepted_frames, 4);
    let cursor = program.input_cursor(&routed).unwrap();
    let silent_block = program.render(4, 8).unwrap();
    assert_eq!(silent_block.samples, [0.0; 16]);
    assert!(silent_block.gaps.is_empty());
    assert_eq!(program.status().input_groups, 0);
    assert_eq!(
        program.status().known_input_streams,
        1,
        "ordering fence outlives drained storage"
    );
    let before = status(&program);
    assert!(
        program
            .ingest(ProgramInputPacket {
                identity: program.identity().unwrap(),
                binding: &routed,
                stream_generation: cursor.generation,
                sequence: cursor.sequence + 1,
                first_media_ns: 250_000,
                received_media_ns: 250_000,
                format: mono(48000),
                samples: &[0.75; 4],
                flags: Default::default(),
            })
            .is_err()
    );
    assert_eq!(status(&program), before);
    assert_eq!(program.input_cursor(&routed), Some(cursor));
}

#[test]
fn coalesced_meter_packet_is_not_admitted_or_hidden_by_small_jitter_smoothing() {
    let mut session = Session::new();
    let scene = session.preview();
    let input = binding("Lossy meter view");
    add(&mut session, scene, "Audio", &input);
    let mut program = engine(&session);
    program.route(&session, &view(scene), MediaTime(0)).unwrap();
    push(&mut program, &input, 1, 0, mono(48000), &[0.25; 4]);
    let cursor = program.input_cursor(&input).unwrap();
    let mut packet = CapturedAudioPacket {
        resolved_device: None,
        binding: input.clone(),
        sequence: cursor.sequence + 1,
        requested_at_ns: 0,
        stream_generation: cursor.generation,
        first_sample_media_ns: 30_083_333,
        timing_origin: AudioTimingOrigin::ReceiveEstimate,
        coalesced_packets: 1,
        meters: Vec::new(),
        block: crate::audio_capture::NativePcmBlock {
            format: mono(48000),
            frames: 2,
            samples: vec![0.75; 2],
            device_timestamp_ns: None,
            graph_timestamp_ns: None,
            directshow_clock: None,
            host_received_ns: 0,
            device_frame_position: None,
            flags: Default::default(),
        },
    };
    let before = status(&program);
    assert!(
        program
            .ingest_captured(program.identity().unwrap(), &packet)
            .is_err()
    );
    assert_eq!(status(&program), before);
    assert_eq!(program.input_cursor(&input), Some(cursor));
    packet.coalesced_packets = 0;
    packet.block.frames += 1;
    assert!(
        program
            .ingest_captured(program.identity().unwrap(), &packet)
            .is_err()
    );
    assert_eq!(status(&program), before);
    assert_eq!(program.input_cursor(&input), Some(cursor));
    assert_eq!(program.render(0, 4).unwrap().samples, [0.25; 8]);
}

#[test]
fn program_accepts_owned_historical_routes_and_checks_full_project_settings() {
    let mut session = Session::new();
    let scene = session.preview();
    session
        .apply(Command::SetAudioSettings {
            settings: AudioSettings::new(48000, SpeakerLayout::Mono).unwrap(),
        })
        .unwrap();
    let input = binding("Owned route PCM");
    let source = add(&mut session, scene, "Audio", &input);
    let first = AudioRouteSnapshot::capture(&session, &view(scene), MediaTime(0)).unwrap();
    let mut program = AudioProgram::new(first.settings(), ProgramAudioLimits::default()).unwrap();
    session
        .apply(Command::SetAudio {
            source,
            gain: 0.5,
            muted: false,
        })
        .unwrap();
    let changed = AudioRouteSnapshot::capture(&session, &view(scene), MediaTime(83_333)).unwrap();
    session
        .apply(Command::SetAudioSettings {
            settings: AudioSettings::new(48000, SpeakerLayout::Stereo).unwrap(),
        })
        .unwrap();
    let wrong_layout =
        AudioRouteSnapshot::capture(&session, &view(scene), MediaTime(166_666)).unwrap();
    drop(session);
    program.route_snapshot(first).unwrap();
    program.route_snapshot(changed).unwrap();
    assert_eq!(program.required_bindings().len(), 1);
    let before = status(&program);
    assert_eq!(
        program.route_snapshot(wrong_layout),
        Err(ProgramAudioError::FormatChanged)
    );
    assert_eq!(status(&program), before);
    push(&mut program, &input, 1, 0, mono(48000), &[0.5; 8]);
    let block = program.render(0, 8).unwrap();
    assert_eq!(block.samples, [0.5, 0.5, 0.5, 0.5, 0.25, 0.25, 0.25, 0.25]);
}

#[test]
fn late_converted_prefix_is_dropped_before_ring_placement_without_rewinding_output_time() {
    for (at, frames, expected_late) in [(0, 1024, 1024), (30_000_000, 4096, 3360)] {
        let mut session = Session::new();
        let scene = session.preview();
        session
            .apply(Command::SetAudioSettings {
                settings: AudioSettings::new(48000, SpeakerLayout::Mono).unwrap(),
            })
            .unwrap();
        let input = binding("Late input");
        add(&mut session, scene, "Audio", &input);
        let mut program = engine(&session);
        program.route(&session, &view(scene), MediaTime(0)).unwrap();
        push(&mut program, &input, 1, 0, mono(48000), &[0.25; 4096]);
        push(
            &mut program,
            &input,
            1,
            85_333_333,
            mono(48000),
            &[0.25; 4096],
        );
        let mut start = 0;
        while start < 4800 {
            let count = (4800 - start).min(1024);
            assert!(
                program
                    .render(start as u64, count)
                    .unwrap()
                    .samples
                    .iter()
                    .all(|value| *value == 0.25)
            );
            start += count;
        }
        let report = push(
            &mut program,
            &input,
            1,
            at,
            mono(48000),
            &vec![0.75; frames],
        );
        assert!(report.discontinuity);
        assert_eq!(report.late_frames, expected_late);
        assert_eq!(report.output_frames, frames);
        let group = program
            .groups
            .get(&AudioInputKey::Native(input.clone()))
            .unwrap();
        assert_eq!(group.output_frames, frames as u64);
        assert_eq!(group.converter.stats().consumed_input_frames, frames as u64);
        if expected_late == frames {
            assert_eq!(
                group.ring.end(),
                8192,
                "a wholly late packet must not clear already queued future PCM"
            );
            let next = program.render(4800, 1024).unwrap();
            assert_eq!(next.samples, [0.25; 1024]);
            assert!(next.gaps.is_empty());
        } else {
            let remaining = frames - expected_late;
            let next = program.render(4800, remaining).unwrap();
            assert_eq!(next.samples, vec![0.75; remaining]);
            assert!(next.gaps.is_empty());
            assert_eq!(
                program
                    .groups
                    .get(&AudioInputKey::Native(input.clone()))
                    .unwrap()
                    .output_anchor
                    + i128::from(frames as u64),
                5536
            );
        }
    }
}

#[test]
fn next_frame_route_uses_the_same_ceil_grid_as_retirement_at_44100_hz() {
    let mut session = Session::new();
    session
        .apply(Command::SetAudioSettings {
            settings: AudioSettings::new(44100, SpeakerLayout::Mono).unwrap(),
        })
        .unwrap();
    let live = session.preview();
    let input = binding("Rational route boundary");
    let source = add(&mut session, live, "Audio", &input);
    let mut program = engine(&session);
    program.route(&session, &view(live), MediaTime(0)).unwrap();
    push(&mut program, &input, 1, 0, mono(44100), &[0.25; 1025]);
    assert_eq!(program.render(0, 1024).unwrap().samples, [0.25; 1024]);
    session
        .apply(Command::SetAudio {
            source,
            gain: 0.5,
            muted: false,
        })
        .unwrap();
    let next_frame_ns = 1024 * 1_000_000_000u64 / 44100;
    assert_eq!(next_frame_ns, 23_219_954);
    assert!(
        program
            .route(&session, &view(live), MediaTime(next_frame_ns))
            .unwrap()
    );
    let before = status(&program);
    assert_eq!(
        program.route(
            &session,
            &view(live),
            MediaTime(1023 * 1_000_000_000 / 44100)
        ),
        Err(ProgramAudioError::ClockRewind)
    );
    assert_eq!(status(&program), before);
    assert_eq!(program.render(1024, 1).unwrap().samples, [0.125]);
}

#[test]
fn explicit_forward_skip_retires_an_hour_without_rendering_catchup_blocks() {
    let mut session = Session::new();
    let old_scene = session.preview();
    session
        .apply(Command::SetAudioSettings {
            settings: AudioSettings::new(48000, SpeakerLayout::Mono).unwrap(),
        })
        .unwrap();
    let new_scene = scene(&mut session, "After suspension");
    let old_input = binding("Before skip");
    let current_input = binding("After skip");
    add(&mut session, old_scene, "Old audio", &old_input);
    add(&mut session, new_scene, "Current audio", &current_input);
    let mut program = engine(&session);
    program
        .route(&session, &view(old_scene), MediaTime(0))
        .unwrap();
    push(&mut program, &old_input, 1, 0, mono(48000), &[0.25; 4096]);
    program
        .route(&session, &view(new_scene), MediaTime(1_000_000_000))
        .unwrap();
    assert_eq!(program.render(0, 1024).unwrap().samples, [0.25; 1024]);
    let before = program.status();
    let identity = program.identity().unwrap();
    let frame = 3600 * 48000u64;
    program.skip_to(frame).unwrap();
    let skipped = program.status();
    assert_eq!(skipped.cursor, Some(frame));
    assert_eq!(skipped.data_generation, before.data_generation + 1);
    assert_eq!(program.identity().unwrap(), identity);
    assert_eq!(skipped.routing_events, 1);
    assert_eq!(skipped.input_groups, 0);
    assert_eq!(program.required_bindings().len(), 1);
    assert_eq!(program.required_bindings()[0].binding, current_input);
    assert!(skipped.accounted_bytes <= before.accounted_bytes);
    let frozen = status(&program);
    assert_eq!(
        program.skip_to(frame - 1),
        Err(ProgramAudioError::ClockRewind)
    );
    assert_eq!(status(&program), frozen);
    assert!(program.skip_to(u64::MAX).is_err());
    assert_eq!(status(&program), frozen);
    program.skip_to(frame).unwrap();
    assert_eq!(status(&program), frozen);
    assert!(
        program
            .ingest(ProgramInputPacket {
                identity,
                binding: &old_input,
                stream_generation: 1,
                sequence: u64::MAX,
                first_media_ns: 0,
                received_media_ns: 0,
                format: mono(48000),
                samples: &[1.0],
                flags: Default::default(),
            })
            .is_err()
    );
    assert_eq!(status(&program), frozen);
    push(
        &mut program,
        &current_input,
        1,
        3_600_000_000_000,
        mono(48000),
        &[0.75; 2],
    );
    let resumed = program.render(frame, 2).unwrap();
    assert_eq!(resumed.samples, [0.75; 2]);
    assert!(resumed.gaps.is_empty());
    push(
        &mut program,
        &current_input,
        1,
        3_600_000_041_666,
        mono(48000),
        &[0.5],
    );
    let contiguous = program.render(frame + 2, 1).unwrap();
    assert_eq!(contiguous.samples, [0.5]);
    assert!(contiguous.gaps.is_empty());
}

#[test]
fn forward_skip_requires_route_coverage_before_publishing_any_cursor_or_generation() {
    let session = Session::new();
    let mut program = engine(&session);
    let before = status(&program);
    assert!(program.skip_to(1).is_err());
    assert_eq!(status(&program), before);
    program
        .route(&session, &view(session.preview()), MediaTime(1_000_000_000))
        .unwrap();
    let before = status(&program);
    assert!(program.skip_to(0).is_err());
    assert_eq!(status(&program), before);
    program.skip_to(48000).unwrap();
    assert_eq!(program.status().cursor, Some(48000));
    assert_eq!(program.render(48000, 1).unwrap().samples, [0.0; 2]);
}

#[test]
fn meter_configuration_revision_is_absent_for_mixed_routes_and_exact_for_single_revision() {
    let mut session = Session::new();
    let scene = session.preview();
    let binding = binding("Meter provenance");
    let source = add(&mut session, scene, "Meter provenance", &binding);
    let mut program = engine(&session);
    program.route(&session, &view(scene), MediaTime(0)).unwrap();
    session
        .apply(Command::SetAudio {
            source,
            gain: 0.5,
            muted: false,
        })
        .unwrap();
    let revision = session.snapshot().revision();
    program
        .route(&session, &view(scene), MediaTime(1_000_000))
        .unwrap();
    push(&mut program, &binding, 1, 0, stereo(48000), &[0.5; 136]);
    let mixed = program.render(0, 64).unwrap();
    assert_eq!(mixed.source_meter_revision, None);
    assert_eq!(&mixed.samples[..96], &[0.5; 96]);
    assert_eq!(&mixed.samples[96..], &[0.25; 32]);
    let current = program.render(64, 4).unwrap();
    assert_eq!(current.source_meter_revision, Some(revision));
    assert_eq!(current.samples, [0.25; 8]);
}

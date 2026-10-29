use super::*;
use crate::{
    audio_capture::NativeAudioFormat,
    audio_program::{AudioProgram, ProgramInputPacket},
    storage,
    transition::FrameScene,
};
use skylive_core::{
    AudioCaptureBinding, AudioFilter, AudioFlow, AudioTarget, Command, SourceAudioProperties,
};
use skylive_media::{AudioFormat, MediaTime};
fn setup() -> (Session, Id, AudioCaptureBinding) {
    let mut session = Session::new();
    let binding =
        AudioCaptureBinding::new(AudioFlow::Input, AudioTarget::Default, false, false).unwrap();
    session
        .apply(Command::AddAudioCaptureSource {
            scene: session.preview(),
            name: "Voice".into(),
            binding: binding.clone(),
        })
        .unwrap();
    let source = session.snapshot().sources()[0].id();
    (session, source, binding)
}
fn output(session: &Session) -> FrameComposition {
    FrameComposition::Scene(FrameScene::Live(session.preview()))
}
fn request(frames: usize, channels: u16) -> AudioMixRequest {
    AudioMixRequest {
        sample_rate: 48000,
        channels,
        frames,
        advance_ns: 0,
    }
}
#[test]
fn channel_property_edit_keeps_existing_compressor_history() {
    let (mut session, source, _binding) = setup();
    let filter = AudioFilter::compressor();
    session
        .apply(Command::SetSourceAudioFilters {
            source,
            filters: vec![filter],
        })
        .unwrap();
    let plan = derive_audio_plan(&session, &output(&session)).unwrap();
    let input = [0.8, 0.0].repeat(1024);
    let mut state = AudioDspState::default();
    let first = plan
        .mix_on_grid_with_dsp(
            request(1024, 2),
            &[PcmInput::Samples {
                source,
                sample_rate: 48000,
                channels: 2,
                samples: &input,
            }],
            0,
            &mut state,
        )
        .unwrap();
    let mut oracle = skylive_media::craft_audio::Compressor::new(
        48000,
        2,
        compressor_parameters(filter).unwrap(),
    )
    .unwrap();
    let mut expected_first = input.clone();
    oracle.process_interleaved(&mut expected_first).unwrap();
    assert_eq!(first.samples, expected_first);
    session
        .apply(Command::SetSourceAudioProperties {
            source,
            properties: SourceAudioProperties::new(true, 0.5).unwrap(),
        })
        .unwrap();
    let next = derive_audio_plan(&session, &output(&session)).unwrap();
    let actual = next
        .mix_on_grid_with_dsp(
            request(1024, 2),
            &[PcmInput::Samples {
                source,
                sample_rate: 48000,
                channels: 2,
                samples: &input,
            }],
            1024,
            &mut state,
        )
        .unwrap();
    let mut expected = vec![0.4; 2048];
    oracle.process_interleaved(&mut expected).unwrap();
    assert_eq!(actual.samples, expected);
    let mut fresh = skylive_media::craft_audio::Compressor::new(
        48000,
        2,
        compressor_parameters(filter).unwrap(),
    )
    .unwrap();
    let mut wrong = vec![0.4; 2048];
    fresh.process_interleaved(&mut wrong).unwrap();
    assert!((actual.samples[0] - wrong[0]).abs() > 0.01);
}
#[test]
fn original_layout_survives_conversion_and_unknown_is_not_assumed_stereo() {
    let (mut session, source, _binding) = setup();
    session
        .apply(Command::SetSourceAudioProperties {
            source,
            properties: SourceAudioProperties::new(false, 0.0).unwrap(),
        })
        .unwrap();
    let plan = derive_audio_plan(&session, &output(&session)).unwrap();
    for (original, expected) in [
        (None, [0.75, 0.25]),
        (Some(AudioFormat::new(48000, 1, 4).unwrap()), [0.75, 0.25]),
        (Some(AudioFormat::new(48000, 2, 3).unwrap()), [0.75, 0.0]),
        (Some(AudioFormat::new(48000, 2, 12).unwrap()), [0.75, 0.25]),
    ] {
        let mixed = plan
            .mix(
                request(1, 2),
                &[PcmInput::Converted {
                    source,
                    sample_rate: 48000,
                    channels: 2,
                    samples: &[0.75, 0.25],
                    original,
                }],
            )
            .unwrap();
        assert_eq!(mixed.samples, expected);
    }
    let mono = plan
        .mix(
            request(1, 1),
            &[PcmInput::Converted {
                source,
                sample_rate: 48000,
                channels: 1,
                samples: &[0.75],
                original: Some(AudioFormat::new(48000, 2, 3).unwrap()),
            }],
        )
        .unwrap();
    assert_eq!(mono.samples, [0.75]);
}
#[test]
fn channel_controls_precede_filter_gain_and_shared_source_route_dedup() {
    let (mut session, source, _binding) = setup();
    let scene = session.preview();
    session
        .apply(Command::SetPluginEnabled {
            plugin: skylive_core::AUDIO_PAN_PLUGIN.into(),
            enabled: true,
        })
        .unwrap();
    session
        .apply(Command::SetSourceAudioProperties {
            source,
            properties: SourceAudioProperties::new(true, 0.5).unwrap(),
        })
        .unwrap();
    session
        .apply(Command::SetSourceAudioFilters {
            source,
            filters: vec![AudioFilter::Pan {
                pan_left: -100.0,
                pan_right: 100.0,
                gain_left_db: 0.0,
                gain_right_db: 0.0,
                invert_left: true,
                invert_right: false,
            }],
        })
        .unwrap();
    session
        .apply(Command::SetAudio {
            source,
            gain: 0.5,
            muted: false,
        })
        .unwrap();
    session
        .apply(Command::AddExistingSource { scene, source })
        .unwrap();
    session
        .apply(Command::SetGlobalAudio {
            slot: skylive_core::AudioSlot::Mic1,
            source: Some(source),
        })
        .unwrap();
    let plan = derive_audio_plan(&session, &output(&session)).unwrap();
    assert_eq!(plan.sources().len(), 1);
    let mixed = plan
        .mix(
            request(1, 2),
            &[PcmInput::Samples {
                source,
                sample_rate: 48000,
                channels: 2,
                samples: &[1.0, 0.0],
            }],
        )
        .unwrap();
    assert_eq!(mixed.samples, [-0.25, 0.25]);
    assert_eq!(mixed.source_meters[0].channels[0].peak, 0.25);
    assert_eq!(mixed.clipped_samples, [0, 0]);
}
#[test]
fn program_replays_channel_property_boundary_per_sample_and_partition_independently() {
    let (mut session, source, binding) = setup();
    let mut one = AudioProgram::new(session.snapshot().audio(), Default::default()).unwrap();
    let mut split = AudioProgram::new(session.snapshot().audio(), Default::default()).unwrap();
    let samples = [1.0, 0.0].repeat(8);
    for engine in [&mut one, &mut split] {
        engine
            .route(&session, &output(&session), MediaTime(0))
            .unwrap();
        let identity = engine.identity().unwrap();
        engine
            .ingest(ProgramInputPacket {
                identity,
                binding: &binding,
                stream_generation: 1,
                sequence: 1,
                first_media_ns: 0,
                received_media_ns: 0,
                format: NativeAudioFormat {
                    sample_rate: 48000,
                    channels: 2,
                    channel_mask: 3,
                },
                samples: &samples,
                flags: Default::default(),
            })
            .unwrap();
    }
    session
        .apply(Command::SetSourceAudioProperties {
            source,
            properties: SourceAudioProperties::new(true, 0.5).unwrap(),
        })
        .unwrap();
    for engine in [&mut one, &mut split] {
        engine
            .route(&session, &output(&session), MediaTime(83_333))
            .unwrap();
    }
    let whole = one.render(0, 8).unwrap();
    let mut partitioned = split.render(0, 3).unwrap().samples;
    partitioned.extend(split.render(3, 5).unwrap().samples);
    let mut expected = [1.0, 0.0].repeat(4);
    expected.extend([0.5, 0.5].repeat(4));
    assert_eq!(whole.samples, expected);
    assert_eq!(partitioned, expected);
    assert!(whole.gaps.is_empty());
    assert_eq!(whole.source_meter_revision, None);
    assert_eq!(one.input_cursor(&binding).unwrap().generation, 1);
}
#[test]
fn program_ignores_balance_for_original_mono_after_stereo_rematrix() {
    let (mut session, source, binding) = setup();
    session
        .apply(Command::SetSourceAudioProperties {
            source,
            properties: SourceAudioProperties::new(false, 0.0).unwrap(),
        })
        .unwrap();
    let mut engine = AudioProgram::new(session.snapshot().audio(), Default::default()).unwrap();
    engine
        .route(&session, &output(&session), MediaTime(0))
        .unwrap();
    let identity = engine.identity().unwrap();
    engine
        .ingest(ProgramInputPacket {
            identity,
            binding: &binding,
            stream_generation: 1,
            sequence: 1,
            first_media_ns: 0,
            received_media_ns: 0,
            format: NativeAudioFormat {
                sample_rate: 48000,
                channels: 1,
                channel_mask: 4,
            },
            samples: &[0.25; 8],
            flags: Default::default(),
        })
        .unwrap();
    let result = engine.render(0, 8).unwrap();
    assert_eq!(result.samples, [0.25; 16]);
    assert!(result.gaps.is_empty());
}
#[test]
fn native_storage_defaults_and_strict_source_property_data_are_real() {
    let (mut session, source, _binding) = setup();
    let properties = SourceAudioProperties::new(true, 0.25).unwrap();
    session
        .apply(Command::SetSourceAudioProperties { source, properties })
        .unwrap();
    let encoded = storage::encode(&session.snapshot()).unwrap();
    let loaded = storage::decode(&encoded, true).unwrap();
    assert_eq!(
        *loaded.snapshot().source(source).unwrap().audio_properties(),
        properties
    );
    let mut value: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
    value["collection"]["sources"][0]
        .as_object_mut()
        .unwrap()
        .remove("audio_properties");
    let legacy = storage::decode(&serde_json::to_vec(&value).unwrap(), true).unwrap();
    assert!(
        legacy
            .snapshot()
            .source(source)
            .unwrap()
            .audio_properties()
            .is_default()
    );
    for invalid in [
        serde_json::json!({"force_mono":true,"balance":1.01}),
        serde_json::json!({"force_mono":false,"balance":"0.5"}),
        serde_json::json!({"force_mono":true,"balance":0.5,"sync_offset_ns":0}),
    ] {
        value["collection"]["sources"][0]["audio_properties"] = invalid;
        assert!(storage::decode(&serde_json::to_vec(&value).unwrap(), true).is_err());
    }
}

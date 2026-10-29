use super::*;
use crate::{mediafile::MediaFileReader, transition::FrameScene};
use skylive_core::{Command, MediaSource};
fn setup() -> (Session, Id, MediaSource) {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/media/no_pns/cavlc.mp4")
        .canonicalize()
        .unwrap();
    let media = MediaSource::new(path.to_str().unwrap().into(), 64, 48, true).unwrap();
    let mut session = Session::new();
    let scene = session.preview();
    let item = session
        .apply(Command::AddMediaSource {
            scene,
            name: "Movie".into(),
            media: media.clone(),
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
        .find(|i| i.id() == item)
        .unwrap()
        .source();
    session
        .apply(Command::SetAudio {
            source,
            gain: 0.5,
            muted: false,
        })
        .unwrap();
    (session, source, media)
}
#[test]
fn actual_file_aac_enters_program_mixer_by_media_identity_and_uses_source_fader() {
    let (session, source, media) = setup();
    let mut decoder =
        MediaFileReader::open(std::path::Path::new(media.path()), Default::default()).unwrap();
    let mut program = AudioProgram::new(session.snapshot().audio(), Default::default()).unwrap();
    program
        .route(
            &session,
            &FrameComposition::Scene(FrameScene::Live(session.preview())),
            MediaTime(0),
        )
        .unwrap();
    assert!(
        matches!(&program.required_bindings()[0].binding,AudioInputKey::Media{source:id,..}if *id==source)
    );
    let identity = program.identity().unwrap();
    let mut sequence = 0;
    let mut reference = Vec::new();
    while let Some(block) = decoder.next_audio().unwrap() {
        sequence += 1;
        reference.extend_from_slice(&block.samples);
        program
            .ingest_media(MediaProgramInputPacket {
                identity,
                source,
                media: &media,
                stream_generation: 1,
                sequence,
                first_media_ns: block.presentation_ns.into(),
                received_media_ns: block.presentation_ns.into(),
                format: NativeAudioFormat {
                    sample_rate: block.sample_rate,
                    channels: block.channels,
                    channel_mask: block.channel_mask,
                },
                samples: &block.samples,
                flags: Default::default(),
            })
            .unwrap();
    }
    program
        .finish_media(identity, source, &media, 1, sequence)
        .unwrap();
    assert_eq!(reference.len(), 28800);
    for start in (0..28800).step_by(1024) {
        let count = (28800 - start).min(1024);
        let block = program.render(start as u64, count).unwrap();
        assert!(block.gaps.is_empty());
        assert_eq!(block.samples.len(), count * 2);
        for (frame, &expected) in block
            .samples
            .chunks_exact(2)
            .zip(&reference[start..start + count])
        {
            assert!((frame[0] - expected * 0.5).abs() < 0.000001);
            assert_eq!(frame[0], frame[1]);
        }
    }
}
#[test]
fn native_and_file_keys_cannot_alias_and_stale_file_packets_preserve_state() {
    let (session, source, media) = setup();
    let mut program = AudioProgram::new(session.snapshot().audio(), Default::default()).unwrap();
    program
        .route(
            &session,
            &FrameComposition::Scene(FrameScene::Live(session.preview())),
            MediaTime(0),
        )
        .unwrap();
    let identity = program.identity().unwrap();
    let packet = |identity, sequence| MediaProgramInputPacket {
        identity,
        source,
        media: &media,
        stream_generation: 1,
        sequence,
        first_media_ns: 0,
        received_media_ns: 0,
        format: NativeAudioFormat {
            sample_rate: 48000,
            channels: 1,
            channel_mask: 4,
        },
        samples: &[0.25; 4],
        flags: Default::default(),
    };
    program.ingest_media(packet(identity, 1)).unwrap();
    let before = serde_json::to_value(program.status()).unwrap();
    assert!(program.ingest_media(packet(identity, 1)).is_err());
    let wrong = ProgramInputIdentity {
        epoch: identity.epoch + 1,
        ..identity
    };
    assert!(program.ingest_media(packet(wrong, 2)).is_err());
    assert_eq!(serde_json::to_value(program.status()).unwrap(), before);
    let native = AudioCaptureBinding::new(
        skylive_core::AudioFlow::Input,
        skylive_core::AudioTarget::Default,
        false,
        false,
    )
    .unwrap();
    assert_ne!(
        AudioInputKey::Native(native),
        AudioInputKey::Media {
            source,
            media: media.clone()
        }
    );
    let other = MediaSource::new(
        format!("{}.replacement.mp4", media.path()),
        media.width(),
        media.height(),
        media.has_audio(),
    )
    .unwrap();
    assert!(
        program
            .ingest_media(MediaProgramInputPacket {
                media: &other,
                ..packet(identity, 2)
            })
            .is_err()
    );
    assert_eq!(serde_json::to_value(program.status()).unwrap(), before);
    program
        .finish_media(identity, source, &media, 1, 1)
        .unwrap();
    assert!(program.ingest_media(packet(identity, 2)).is_err());
}
#[test]
fn lifecycle_only_rebind_preserves_pcm_group_and_buffered_signal() {
    let (mut session, source, media) = setup();
    let mut program = AudioProgram::new(session.snapshot().audio(), Default::default()).unwrap();
    let output = FrameComposition::Scene(FrameScene::Live(session.preview()));
    program.route(&session, &output, MediaTime(0)).unwrap();
    let identity = program.identity().unwrap();
    let samples = vec![0.25; 2048];
    program
        .ingest_media(MediaProgramInputPacket {
            identity,
            source,
            media: &media,
            stream_generation: 7,
            sequence: 1,
            first_media_ns: 0,
            received_media_ns: 0,
            format: NativeAudioFormat {
                sample_rate: 48000,
                channels: 1,
                channel_mask: 4,
            },
            samples: &samples,
            flags: Default::default(),
        })
        .unwrap();
    let published = std::sync::Arc::new(program.render(0, 1024).unwrap());
    let published_json = serde_json::to_value(&published).unwrap();
    assert!(published.gaps.is_empty());
    let changed = media.clone().with_playback(true, false, true, false);
    session
        .apply(Command::RebindMediaSource {
            source,
            media: changed.clone(),
        })
        .unwrap();
    program
        .route(&session, &output, MediaTime(21_333_333))
        .unwrap();
    assert_eq!(
        program.media_input_cursor(source, &changed),
        Some(ProgramInputCursor {
            generation: 7,
            sequence: 1
        })
    );
    let block = program.render(1024, 1024).unwrap();
    assert_eq!(
        block.source_meter_revision,
        Some(session.snapshot().revision())
    );
    assert_eq!(serde_json::to_value(&published).unwrap(), published_json);
    assert!(block.gaps.is_empty());
    assert!(block.samples.iter().all(|s| *s == 0.125));
    use std::hash::{Hash, Hasher};
    let mut a = std::collections::hash_map::DefaultHasher::new();
    let mut b = std::collections::hash_map::DefaultHasher::new();
    AudioInputKey::Media { source, media }.hash(&mut a);
    AudioInputKey::Media {
        source,
        media: changed,
    }
    .hash(&mut b);
    assert_eq!(a.finish(), b.finish());
}
#[test]
fn frozen_removed_root_retains_actual_file_audio_and_rejects_foreign_session() {
    let (mut session, source, media) = setup();
    let root = session.preview();
    let mut controller = crate::transition::Controller::new();
    controller
        .set_studio(&mut session, true, MediaTime(0))
        .unwrap();
    let output = controller.sync(&mut session, MediaTime(0)).unwrap().output;
    assert!(matches!(
        &output,
        FrameComposition::Scene(FrameScene::Frozen(_))
    ));
    session
        .apply(Command::AddScene {
            name: "Survivor".into(),
        })
        .unwrap();
    session.apply(Command::RemoveScene { scene: root }).unwrap();
    assert!(session.snapshot().scene(root).is_none());
    assert!(session.snapshot().source(source).is_none());
    let mut program = AudioProgram::new(session.snapshot().audio(), Default::default()).unwrap();
    program.route(&session, &output, MediaTime(0)).unwrap();
    let identity = program.identity().unwrap();
    let mut decoder =
        MediaFileReader::open(std::path::Path::new(media.path()), Default::default()).unwrap();
    let packet = decoder.next_audio().unwrap().unwrap();
    program
        .ingest_media(MediaProgramInputPacket {
            identity,
            source,
            media: &media,
            stream_generation: 1,
            sequence: 1,
            first_media_ns: packet.presentation_ns.into(),
            received_media_ns: 0,
            format: NativeAudioFormat {
                sample_rate: packet.sample_rate,
                channels: packet.channels,
                channel_mask: packet.channel_mask,
            },
            samples: &packet.samples,
            flags: Default::default(),
        })
        .unwrap();
    let block = program.render(0, 512).unwrap();
    assert!(block.gaps.is_empty());
    assert!(block.samples.iter().any(|s| s.abs() > 0.01));
    let foreign = Session::new();
    assert!(crate::audio::derive_audio_plan(&foreign, &output).is_err());
}

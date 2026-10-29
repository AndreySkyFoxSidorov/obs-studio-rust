//! Pure supplied PCM, exact sample-boundary oracles, no file/native decoder.
use super::*;
use crate::{
    media_source::control::MediaBarrier,
    transition::{FrameComposition, FrameScene},
};
use skylive_core::{Command, MediaSource, Session, SpeakerLayout};
use std::sync::{Arc, mpsc};
fn fixture(rate: u32) -> (AudioProgram, Id, Arc<MediaSource>) {
    let mut s = Session::new();
    s.apply(Command::SetAudioSettings {
        settings: AudioSettings::new(rate, SpeakerLayout::Mono).unwrap(),
    })
    .unwrap();
    let media =
        Arc::new(MediaSource::new("C:/synthetic-supplied-pcm.mp4".into(), 64, 48, true).unwrap());
    let scene = s.preview();
    s.apply(Command::AddMediaSource {
        scene,
        name: "Supplied PCM only".into(),
        media: (*media).clone(),
    })
    .unwrap();
    let source = s.snapshot().sources()[0].id();
    let mut program =
        AudioProgram::new_buffered(s.snapshot().audio(), ProgramAudioLimits::default()).unwrap();
    program
        .route(
            &s,
            &FrameComposition::Scene(FrameScene::Live(scene)),
            MediaTime(0),
        )
        .unwrap();
    (program, source, media)
}
fn barrier(
    source: Id,
    media: &Arc<MediaSource>,
    generation: u64,
    sequence: u64,
    at: u64,
    suspend: bool,
) -> MediaBarrier {
    let (reply, _receiver) = mpsc::sync_channel(1);
    MediaBarrier {
        source,
        media: media.clone(),
        generation,
        next_generation: generation + 1,
        last_sequence: sequence,
        effective_ns: at,
        suspend,
        reply,
    }
}
#[test]
fn partial_pre_pause_fifo_is_rendered_then_intentional_silence_never_grows_or_stops_the_input() {
    let (mut program, source, media) = fixture(48_000);
    let data = vec![0.25; 4096];
    program
        .ingest_media(MediaProgramInputPacket {
            identity: program.identity().unwrap(),
            source,
            media: &media,
            stream_generation: 1,
            sequence: 1,
            first_media_ns: 0,
            received_media_ns: 0,
            format: NativeAudioFormat {
                sample_rate: 48_000,
                channels: 1,
                channel_mask: 4,
            },
            samples: &data,
            flags: Default::default(),
        })
        .unwrap();
    let b = barrier(source, &media, 1, 1, 10_000_000, true);
    assert_eq!(
        program
            .control_media(program.identity().unwrap(), &b)
            .unwrap()
            .effective_frame,
        480
    );
    let readiness = program.buffered_prepare(0).unwrap();
    assert_eq!(readiness.pending_inputs, 0);
    assert_eq!(readiness.minimum_frame, Some(0));
    let first = program.render(0, 1024).unwrap();
    assert!(first.samples[..480].iter().all(|v| *v == 0.25));
    assert!(first.samples[480..].iter().all(|v| *v == 0.0));
    assert!(first.gaps.is_empty());
    let discarded = program.buffered_discard(0).unwrap();
    assert_eq!(discarded.discarded_frames, 480);
    assert_eq!(discarded.stopped_inputs, 0);
    for start in [1024, 2048, 3072] {
        let ready = program.buffered_prepare(start).unwrap();
        assert_eq!(ready.pending_inputs, 0);
        assert_eq!(ready.minimum_frame, Some(start));
        let output = program.render(start as u64, 1024).unwrap();
        assert!(output.samples.iter().all(|v| *v == 0.0));
        assert!(output.gaps.is_empty());
        assert_eq!(program.buffered_discard(start).unwrap().stopped_inputs, 0);
    }
    let before = program.status().data_generation;
    assert!(
        program
            .ingest_media(MediaProgramInputPacket {
                identity: program.identity().unwrap(),
                source,
                media: &media,
                stream_generation: 1,
                sequence: 2,
                first_media_ns: 85_333_333,
                received_media_ns: 85_333_333,
                format: NativeAudioFormat {
                    sample_rate: 48_000,
                    channels: 1,
                    channel_mask: 4
                },
                samples: &data[..1024],
                flags: Default::default()
            })
            .is_err()
    );
    assert_eq!(program.status().data_generation, before);
}
#[test]
fn control_quantization_is_ceil_on_project_grid_not_a_floor_or_native_rate_guess() {
    for (rate, cases) in [
        (44_100, [(1, 1), (22_675, 1), (22_676, 2)]),
        (48_000, [(1, 1), (20_833, 1), (20_834, 2)]),
    ] {
        for (ns, expected) in cases {
            let (mut program, source, media) = fixture(rate);
            let b = barrier(source, &media, 0, 0, ns, true);
            let receipt = program
                .control_media(program.identity().unwrap(), &b)
                .unwrap();
            assert_eq!(receipt.effective_frame, expected);
            assert_eq!(receipt.late_frames, 0);
        }
    }
}
#[test]
fn unemitted_control_history_is_bounded_and_overload_preserves_ordering_and_data_generation() {
    let (mut program, source, media) = fixture(48_000);
    let identity = program.identity().unwrap();
    let mut generation = 0;
    for index in 0..64 {
        for suspend in [true, false] {
            let at = (index * 2 + usize::from(!suspend)) as u64 * 1_000_000;
            program
                .control_media(
                    identity,
                    &barrier(source, &media, generation, 0, at, suspend),
                )
                .unwrap();
            generation += 1;
        }
    }
    let before = program.status().data_generation;
    let cursor = program.media_input_cursor(source, &media);
    assert!(matches!(
        program.control_media(
            identity,
            &barrier(source, &media, generation, 0, 128_000_000, true)
        ),
        Err(ProgramAudioError::Backpressure(_))
    ));
    assert_eq!(program.status().data_generation, before);
    assert_eq!(program.media_input_cursor(source, &media), cursor);
    let mut stale = barrier(source, &media, generation, 0, 129_000_000, false);
    stale.generation = 0;
    stale.next_generation = 1;
    assert!(program.control_media(identity, &stale).is_err());
    assert_eq!(program.status().data_generation, before);
}

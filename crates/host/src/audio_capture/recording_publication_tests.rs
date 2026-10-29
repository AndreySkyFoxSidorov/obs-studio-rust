// SPDX-License-Identifier: GPL-2.0-or-later
//! Deterministic recording/publication independence. No physical native device
//! or real codec is used; the separate opt-in acceptance tests the native codec.
use super::*;
use crate::recording::{
    RecordingConfig, RecordingError, RecordingEvent, RecordingFactory, RecordingFrame,
    RecordingOwner, RecordingWriter,
};
use std::{
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

struct PcmWriter {
    path: PathBuf,
    pcm: Arc<Mutex<Vec<i16>>>,
}
impl RecordingWriter for PcmWriter {
    fn video(&mut self, _: &skylive_media::RgbaFrame, _: u64) -> Result<(), RecordingError> {
        Ok(())
    }
    fn audio(&mut self, samples: &[i16], _: u64) -> Result<(), RecordingError> {
        self.pcm.lock().unwrap().extend_from_slice(samples);
        Ok(())
    }
    fn finalize(&mut self) -> Result<(), RecordingError> {
        let mut file = std::fs::File::create(&self.path).unwrap();
        for kind in [b"ftyp", b"moov", b"mdat"] {
            file.write_all(&8u32.to_be_bytes()).unwrap();
            file.write_all(kind).unwrap();
        }
        Ok(())
    }
}
fn fake_factory(pcm: Arc<Mutex<Vec<i16>>>) -> RecordingFactory {
    Box::new(move |_, path| {
        std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(path)
            .unwrap();
        Ok(Box::new(PcmWriter {
            path: path.into(),
            pcm,
        }))
    })
}
fn start_recorder(session: &Session, path: &Path, pcm: Arc<Mutex<Vec<i16>>>) -> RecordingOwner {
    let mut owner = RecordingOwner::new().unwrap();
    owner
        .start_with(
            session.identity(),
            RecordingConfig::from_session(session, path.into()).unwrap(),
            MediaTime(0),
            fake_factory(pcm),
        )
        .unwrap();
    owner
        .video(RecordingFrame {
            session: session.identity(),
            time: MediaTime(0),
            model: session.snapshot(),
            output: FrameComposition::Scene(FrameScene::Live(session.preview())),
            playback: crate::playback::PlaybackSnapshot::at_time(session, MediaTime(0)),
            capture: Arc::new(crate::capture::CaptureSnapshot::empty(session.identity())),
            resource_generation: 0,
        })
        .unwrap();
    owner
}
fn feed_pair(a: &mut LiveProgram, b: &mut LiveProgram, first: usize, sequence: &mut u64) {
    for part in 0..4 {
        let first = first + part * 256;
        let values = (first..first + 256)
            .map(|frame| {
                (0.1 * (std::f64::consts::TAU * 1000.0 * frame as f64 / 48000.0).sin()) as f32
            })
            .collect();
        let packet = packet(*sequence, first, values);
        *sequence += 1;
        a.ingest(&packet);
        b.ingest(&packet);
    }
}
fn finish_recording(owner: &mut RecordingOwner) {
    let until = Instant::now() + Duration::from_secs(3);
    loop {
        match owner.poll().unwrap() {
            Some(RecordingEvent::Started) | None => {}
            Some(RecordingEvent::Finished { audio_frames, .. }) => {
                assert_eq!(audio_frames, 24 * 1024);
                break;
            }
            Some(RecordingEvent::Failed(error)) => panic!("recording failed: {error:?}"),
            Some(RecordingEvent::PauseChanged { .. }) => panic!("unexpected recording pause event"),
        }
        assert!(Instant::now() < until);
        std::thread::sleep(Duration::from_millis(1));
    }
    owner.join().unwrap();
}

#[test]
fn identical_ready_pcm_records_bit_exactly_when_presentation_is_drained_or_full() {
    let mut session = Session::new();
    settings(&mut session, 48000, SpeakerLayout::Mono);
    session
        .apply(Command::SetVideo {
            width: 16,
            height: 16,
            fps_num: 30,
            fps_den: 1,
        })
        .unwrap();
    let source = add(&mut session, "Publication invariant");
    let mut eq = skylive_core::AudioFilter::simple_eq();
    if let skylive_core::AudioFilter::SimpleEq { mid_db, .. } = &mut eq {
        *mid_db = 6.0;
    }
    session
        .apply(Command::SetSourceAudioFilters {
            source,
            filters: vec![eq, skylive_core::AudioFilter::gate()],
        })
        .unwrap();
    let (mut full, full_receiver, _) = owner(&session, 0, 8);
    let (mut drained, drained_receiver, _) = owner(&session, 0, 8);
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let directory = std::env::temp_dir().join(format!(
        "skylive-publication-ab-{}-{nonce}",
        std::process::id()
    ));
    std::fs::create_dir(&directory).unwrap();
    let a_path = directory.join("full.mp4");
    let b_path = directory.join("drained.mp4");
    let a_pcm = Arc::new(Mutex::new(Vec::new()));
    let b_pcm = Arc::new(Mutex::new(Vec::new()));
    let mut a = start_recorder(&session, &a_path, a_pcm.clone());
    let mut b = start_recorder(&session, &b_path, b_pcm.clone());
    full.attach_recording(a.audio_sink(session.snapshot().audio()).unwrap())
        .unwrap();
    drained
        .attach_recording(b.audio_sink(session.snapshot().audio()).unwrap())
        .unwrap();
    // Both engines see the same timestamped packets and logical deadlines.
    // An explicitly ready two-block source queue removes wall scheduling jitter
    // from this test of lossy presentation publication, without changing DSP.
    let mut sequence = 1;
    feed_pair(&mut full, &mut drained, 0, &mut sequence);
    feed_pair(&mut full, &mut drained, 1024, &mut sequence);
    for index in 0..24 {
        full.tick(full.next_deadline().unwrap());
        drained.tick(drained.next_deadline().unwrap());
        let visible = drained_receiver.try_recv().unwrap();
        assert!(visible.block.gaps.is_empty());
        assert_eq!(visible.stats.pending_inputs, 0);
        assert_eq!(full.stats().pending_inputs, 0);
        assert_eq!(full.stats().input_errors, 0);
        drop(visible);
        feed_pair(&mut full, &mut drained, (index + 2) * 1024, &mut sequence);
    }
    assert_eq!(full.stats().output_blocks, 24);
    assert_eq!(drained.stats().output_blocks, 24);
    assert_eq!(full.stats().dropped_blocks, 16);
    assert_eq!(drained.stats().dropped_blocks, 0);
    a.stop().unwrap();
    b.stop().unwrap();
    full.tick(full.next_deadline().unwrap());
    drained.tick(drained.next_deadline().unwrap());
    finish_recording(&mut a);
    finish_recording(&mut b);
    let actual = a_pcm.lock().unwrap();
    let control = b_pcm.lock().unwrap();
    assert_eq!(actual.len(), 24 * 1024);
    assert_eq!(
        *actual, *control,
        "queueFull/reap/statistics cannot alter recorded samples or reset source DSP"
    );
    let tail = &actual[4096..];
    let (mut sine, mut cosine) = (0.0, 0.0);
    for (index, &sample) in tail.iter().enumerate() {
        let phase = std::f64::consts::TAU * 1000.0 * index as f64 / 48000.0;
        let value = f64::from(sample) / 32768.0;
        sine += value * phase.sin();
        cosine += value * phase.cos();
    }
    let amplitude = 2.0 * sine.hypot(cosine) / tail.len() as f64;
    assert!(
        (amplitude - 0.1995262315).abs() < 0.0005,
        "independent+6dB1kHz oracle: {amplitude}"
    );
    drop(control);
    drop(actual);
    drop(full_receiver);
    drop(drained_receiver);
    std::fs::remove_file(a_path).unwrap();
    std::fs::remove_file(b_path).unwrap();
    std::fs::remove_dir(directory).unwrap();
}

// Append to audio_capture/recording_publication_tests.rs, beside its existing
// injected PcmWriter/route/native-packet helpers. No codec or device is used.
#[test]
fn recording_and_streaming_share_dsp_once_and_stream_stop_keeps_recording_history_exact() {
    use crate::audio_output::{AudioOutput, OutputKey, OutputKind};
    use std::sync::atomic::AtomicUsize;
    let mut session = fixed_session();
    settings(&mut session, 48000, SpeakerLayout::Mono);
    session
        .apply(Command::SetVideo {
            width: 16,
            height: 16,
            fps_num: 30,
            fps_den: 1,
        })
        .unwrap();
    let source = add(&mut session, "Two independent consumers");
    let mut eq = skylive_core::AudioFilter::simple_eq();
    if let skylive_core::AudioFilter::SimpleEq { mid_db, .. } = &mut eq {
        *mid_db = 6.0;
    }
    session
        .apply(Command::SetSourceAudioFilters {
            source,
            filters: vec![eq, skylive_core::AudioFilter::compressor()],
        })
        .unwrap();
    let (mut actual, visible, _) = owner(&session, 0, 8);
    let (mut reference, reference_visible, _) = owner(&session, 0, 8);
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let directory =
        std::env::temp_dir().join(format!("skylive-two-output-{}-{nonce}", std::process::id()));
    std::fs::create_dir(&directory).unwrap();
    let recorded = Arc::new(Mutex::new(Vec::new()));
    let mut recorder = start_recorder(&session, &directory.join("recording.mp4"), recorded.clone());
    actual
        .attach_recording(recorder.audio_sink(session.snapshot().audio()).unwrap())
        .unwrap();
    let stream_key = OutputKey {
        kind: OutputKind::Streaming,
        session: session.identity(),
        id: 77,
        epoch: 1,
    };
    let origin = MediaTime((4u128 * 1024 * 1_000_000_000 / 48000) as u64);
    let (stream, mut stream_receiver, stream_control) = crate::audio_output::channel_for_test(
        stream_key,
        session.snapshot().audio(),
        Some(origin),
        Arc::new(AtomicUsize::new(0)),
    )
    .unwrap();
    let mut stream = Some(stream);
    let mut expected = Vec::new();
    let mut streamed = Vec::new();
    let mut sequence = 1;
    feed_pair(&mut actual, &mut reference, 0, &mut sequence);
    feed_pair(&mut actual, &mut reference, 1024, &mut sequence);
    for index in 0..24 {
        if index == 4 {
            actual
                .attach_output(AudioOutput::Streaming(stream.take().unwrap()))
                .unwrap();
        }
        if index == 12 {
            stream_control.request_stop();
        }
        actual.tick(actual.next_deadline().unwrap());
        reference.tick(reference.next_deadline().unwrap());
        let reference_block = reference_visible.try_recv().unwrap();
        expected.extend_from_slice(&reference_block.block.samples);
        if index < 8 {
            drop(visible.try_recv().unwrap());
        }
        // The actual GUI queue then remains full for the rest of this test;
        // both direct outputs must stay independent of presentation polling.
        if (4..12).contains(&index) {
            let delivery = stream_receiver.poll().unwrap();
            assert_eq!(delivery.absolute_first_frame(), index * 1024);
            assert_eq!(delivery.samples(), reference_block.block.samples);
            assert_eq!(delivery.gap_before_frames(), 0);
            streamed.extend_from_slice(delivery.samples());
        } else {
            assert!(stream_receiver.poll().is_none());
        }
        if index >= 12 {
            assert!(stream_receiver.producer_ended());
            assert!(actual.has_recording());
        }
        assert_eq!(
            actual.stats().input_packets,
            reference.stats().input_packets
        );
        assert_eq!(actual.stats().input_frames, reference.stats().input_frames);
        assert_eq!(actual.stats().input_errors, 0);
        feed_pair(
            &mut actual,
            &mut reference,
            (index as usize + 2) * 1024,
            &mut sequence,
        );
    }
    assert_eq!(streamed, &expected[4 * 1024..12 * 1024]);
    assert!(
        actual.stats().dropped_blocks > 0,
        "GUI publication really saturated"
    );
    recorder.stop().unwrap();
    actual.tick(actual.next_deadline().unwrap());
    finish_recording(&mut recorder);
    let expected: Vec<i16> = expected
        .into_iter()
        .map(|value| {
            let value = value.clamp(-1.0, 1.0);
            (value * if value < 0.0 { 32768.0 } else { 32767.0 }).round() as i16
        })
        .collect();
    assert_eq!(
        *recorded.lock().unwrap(),
        expected,
        "source EQ/compressor history was not reset or processed twice by output attach/stop"
    );
    drop(recorder);
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn saturated_stream_ingress_fails_that_slot_while_recording_continues() {
    use crate::audio_output::{AudioOutput, OutputFault, OutputKey, OutputKind};
    use std::sync::atomic::AtomicUsize;
    let mut session = fixed_session();
    settings(&mut session, 48000, SpeakerLayout::Mono);
    session
        .apply(Command::SetVideo {
            width: 16,
            height: 16,
            fps_num: 30,
            fps_den: 1,
        })
        .unwrap();
    add(&mut session, "Ingress independence");
    let (mut live, visible, _) = owner(&session, 0, 8);
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let directory = std::env::temp_dir().join(format!(
        "skylive-output-overload-{}-{nonce}",
        std::process::id()
    ));
    std::fs::create_dir(&directory).unwrap();
    let recorded = Arc::new(Mutex::new(Vec::new()));
    let mut recorder = start_recorder(&session, &directory.join("recording.mp4"), recorded.clone());
    live.attach_recording(recorder.audio_sink(session.snapshot().audio()).unwrap())
        .unwrap();
    let (stream, mut receiver, control) = crate::audio_output::channel_for_test(
        OutputKey {
            kind: OutputKind::Streaming,
            session: session.identity(),
            id: 88,
            epoch: 1,
        },
        session.snapshot().audio(),
        Some(MediaTime(0)),
        Arc::new(AtomicUsize::new(0)),
    )
    .unwrap();
    live.attach_output(AudioOutput::Streaming(stream)).unwrap();
    let mut sequence = 1;
    let feed = |live: &mut LiveProgram, first: usize, sequence: &mut u64| {
        for part in 0..4 {
            let at = first + part * 256;
            live.ingest(&packet(*sequence, at, vec![0.1; 256]));
            *sequence += 1;
        }
    };
    feed(&mut live, 0, &mut sequence);
    feed(&mut live, 1024, &mut sequence);
    for index in 0..44 {
        live.tick(live.next_deadline().unwrap());
        drop(visible.try_recv().unwrap());
        // Wait for actual recording consumption so this test isolates the
        // intentionally stalled Streaming endpoint, not a scheduler burst.
        let until = Instant::now() + Duration::from_secs(3);
        while recorded.lock().unwrap().len() < (index + 1) * 1024 {
            assert!(Instant::now() < until);
            std::thread::yield_now();
        }
        feed(&mut live, (index + 2) * 1024, &mut sequence);
    }
    assert_eq!(control.stats().fault, Some(OutputFault::Backpressure));
    assert!(receiver.producer_ended());
    assert!(live.has_recording());
    for index in 0..40 {
        assert_eq!(
            receiver.poll().unwrap().absolute_first_frame(),
            index * 1024
        );
    }
    assert!(receiver.drained());
    assert_eq!(recorded.lock().unwrap().len(), 44 * 1024);
    recorder.stop().unwrap();
    live.tick(live.next_deadline().unwrap());
    let until = Instant::now() + Duration::from_secs(3);
    loop {
        match recorder.poll().unwrap() {
            Some(RecordingEvent::Finished { audio_frames, .. }) => {
                assert_eq!(audio_frames, 44 * 1024);
                break;
            }
            Some(RecordingEvent::Failed(error)) => panic!("{error:?}"),
            _ => {}
        }
        assert!(Instant::now() < until);
        std::thread::yield_now();
    }
    recorder.join().unwrap();
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn stopping_recording_leaves_streaming_and_program_sample_cursor_running() {
    use crate::audio_output::{AudioOutput, OutputKey, OutputKind};
    use std::sync::atomic::AtomicUsize;
    let mut session = fixed_session();
    settings(&mut session, 48000, SpeakerLayout::Mono);
    session
        .apply(Command::SetVideo {
            width: 16,
            height: 16,
            fps_num: 30,
            fps_den: 1,
        })
        .unwrap();
    add(&mut session, "Recording stop independent");
    let (mut live, visible, _) = owner(&session, 0, 8);
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let directory = std::env::temp_dir().join(format!(
        "skylive-record-stop-{}-{nonce}",
        std::process::id()
    ));
    std::fs::create_dir(&directory).unwrap();
    let recorded = Arc::new(Mutex::new(Vec::new()));
    let mut recorder = start_recorder(&session, &directory.join("recording.mp4"), recorded.clone());
    live.attach_recording(recorder.audio_sink(session.snapshot().audio()).unwrap())
        .unwrap();
    let (stream, mut receiver, control) = crate::audio_output::channel_for_test(
        OutputKey {
            kind: OutputKind::Streaming,
            session: session.identity(),
            id: 99,
            epoch: 1,
        },
        session.snapshot().audio(),
        Some(MediaTime(0)),
        Arc::new(AtomicUsize::new(0)),
    )
    .unwrap();
    live.attach_output(AudioOutput::Streaming(stream)).unwrap();
    let mut sequence = 1;
    let feed = |live: &mut LiveProgram, first: usize, sequence: &mut u64| {
        for part in 0..4 {
            let at = first + part * 256;
            live.ingest(&packet(*sequence, at, vec![0.1; 256]));
            *sequence += 1;
        }
    };
    feed(&mut live, 0, &mut sequence);
    feed(&mut live, 1024, &mut sequence);
    for index in 0..24 {
        if index == 12 {
            recorder.stop().unwrap();
        }
        live.tick(live.next_deadline().unwrap());
        drop(visible.try_recv().unwrap());
        let delivered = receiver.poll().unwrap();
        assert_eq!(delivered.absolute_first_frame(), index * 1024);
        assert_eq!(delivered.frames(), 1024);
        drop(delivered);
        if index >= 12 {
            assert!(!live.has_recording());
            assert!(!control.stats().producer_ended);
        }
        feed(&mut live, (index as usize + 2) * 1024, &mut sequence);
    }
    let until = Instant::now() + Duration::from_secs(3);
    loop {
        match recorder.poll().unwrap() {
            Some(RecordingEvent::Finished { audio_frames, .. }) => {
                assert_eq!(audio_frames, 12 * 1024);
                break;
            }
            Some(RecordingEvent::Failed(error)) => panic!("{error:?}"),
            _ => {}
        }
        assert!(Instant::now() < until);
        std::thread::yield_now();
    }
    recorder.join().unwrap();
    assert_eq!(recorded.lock().unwrap().len(), 12 * 1024);
    control.request_stop();
    live.tick(live.next_deadline().unwrap());
    assert!(receiver.drained());
    assert!(control.stats().fault.is_none());
    std::fs::remove_dir_all(directory).unwrap();
}

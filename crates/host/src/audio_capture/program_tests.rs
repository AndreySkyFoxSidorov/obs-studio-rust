use super::*;
use crate::audio_capture::{
    AudioTimingOrigin, NativeAudioFormat, NativePacketFlags, NativePcmBlock,
};
use crate::transition::{FrameComposition, FrameScene};
use skylive_core::{AudioFlow, AudioTarget, Command, Id, Session, SpeakerLayout};
#[path = "recording_publication_tests.rs"]
mod publication_tests;

fn binding() -> AudioCaptureBinding {
    AudioCaptureBinding::new(AudioFlow::Input, AudioTarget::Default, false, false).unwrap()
}
// The pre-existing output/retirement oracles explicitly select the previous
// fixed four-tick profile. Adaptive startup/wait traces have their own tests.
fn fixed_session() -> Session {
    let mut session = Session::new();
    let settings = (session.snapshot().audio())
        .with_buffering(skylive_core::AudioBuffering::new(80, true).unwrap())
        .unwrap();
    session
        .apply(Command::SetAudioSettings { settings })
        .unwrap();
    session
}
fn settings(session: &mut Session, rate: u32, layout: SpeakerLayout) {
    session
        .apply(Command::SetAudioSettings {
            settings: AudioSettings::new(rate, layout)
                .unwrap()
                .with_buffering(session.snapshot().audio().buffering())
                .unwrap(),
        })
        .unwrap();
}
fn add(session: &mut Session, name: &str) -> Id {
    let live = session.preview();
    let item = session
        .apply(Command::AddAudioCaptureSource {
            scene: live,
            name: name.into(),
            binding: binding(),
        })
        .unwrap()
        .created
        .unwrap();
    session
        .snapshot()
        .scene(live)
        .unwrap()
        .items()
        .iter()
        .find(|item_ref| item_ref.id() == item)
        .unwrap()
        .source()
}
fn route(session: &Session, time: u64) -> AudioRouteSnapshot {
    AudioRouteSnapshot::capture(
        session,
        &FrameComposition::Scene(FrameScene::Live(session.preview())),
        MediaTime(time),
    )
    .unwrap()
}
fn packet(sequence: u64, first_frame: usize, values: Vec<f32>) -> CapturedAudioPacket {
    let frames = values.len();
    let timestamp = (first_frame as u128 * 1_000_000_000 / 48000) as i128;
    CapturedAudioPacket {
        resolved_device: None,
        binding: binding(),
        sequence,
        requested_at_ns: 0,
        stream_generation: 1,
        first_sample_media_ns: timestamp,
        timing_origin: AudioTimingOrigin::ReceiveEstimate,
        coalesced_packets: 0,
        meters: Vec::new(),
        block: NativePcmBlock {
            format: NativeAudioFormat {
                sample_rate: 48000,
                channels: 1,
                channel_mask: 4,
            },
            frames,
            samples: values,
            device_timestamp_ns: None,
            graph_timestamp_ns: None,
            directshow_clock: None,
            host_received_ns: ((first_frame + frames) as u128 * 1_000_000_000 / 48000) as u64,
            device_frame_position: Some(first_frame as u64),
            flags: NativePacketFlags::default(),
        },
    }
}
fn owner(
    session: &Session,
    at: u64,
    capacity: usize,
) -> (LiveProgram, mpsc::Receiver<ProgramCaptureOutput>, Instant) {
    let now = Instant::now();
    let (send, recv) = mpsc::sync_channel(capacity);
    let mut live = LiveProgram::new(route(session, at), 1, 1, now, send).unwrap();
    // Advance the declared fixed preroll one wall callback at a time; these
    // output-focused fixtures begin before the first successful output.
    let waits = live.clock.waiting_ticks();
    for _ in 0..waits {
        live.tick(live.next_deadline().unwrap());
        assert!(recv.try_recv().is_err());
    }
    (live, recv, now)
}
fn finish_time(anchor: Instant, start: u64, blocks: u64, rate: u32, media_anchor: u64) -> Instant {
    anchor
        .checked_add(Duration::from_nanos(
            ((u128::from(start + 1024 * (blocks + 4)) * 1_000_000_000 / u128::from(rate)) as u64)
                - media_anchor,
        ))
        .unwrap()
}

#[test]
fn fixed_four_block_buffer_uses_cumulative_rational_44100_deadlines_and_signed_grid_origin() {
    let mut session = fixed_session();
    settings(&mut session, 44100, SpeakerLayout::Mono);
    let (mut live, recv, anchor) = owner(&session, 1, 8);
    let first = finish_time(anchor, 1, 1, 44100, 1);
    assert_eq!(live.next_deadline(), Some(first));
    assert_eq!(live.stats().buffer_blocks, 4);
    assert!(live.stats().running);
    live.tick(first - Duration::from_nanos(1));
    assert!(matches!(recv.try_recv(), Err(mpsc::TryRecvError::Empty)));
    for block in 0..6 {
        let due = finish_time(anchor, 1, block + 1, 44100, 1);
        live.tick(due);
        let output = recv.try_recv().unwrap();
        assert_eq!(output.block.start_frame, 1 + block * 1024);
        assert_eq!(output.block.frames, 1024);
        assert_eq!(output.block.samples, [0.0; 1024]);
        assert_eq!(output.stats.output_blocks, block + 1);
        assert!(!live.needs_immediate());
        assert_eq!(
            live.next_deadline(),
            Some(finish_time(anchor, 1, block + 2, 44100, 1))
        );
    }
    assert_eq!(
        live.media_now(anchor + Duration::from_nanos(25)).unwrap(),
        MediaTime(26)
    );
    assert!(live.media_now(anchor - Duration::from_nanos(1)).is_err());
}

#[test]
fn every_native_packet_is_ingested_and_shared_binding_preserves_independent_source_faders() {
    let mut session = fixed_session();
    settings(&mut session, 48000, SpeakerLayout::Mono);
    let a = add(&mut session, "A audio");
    let b = add(&mut session, "B audio");
    session
        .apply(Command::SetAudio {
            source: b,
            gain: 0.5,
            muted: false,
        })
        .unwrap();
    let (mut live, recv, _) = owner(&session, 0, 8);
    assert_eq!(live.required_bindings(), [binding()]);
    for (index, value) in [0.125, 0.25, 0.375, 0.5].into_iter().enumerate() {
        live.ingest(&packet(index as u64 + 1, index * 256, vec![value; 256]));
    }
    session
        .apply(Command::SetAudio {
            source: a,
            gain: 1.0,
            muted: true,
        })
        .unwrap();
    live.update_route(route(&session, 10_666_666)).unwrap();
    live.tick(live.next_deadline().unwrap());
    let output = recv.try_recv().unwrap();
    assert_eq!(output.stats.input_packets, 4);
    assert_eq!(output.stats.input_frames, 1024);
    assert_eq!(output.stats.input_errors, 0);
    assert!(output.block.gaps.is_empty());
    assert_eq!(output.block.clipped_samples, [0]);
    for (index, expected) in [0.1875, 0.375, 0.1875, 0.25].into_iter().enumerate() {
        assert!(
            output.block.samples[index * 256..(index + 1) * 256]
                .iter()
                .all(|value| *value == expected)
        );
    }
    assert_eq!(
        output
            .block
            .source_meters
            .iter()
            .find(|meter| meter.source == a)
            .unwrap()
            .channels[0]
            .peak,
        0.25
    );
    assert_eq!(
        output
            .block
            .source_meters
            .iter()
            .find(|meter| meter.source == b)
            .unwrap()
            .channels[0]
            .peak,
        0.25
    );
}

#[test]
fn catchup_is_four_blocks_per_turn_and_never_reanchors_due_to_scheduler_delay() {
    let mut session = fixed_session();
    settings(&mut session, 48000, SpeakerLayout::Mono);
    let (mut live, recv, anchor) = owner(&session, 0, 32);
    let late = finish_time(anchor, 0, 20, 48000, 0);
    live.tick(late);
    assert_eq!(live.stats().output_blocks, 4);
    assert!(live.needs_immediate());
    for index in 0..4 {
        assert_eq!(recv.try_recv().unwrap().block.start_frame, index * 1024);
    }
    live.tick(late);
    assert_eq!(live.stats().output_blocks, 8);
    assert!(live.needs_immediate());
    for index in 4..8 {
        assert_eq!(recv.try_recv().unwrap().block.start_frame, index * 1024);
    }
    assert_eq!(
        live.next_deadline(),
        Some(finish_time(anchor, 0, 9, 48000, 0))
    );
}

#[test]
fn one_hour_suspension_skips_exact_unrendered_blocks_in_one_turn_and_future_pcm_recovers() {
    // Literal independent counts for an hour's worth of deadlines with the
    // initial ceil grid preserved: 48k@0ns and 44.1k@1ns respectively.
    for (rate, at, first, skipped) in [(48000, 0, 0, 168742), (44100, 1, 1, 155031)] {
        let mut session = fixed_session();
        settings(&mut session, rate, SpeakerLayout::Mono);
        add(&mut session, "Input");
        let (mut live, recv, anchor) = owner(&session, at, 8);
        let resumed = anchor + Duration::from_secs(3600);
        live.tick(resumed);
        let stats = live.stats();
        assert_eq!(stats.skipped_blocks, skipped);
        assert_eq!(stats.skipped_frames, skipped * 1024);
        assert_eq!(stats.output_blocks, 4);
        assert_eq!(stats.dropped_blocks, 0);
        assert_eq!(stats.dropped_frames, 0);
        assert!(!live.needs_immediate());
        assert!(live.next_deadline().unwrap() > resumed);
        for index in 0..4 {
            let output = recv.try_recv().unwrap();
            assert_eq!(output.block.start_frame, first + (skipped + index) * 1024);
            assert_eq!(output.stats.skipped_blocks, skipped);
        }
        assert!(matches!(recv.try_recv(), Err(mpsc::TryRecvError::Empty)));
        let start = first + (skipped + 4) * 1024;
        let mut input = packet(1, 0, vec![0.5; 1024]);
        input.first_sample_media_ns =
            (u128::from(start) * 1_000_000_000 / u128::from(rate)) as i128;
        input.block.host_received_ns =
            (u128::from(start + 1024) * 1_000_000_000 / u128::from(rate)) as u64;
        input.block.format.sample_rate = rate;
        live.ingest(&input);
        assert_eq!(live.stats().input_packets, 1);
        assert_eq!(live.stats().input_errors, 0);
        live.tick(live.next_deadline().unwrap());
        let output = recv.try_recv().unwrap();
        assert_eq!(output.block.start_frame, start);
        assert_eq!(output.block.samples, [0.5; 1024]);
        assert!(output.block.gaps.is_empty());
        assert_eq!(output.stats.skipped_blocks, skipped);
        assert_eq!(output.stats.output_blocks, 5);
    }
}

#[test]
fn forty_five_overdue_blocks_keep_normal_catchup_but_forty_six_trigger_explicit_skip() {
    for (due, skipped) in [(45, 0), (46, 42)] {
        let mut session = fixed_session();
        settings(&mut session, 48000, SpeakerLayout::Mono);
        let (mut live, recv, anchor) = owner(&session, 0, 8);
        let now = finish_time(anchor, 0, due, 48000, 0);
        live.tick(now);
        assert_eq!(live.stats().skipped_blocks, skipped);
        assert_eq!(live.stats().output_blocks, 4);
        assert_eq!(live.needs_immediate(), due == 45);
        assert_eq!(recv.try_recv().unwrap().block.start_frame, skipped * 1024);
    }
}

#[test]
fn exhausted_suspension_counters_stop_owner_without_unbounded_rendering() {
    let session = fixed_session();
    let (mut live, recv, anchor) = owner(&session, 0, 8);
    live.stats.skipped_blocks = u64::MAX;
    live.tick(anchor + Duration::from_secs(3600));
    assert!(!live.is_active());
    assert!(!live.stats().running);
    assert_eq!(live.stats().output_blocks, 0);
    assert_eq!(live.stats().skipped_frames, 0);
    assert_eq!(live.stats().skipped_blocks, u64::MAX);
    assert_eq!(live.stats().problem.unwrap().code, "audio_capture_limit");
    assert!(matches!(
        recv.try_recv(),
        Err(mpsc::TryRecvError::Disconnected)
    ));
}

#[test]
fn full_sink_drops_whole_blocks_without_blocking_input_and_next_output_reports_exact_loss() {
    let mut session = fixed_session();
    settings(&mut session, 48000, SpeakerLayout::Mono);
    add(&mut session, "Input");
    let (mut live, recv, anchor) = owner(&session, 0, 1);
    live.ingest(&packet(1, 0, vec![0.25; 4096]));
    let late = finish_time(anchor, 0, 12, 48000, 0);
    live.tick(late);
    assert_eq!(
        (
            live.stats().output_blocks,
            live.stats().dropped_blocks,
            live.stats().dropped_frames
        ),
        (4, 3, 3072)
    );
    assert_eq!(live.resident_count(), 1);
    let first = recv.try_recv().unwrap();
    assert_eq!(first.block.start_frame, 0);
    assert_eq!(first.stats.dropped_blocks, 0);
    live.ingest(&packet(2, 4096, vec![0.5; 4096]));
    assert_eq!(live.stats().input_packets, 2);
    drop(first);
    live.tick(late);
    let next = recv.try_recv().unwrap();
    assert_eq!(next.block.start_frame, 4096);
    assert_eq!(next.stats.output_blocks, 5);
    assert_eq!(next.stats.dropped_blocks, 3);
    assert_eq!(next.stats.dropped_frames, 3072);
    assert!(next.block.samples.iter().all(|sample| *sample == 0.5));
    assert_eq!(live.stats().dropped_blocks, 6);
}

#[test]
fn pinned_reader_pool_has_count_backpressure_and_released_readers_allow_recovery() {
    let mut session = fixed_session();
    settings(&mut session, 48000, SpeakerLayout::Mono);
    let (mut live, recv, anchor) = owner(&session, 0, 8);
    let mut readers = Vec::new();
    for turn in 0..64 {
        live.tick(finish_time(anchor, 0, (turn + 1) * 4, 48000, 0));
        while let Ok(output) = recv.try_recv() {
            readers.push(output.block);
        }
    }
    assert_eq!(readers.len(), 256);
    assert_eq!(live.resident_count(), 256);
    assert!(live.resident_bytes() <= 8 * 1024 * 1024);
    live.tick(finish_time(anchor, 0, 260, 48000, 0));
    assert!(matches!(recv.try_recv(), Err(mpsc::TryRecvError::Empty)));
    assert_eq!(live.stats().dropped_blocks, 4);
    assert_eq!(live.stats().dropped_frames, 4096);
    readers.pop();
    live.tick(finish_time(anchor, 0, 264, 48000, 0));
    let recovered = recv.try_recv().unwrap();
    assert_eq!(recovered.block.start_frame, 260 * 1024);
    assert_eq!(recovered.stats.dropped_blocks, 4);
    assert_eq!(live.resident_count(), 256);
    live.retire();
    assert!(!live.is_active());
    assert!(!live.stats().running);
    assert!(
        live.program.is_none(),
        "retirement must release converters and their128MiB budget immediately"
    );
    assert!(!live.can_retire());
    drop(readers);
    drop(recovered);
    assert!(live.can_retire());
    assert_eq!(live.resident_bytes(), 0);
    assert_eq!(live.resident_count(), 0);
}

#[test]
fn eight_channel_pcm_hits_byte_ceiling_before_count_ceiling() {
    let mut session = fixed_session();
    settings(&mut session, 48000, SpeakerLayout::SevenPointOne);
    let (mut live, recv, anchor) = owner(&session, 0, 8);
    let mut readers = Vec::new();
    for turn in 0..64 {
        live.tick(finish_time(anchor, 0, (turn + 1) * 4, 48000, 0));
        while let Ok(output) = recv.try_recv() {
            readers.push(output.block);
        }
    }
    assert!(live.resident_count() < 256);
    assert!(live.stats().dropped_blocks > 0);
    assert!(live.resident_bytes() <= 8 * 1024 * 1024);
    assert!(readers.iter().all(|block| block.samples.len() == 8192));
    drop(readers);
    assert!(!live.reap());
    assert_eq!(live.resident_bytes(), 0);
}

#[test]
fn receiver_disconnect_stops_dsp_and_idle_ticks_do_not_spin_or_allocate_more_blocks() {
    let session = fixed_session();
    let (mut live, recv, _) = owner(&session, 0, 8);
    drop(recv);
    let deadline = live.next_deadline().unwrap();
    live.tick(deadline);
    assert!(!live.is_active());
    assert!(!live.stats().running);
    assert!(!live.needs_immediate());
    assert_eq!(live.next_deadline(), None);
    assert_eq!(live.stats().output_blocks, 1);
    assert_eq!(live.stats().dropped_blocks, 1);
    assert_eq!(
        live.stats().problem.unwrap().code,
        "audio_capture_disconnected"
    );
    assert!(live.can_retire());
    live.tick(deadline + Duration::from_secs(3600));
    assert_eq!(live.stats().output_blocks, 1);
}

#[test]
fn route_foreign_identity_format_or_backwards_revision_is_rejected_without_replacing_live_owner() {
    let mut session = fixed_session();
    settings(&mut session, 48000, SpeakerLayout::Mono);
    let (mut live, recv, _) = owner(&session, 0, 8);
    let key = live.key();
    let stale = route(&session, 0);
    session
        .apply(Command::RenameCollection {
            name: "New route revision".into(),
        })
        .unwrap();
    live.update_route(route(&session, 0)).unwrap();
    assert!(live.update_route(stale).is_err());
    let foreign = fixed_session();
    assert!(live.update_route(route(&foreign, 0)).is_err());
    session
        .apply(Command::SetAudioSettings {
            settings: AudioSettings::new(44100, SpeakerLayout::Mono).unwrap(),
        })
        .unwrap();
    assert!(live.update_route(route(&session, 0)).is_err());
    assert_eq!(live.key(), key);
    live.tick(live.next_deadline().unwrap());
    let output = recv.try_recv().unwrap();
    assert_eq!(output.key, key);
    assert_eq!(output.block.settings.sample_rate(), 48000);
    assert!(live.update_route(route(&session, 0)).is_err());
}

#[test]
fn stopped_and_restarted_same_session_clock_and_format_still_have_different_output_keys() {
    let session = Session::new();
    let anchor = Instant::now();
    let (send, recv) = mpsc::sync_channel(8);
    let mut old = LiveProgram::new(route(&session, 0), 9, 1, anchor, send.clone()).unwrap();
    old.tick(old.next_deadline().unwrap());
    old.retire();
    let mut new = LiveProgram::new(route(&session, 0), 9, 2, anchor, send).unwrap();
    new.tick(new.next_deadline().unwrap());
    let old_output = recv.try_recv().unwrap();
    let new_output = recv.try_recv().unwrap();
    assert_eq!(old_output.key.session, new_output.key.session);
    assert_eq!(old_output.key.clock_epoch, new_output.key.clock_epoch);
    assert_ne!(old_output.key, new_output.key);
    assert_eq!(old_output.key.generation, 1);
    assert_eq!(new_output.key.generation, 2);
    assert!(!old.can_retire());
    drop(old_output);
    assert!(old.can_retire());
}

#[test]
fn invalid_coalesced_and_replayed_input_are_reported_without_poisoning_next_valid_packet() {
    let mut session = fixed_session();
    settings(&mut session, 48000, SpeakerLayout::Mono);
    add(&mut session, "Input");
    let (mut live, recv, _) = owner(&session, 0, 8);
    let mut bad = packet(1, 0, vec![0.75; 1024]);
    bad.coalesced_packets = 1;
    live.ingest(&bad);
    assert_eq!(live.stats().input_errors, 1);
    assert_eq!(live.stats().input_packets, 0);
    let good = packet(1, 0, vec![0.25; 1024]);
    live.ingest(&good);
    live.ingest(&good);
    assert_eq!(live.stats().input_errors, 2);
    assert_eq!(live.stats().input_packets, 1);
    live.tick(live.next_deadline().unwrap());
    let output = recv.try_recv().unwrap();
    assert_eq!(output.block.samples, [0.25; 1024]);
    assert_eq!(output.stats.problem.unwrap().code, "program_audio_invalid");
    live.problem("fixture", "Ж".repeat(8192));
    assert!(live.stats().problem.unwrap().detail.len() <= 8192);
}

#[test]
fn first_deadline_overflow_and_zero_freshness_identity_reject_before_scheduling() {
    let session = fixed_session();
    let anchor = Instant::now();
    let (send, _recv) = mpsc::sync_channel(1);
    assert!(LiveProgram::new(route(&session, 0), 0, 1, anchor, send.clone()).is_err());
    assert!(LiveProgram::new(route(&session, 0), 1, 0, anchor, send.clone()).is_err());
    // This timestamp itself is representable but adding the fixed buffering
    // and complete first block is not. No unusable owner may be published.
    let near_end = u64::MAX - 1_000_000;
    let snapshot = route(&session, near_end);
    assert!(matches!(
        LiveProgram::new(snapshot, 1, 1, anchor, send),
        Err(AudioCaptureError::TimeOverflow)
    ));
}

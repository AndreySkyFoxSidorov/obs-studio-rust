use super::*;
use crate::audio::ChannelMeter;
use skylive_core::{AudioBuffering, SpeakerLayout};
use std::sync::atomic::{AtomicUsize, Ordering};
fn settings() -> AudioSettings {
    AudioSettings::new(48000, SpeakerLayout::Stereo).unwrap()
}
fn output(id: u64) -> OutputKey {
    OutputKey {
        kind: OutputKind::Streaming,
        session: 1,
        id,
        epoch: 1,
    }
}
fn program() -> ProgramCaptureKey {
    ProgramCaptureKey {
        session: 1,
        clock_epoch: 1,
        generation: 1,
        settings: settings(),
    }
}
fn block(start: u64, frames: usize) -> Arc<ProgramAudioBlock> {
    Arc::new(ProgramAudioBlock {
        tracks:Vec::new(),
        instance: 1,
        epoch: 1,
        session: 1,
        data_generation: 1,
        routing_serial: 1,
        settings: settings(),
        start_frame: start,
        frames,
        samples: vec![0.125; frames * 2],
        program_meters: vec![ChannelMeter::default(); 2],
        source_meters: Vec::new(),
        source_meter_revision: Some(1),
        clipped_samples: vec![0; 2],
        gaps: Vec::new(),
    })
}
fn channel(
    origin: Option<MediaTime>,
) -> (
    StreamingAudioSink,
    StreamingAudioReceiver,
    StreamingAudioControl,
) {
    channel_for_test(output(1), settings(), origin, Arc::new(AtomicUsize::new(0))).unwrap()
}
#[test]
fn exact_shared_anchor_retains_fractional_sample_relation_and_trim_without_pcm_copy() {
    let (sink, mut receiver, control) = channel(Some(MediaTime(1)));
    let mut router = ProgramOutputRouter::default();
    router
        .attach(program(), AudioOutput::Streaming(sink))
        .unwrap();
    let source = block(0, 1024);
    let identity = Arc::as_ptr(&source);
    router.submit(program(), source);
    let delivery = receiver.poll().unwrap();
    assert_eq!(delivery.allocation(), identity);
    assert_eq!(delivery.absolute_first_frame(), 1);
    assert_eq!(delivery.offset_frames(), 1);
    assert_eq!(delivery.frames(), 1023);
    assert_eq!(delivery.samples(), vec![0.125; 2046]);
    assert_eq!(delivery.anchor().media_origin_ns(), 1);
    assert_eq!(delivery.anchor().first_sample_frame(), 1);
    assert_eq!(
        delivery.first_pts_100ns().unwrap(),
        208,
        "do not reset independently to audio zero"
    );
    control.request_stop();
    router.prune();
    assert!(receiver.producer_ended());
    assert!(!receiver.drained(), "delivery still owns its permit");
    drop(delivery);
    assert!(receiver.drained());
}
#[test]
fn pending_preroll_evicts_to_two_then_flushes_on_activation_without_new_pcm() {
    let (sink, mut receiver, control) = channel(None);
    let mut router = ProgramOutputRouter::default();
    router
        .attach(program(), AudioOutput::Streaming(sink))
        .unwrap();
    for n in 0..5 {
        router.submit(program(), block(n * 1024, 1024));
    }
    assert_eq!(control.stats().queued_blocks, 2);
    assert!(receiver.poll().is_none());
    assert!(
        control
            .activate(
                OutputKey {
                    epoch: 2,
                    ..output(1)
                },
                MediaTime(0)
            )
            .is_err()
    );
    assert!(
        control.stats().fault.is_none(),
        "stale callback cannot fault current output"
    );
    let origin = MediaTime((3u128 * 1024 * 1_000_000_000 / 48000) as u64);
    let anchor = control.activate(output(1), origin).unwrap();
    assert_eq!(anchor.first_sample_frame(), 3072);
    assert_eq!(control.activate(output(1), origin).unwrap(), anchor);
    assert!(
        control
            .activate(output(1), MediaTime(origin.0 + 1))
            .is_err()
    );
    router.prune();
    for expected in [3072, 4096] {
        let delivery = receiver.poll().unwrap();
        assert_eq!(delivery.absolute_first_frame(), expected);
        assert_eq!(delivery.gap_before_frames(), 0);
    }
    assert!(receiver.poll().is_none());
    assert_eq!(control.stats().queued_blocks, 0);
    control.request_stop();
    router.prune();
    assert!(receiver.drained());
}
#[test]
fn count_backpressure_faults_only_stream_slot_and_retains_accepted_prefix() {
    let (sink, mut receiver, control) = channel(Some(MediaTime(0)));
    let mut router = ProgramOutputRouter::default();
    router
        .attach(program(), AudioOutput::Streaming(sink))
        .unwrap();
    for n in 0..40 {
        router.submit(program(), block(n * 1024, 1024));
    }
    assert!(router.has(OutputKind::Streaming));
    assert_eq!(control.stats().queued_blocks, 40);
    router.submit(program(), block(40 * 1024, 1024));
    assert!(!router.has(OutputKind::Streaming));
    assert_eq!(control.stats().fault, Some(OutputFault::Backpressure));
    for n in 0..40 {
        let delivery = receiver.poll().unwrap();
        assert_eq!(delivery.absolute_first_frame(), n * 1024);
    }
    assert!(receiver.drained());
    assert_eq!(control.stats().charged_bytes, 0);
}
#[test]
fn finite_small_gap_is_metadata_only_and_large_gap_fails_without_unbounded_fill() {
    let (mut sink, mut receiver, control) = channel(Some(MediaTime(0)));
    assert!(sink.submit(program(), block(2400, 1024)));
    let delivery = receiver.poll().unwrap();
    assert_eq!(delivery.gap_before_frames(), 2400);
    assert_eq!(
        delivery.samples().len(),
        2048,
        "no silence expansion in bridge"
    );
    drop(delivery);
    assert!(!sink.submit(program(), block(12_000, 1024)));
    assert_eq!(control.stats().fault, Some(OutputFault::Gap));
    drop(sink);
    assert!(receiver.drained());
}
#[test]
fn duplicate_and_stale_output_keys_do_not_replace_current_generation() {
    let pool = Arc::new(AtomicUsize::new(0));
    let (old, old_receiver, old_control) =
        channel_for_test(output(1), settings(), Some(MediaTime(0)), pool.clone()).unwrap();
    let mut router = ProgramOutputRouter::default();
    router
        .attach(program(), AudioOutput::Streaming(old))
        .unwrap();
    let (duplicate, duplicate_receiver, duplicate_control) =
        channel_for_test(output(2), settings(), Some(MediaTime(0)), pool.clone()).unwrap();
    assert!(matches!(
        router.attach(program(), AudioOutput::Streaming(duplicate)),
        Err(OutputError::DuplicateKind(OutputKind::Streaming))
    ));
    assert!(router.has(OutputKind::Streaming));
    drop(duplicate_receiver);
    drop(duplicate_control);
    old_control.request_stop();
    router.prune();
    assert!(old_receiver.drained());
    drop(old_receiver);
    drop(old_control);
    let fresh_key = OutputKey {
        epoch: 2,
        ..output(1)
    };
    let (fresh, mut receiver, control) =
        channel_for_test(fresh_key, settings(), Some(MediaTime(0)), pool).unwrap();
    router
        .attach(program(), AudioOutput::Streaming(fresh))
        .unwrap();
    assert!(
        !router.detach(output(1)),
        "obsolete end completion cannot detach a restarted stream"
    );
    router.submit(program(), block(0, 1024));
    assert!(receiver.poll().is_some());
    control.request_stop();
    router.prune();
}
#[test]
fn format_clock_order_nonfinite_and_byte_capacity_guards_are_terminal() {
    for mode in 0..5 {
        let (mut sink, _receiver, control) = channel(Some(MediaTime(0)));
        assert!(sink.submit(program(), block(0, 1024)));
        let mut key = program();
        let mut candidate = block(1024, 1024);
        match mode {
            0 => key.clock_epoch += 1,
            1 => key.settings = AudioSettings::new(44100, SpeakerLayout::Stereo).unwrap(),
            2 => candidate = block(0, 1024),
            3 => Arc::get_mut(&mut candidate).unwrap().samples[0] = f32::NAN,
            _ => {
                let block = Arc::get_mut(&mut candidate).unwrap();
                block.samples.reserve(4096 * 8);
            }
        }
        assert!(!sink.submit(key, candidate));
        assert!(control.stats().fault.is_some());
    }
}
#[test]
fn buffering_only_owner_migration_keeps_sample_order_and_retains_accepted_old_key() {
    let (sink, mut receiver, control) = channel(Some(MediaTime(0)));
    let mut old = ProgramOutputRouter::default();
    old.attach(program(), AudioOutput::Streaming(sink)).unwrap();
    old.submit(program(), block(0, 1024));
    let mut next = program();
    next.generation += 1;
    next.settings = next
        .settings
        .with_buffering(AudioBuffering::low_latency())
        .unwrap();
    let mut candidate = ProgramOutputRouter::default();
    old.migrate_to(&mut candidate, next);
    assert!(!old.has(OutputKind::Streaming));
    assert!(candidate.has(OutputKind::Streaming));
    let mut next_block = Arc::try_unwrap(block(1024, 1024)).unwrap();
    next_block.settings = next.settings;
    next_block.instance = 2;
    candidate.submit(next, Arc::new(next_block));
    let first = receiver.poll().unwrap();
    let second = receiver.poll().unwrap();
    assert_eq!(first.program_key(), program());
    assert_eq!(second.program_key(), next);
    assert_eq!(second.absolute_first_frame(), 1024);
    drop(first);
    drop(second);
    control.request_stop();
    candidate.prune();
    assert!(receiver.drained());
}
#[test]
fn global_ingress_lease_counts_queued_deliveries_after_stop_and_receiver_retirement() {
    let pool = Arc::new(AtomicUsize::new(0));
    let (mut a, mut ar, ac) =
        channel_for_test(output(1), settings(), Some(MediaTime(0)), pool.clone()).unwrap();
    let (b, br, bc) = channel_for_test(output(2), settings(), None, pool.clone()).unwrap();
    assert!(channel_for_test(output(3), settings(), None, pool.clone()).is_err());
    assert!(a.submit(program(), block(0, 1024)));
    let delivery = ar.poll().unwrap();
    ac.request_stop();
    drop(a);
    drop(ar);
    drop(ac);
    assert_eq!(
        pool.load(Ordering::Acquire),
        2,
        "outstanding delivery retains its ingress lease"
    );
    drop(delivery);
    assert_eq!(pool.load(Ordering::Acquire), 1);
    let third = channel_for_test(output(3), settings(), None, pool.clone()).unwrap();
    drop(third);
    bc.request_stop();
    drop(b);
    drop(br);
    drop(bc);
    assert_eq!(pool.load(Ordering::Acquire), 0);
}

#[test]
fn byte_limit_is_independent_of_count_and_releases_with_delivery_permits() {
    let layout = AudioSettings::new(48000, SpeakerLayout::SevenPointOne).unwrap();
    let (mut sink, mut receiver, control) = channel_for_test(
        output(1),
        layout,
        Some(MediaTime(0)),
        Arc::new(AtomicUsize::new(0)),
    )
    .unwrap();
    let key = ProgramCaptureKey {
        settings: layout,
        ..program()
    };
    let mut accepted = 0;
    for n in 0..40u64 {
        let mut next = Arc::try_unwrap(block(n * 4096, 4096)).unwrap();
        next.settings = layout;
        next.samples = vec![0.125; 4096 * 8];
        next.program_meters = vec![ChannelMeter::default(); 8];
        next.clipped_samples = vec![0; 8];
        if !sink.submit(key, Arc::new(next)) {
            break;
        }
        accepted += 1;
    }
    assert!(
        accepted < 40,
        "5 MiB byte cap rejects before the count cap for large eight-channel blocks"
    );
    assert_eq!(control.stats().fault, Some(OutputFault::Backpressure));
    assert!(control.stats().charged_bytes <= streaming::MAX_STREAM_BYTES);
    drop(sink);
    for _ in 0..accepted {
        drop(receiver.poll().unwrap());
    }
    assert!(receiver.drained());
    assert_eq!(control.stats().charged_bytes, 0);
}

//! Independent wall-callback traces through the complete native-live CPU path.
//! Synthetic PCM and Instant arithmetic only; no backend or native API exists.
use super::*;
use crate::audio_capture::{
    AudioTimingOrigin, NativeAudioFormat, NativePacketFlags, NativePcmBlock,
};
use crate::transition::{FrameComposition, FrameScene};
use skylive_core::{
    AudioBuffering, AudioCaptureBinding, AudioFlow, AudioTarget, Command, Id, Session,
    SpeakerLayout,
};

fn binding(name: &str) -> AudioCaptureBinding {
    AudioCaptureBinding::new(
        AudioFlow::Input,
        AudioTarget::WindowsEndpoint { id: name.into() },
        false,
        false,
    )
    .unwrap()
}
fn setup(
    rate: u32,
    policy: AudioBuffering,
    names: &[&str],
) -> (
    LiveProgram,
    mpsc::Receiver<ProgramCaptureOutput>,
    Instant,
    Vec<(AudioCaptureBinding, Id)>,
) {
    setup_at(rate, policy, names, 0)
}
fn setup_at(
    rate: u32,
    policy: AudioBuffering,
    names: &[&str],
    origin: u64,
) -> (
    LiveProgram,
    mpsc::Receiver<ProgramCaptureOutput>,
    Instant,
    Vec<(AudioCaptureBinding, Id)>,
) {
    let mut session = Session::new();
    session
        .apply(Command::SetAudioSettings {
            settings: AudioSettings::new(rate, SpeakerLayout::Mono)
                .unwrap()
                .with_buffering(policy)
                .unwrap(),
        })
        .unwrap();
    let scene = session.preview();
    let mut inputs = Vec::new();
    for name in names {
        let input = binding(name);
        let item = session
            .apply(Command::AddAudioCaptureSource {
                scene,
                name: format!("{name} audio"),
                binding: input.clone(),
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
            .find(|row| row.id() == item)
            .unwrap()
            .source();
        inputs.push((input, source));
    }
    let route = AudioRouteSnapshot::capture(
        &session,
        &FrameComposition::Scene(FrameScene::Live(scene)),
        MediaTime(origin),
    )
    .unwrap();
    let anchor = Instant::now();
    let (send, recv) = mpsc::sync_channel(8);
    let mut live = LiveProgram::new(route, 1, 1, anchor, send).unwrap();
    live.set_capture_bindings(inputs.iter().map(|row| row.0.clone()).collect())
        .unwrap();
    (live, recv, anchor, inputs)
}
fn wall(anchor: Instant, tick: u64, rate: u32) -> Instant {
    anchor
        + Duration::from_nanos((u128::from(tick) * 1024 * 1_000_000_000 / u128::from(rate)) as u64)
}
fn feed(
    live: &mut LiveProgram,
    input: &AudioCaptureBinding,
    sequence: u64,
    first: i128,
    frames: usize,
    value: f32,
) -> u64 {
    let rate = live.key().settings.sample_rate();
    let mut offset = 0;
    let mut sequence = sequence;
    while offset < frames {
        let count = (frames - offset).min(4096);
        let start = first + offset as i128;
        let end = start + count as i128;
        let first_ns = (start * 1_000_000_000).div_euclid(i128::from(rate));
        let received_ns = (end * 1_000_000_000).div_euclid(i128::from(rate));
        live.ingest(&CapturedAudioPacket {
            resolved_device: None,
            binding: input.clone(),
            sequence,
            requested_at_ns: 0,
            stream_generation: 1,
            first_sample_media_ns: first_ns,
            timing_origin: AudioTimingOrigin::ReceiveEstimate,
            coalesced_packets: 0,
            meters: Vec::new(),
            block: NativePcmBlock {
                format: NativeAudioFormat {
                    sample_rate: rate,
                    channels: 1,
                    channel_mask: 4,
                },
                frames: count,
                samples: vec![value; count],
                device_timestamp_ns: None,
                graph_timestamp_ns: None,
                directshow_clock: None,
                host_received_ns: u64::try_from(1_000_000_000 + received_ns).unwrap(),
                device_frame_position: None,
                flags: NativePacketFlags::default(),
            },
        });
        offset += count;
        sequence += 1;
    }
    assert_eq!(live.stats().input_errors, 0, "{:?}", live.stats().problem);
    sequence - 1
}
fn tick(live: &mut LiveProgram, anchor: Instant, index: u64) {
    live.tick(wall(anchor, index, live.key().settings.sample_rate()));
    assert!(live.is_active(), "{:?}", live.stats().problem);
    assert_eq!(live.stats().wall_ticks, index);
    assert!(!live.needs_immediate());
}
fn absent(recv: &mpsc::Receiver<ProgramCaptureOutput>) {
    assert!(
        matches!(recv.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "buffering must suppress output, not publish zero/repeated PCM"
    );
}
fn take(
    recv: &mpsc::Receiver<ProgramCaptureOutput>,
    start: u64,
    value: f32,
) -> ProgramCaptureOutput {
    let output = recv.try_recv().expect("expected one emitted media block");
    assert_eq!(output.block.start_frame, start);
    assert_eq!(output.block.frames, 1024);
    assert_eq!(output.block.samples.len(), 1024);
    assert!(
        output.block.samples.iter().all(|sample| *sample == value),
        "block {start} differs from literal PCM oracle"
    );
    output
}

#[test]
fn adaptive_default_healthy_input_emits_on_first_wall_tick_without_extra_latency() {
    for rate in [44100, 48000] {
        let (mut live, recv, anchor, inputs) = setup(rate, AudioBuffering::default(), &["A"]);
        feed(&mut live, &inputs[0].0, 1, 0, 4096, 0.125);
        assert_eq!(live.stats().buffer_blocks, 0);
        assert_eq!(live.stats().max_buffer_blocks, 45);
        live.tick(wall(anchor, 1, rate) - Duration::from_nanos(1));
        absent(&recv);
        for index in 1..=3 {
            tick(&mut live, anchor, index);
            let output = take(&recv, (index - 1) * 1024, 0.125);
            assert!(output.block.gaps.is_empty());
            assert_eq!(output.stats.output_blocks, index);
            assert_eq!(output.stats.suppressed_blocks, 0);
        }
        assert_eq!(live.stats().buffering_growths, 0);
        assert_eq!(live.stats().dropped_blocks, 0);
        assert_eq!(live.stats().skipped_blocks, 0);
    }
}

#[test]
fn unseen_input_remains_pending_without_stalling_healthy_source_or_growing_buffer() {
    let (mut live, recv, anchor, inputs) = setup(48000, AudioBuffering::default(), &["A", "B"]);
    feed(&mut live, &inputs[0].0, 1, 0, 4096, 0.125);
    for index in 1..=3 {
        tick(&mut live, anchor, index);
        let output = take(&recv, (index - 1) * 1024, 0.125);
        assert_eq!(output.stats.pending_inputs, 1);
        assert_eq!(output.block.gaps.len(), 1);
        assert_eq!(output.block.gaps[0].source, inputs[1].1);
        assert_eq!(output.block.gaps[0].missing_frames, 1024);
    }
    assert_eq!(live.stats().buffer_blocks, 0);
    assert_eq!(live.stats().suppressed_blocks, 0);
}

#[test]
fn delayed_ready_source_adds_one_discard_only_wait_then_resumes_at_original_next_cursor() {
    let (mut live, recv, anchor, inputs) = setup(48000, AudioBuffering::default(), &["A", "B"]);
    feed(&mut live, &inputs[0].0, 1, 0, 8192, 0.125);
    for index in 1..=2 {
        tick(&mut live, anchor, index);
        take(&recv, (index - 1) * 1024, 0.125);
    }
    feed(&mut live, &inputs[1].0, 1, 1024, 4096, 0.25);
    tick(&mut live, anchor, 3);
    absent(&recv);
    assert_eq!(live.stats().buffer_blocks, 1);
    assert_eq!(live.stats().waiting_blocks, 0);
    assert_eq!(live.stats().suppressed_blocks, 1);
    assert_eq!(live.stats().output_blocks, 2);
    assert_eq!(live.stats().buffering_growths, 1);
    tick(&mut live, anchor, 4);
    let output = take(&recv, 2048, 0.375);
    assert!(output.block.gaps.is_empty());
    assert_eq!(output.stats.pending_inputs, 0);
    tick(&mut live, anchor, 5);
    take(&recv, 3072, 0.375);
    assert_eq!(live.stats().suppressed_blocks, 1);
    assert_eq!(live.stats().buffer_blocks, 1);
    assert_eq!(live.stats().output_blocks, 4);
}

#[test]
fn negative_startup_head_is_discarded_during_two_waits_and_first_output_is_cursor_zero() {
    let (mut live, recv, anchor, inputs) = setup(48000, AudioBuffering::default(), &["A"]);
    feed(&mut live, &inputs[0].0, 1, -2048, 4096, 0.25);
    for index in 1..=2 {
        tick(&mut live, anchor, index);
        absent(&recv);
        assert_eq!(live.stats().output_blocks, 0);
    }
    assert_eq!(live.stats().buffer_blocks, 2);
    assert_eq!(live.stats().suppressed_blocks, 2);
    assert_eq!(live.stats().discarded_source_frames, 2048);
    assert_eq!(live.stats().waiting_blocks, 0);
    tick(&mut live, anchor, 3);
    let output = take(&recv, 0, 0.25);
    assert!(output.block.gaps.is_empty());
    assert_eq!(output.stats.buffering_growths, 1);
    assert_eq!(output.stats.late_frames, 0);
}

#[test]
fn configured_one_tick_cap_drops_exact_late_prefix_and_preserves_recovered_source_suffix() {
    let (mut live, recv, anchor, inputs) =
        setup(48000, AudioBuffering::new(1, false).unwrap(), &["A", "B"]);
    feed(&mut live, &inputs[0].0, 1, 0, 8192, 0.125);
    for index in 1..=2 {
        tick(&mut live, anchor, index);
        take(&recv, (index - 1) * 1024, 0.125);
    }
    feed(&mut live, &inputs[1].0, 1, -5120, 8192, 0.25);
    tick(&mut live, anchor, 3);
    absent(&recv);
    assert_eq!(live.stats().buffer_blocks, 1);
    assert_eq!(live.stats().max_buffer_blocks, 1);
    tick(&mut live, anchor, 4);
    let output = take(&recv, 2048, 0.375);
    assert!(output.block.gaps.is_empty());
    assert_eq!(output.stats.late_frames, 7168);
    assert_eq!(output.stats.reset_inputs, 0);
    assert_eq!(output.stats.suppressed_blocks, 1);
    assert_eq!(output.stats.discarded_source_frames, 11264);
}

#[test]
fn unrecoverable_late_source_resets_once_and_next_sequence_on_same_binding_recovers() {
    let (mut live, recv, anchor, inputs) =
        setup(48000, AudioBuffering::new(1, false).unwrap(), &["A", "B"]);
    feed(&mut live, &inputs[0].0, 1, 0, 8192, 0.125);
    for index in 1..=2 {
        tick(&mut live, anchor, index);
        take(&recv, (index - 1) * 1024, 0.125);
    }
    feed(&mut live, &inputs[1].0, 1, -5120, 4096, 0.25);
    tick(&mut live, anchor, 3);
    absent(&recv);
    tick(&mut live, anchor, 4);
    let output = take(&recv, 2048, 0.125);
    assert_eq!(output.stats.reset_inputs, 1);
    assert_eq!(output.stats.late_frames, 4096);
    assert_eq!(output.stats.pending_inputs, 1);
    feed(&mut live, &inputs[1].0, 2, 3072, 2048, 0.25);
    tick(&mut live, anchor, 5);
    let recovered = take(&recv, 3072, 0.375);
    assert!(recovered.block.gaps.is_empty());
    assert_eq!(recovered.stats.reset_inputs, 1);
    assert_eq!(recovered.stats.pending_inputs, 0);
}

#[test]
fn unchanged_four_frame_pending_source_is_cleared_on_third_tick_while_healthy_audio_continues() {
    let (mut live, recv, anchor, inputs) = setup(48000, AudioBuffering::default(), &["A", "B"]);
    feed(&mut live, &inputs[0].0, 1, 0, 4096, 0.125);
    feed(&mut live, &inputs[1].0, 1, 0, 4, 0.25);
    for index in 1..=3 {
        tick(&mut live, anchor, index);
        let output = take(&recv, (index - 1) * 1024, 0.125);
        assert_eq!(output.stats.pending_inputs, 1);
        assert_eq!(output.stats.stopped_inputs, u64::from(index == 3));
    }
    assert_eq!(live.stats().discarded_source_frames, 3076);
    assert_eq!(live.stats().buffer_blocks, 0);
    assert_eq!(live.stats().suppressed_blocks, 0);
    assert_eq!(live.stats().reset_inputs, 0);
}

#[test]
fn low_latency_fixed_twenty_ms_suppresses_first_wall_callback_then_emits_cursor_zero() {
    let (mut live, recv, anchor, inputs) = setup(48000, AudioBuffering::low_latency(), &["A"]);
    feed(&mut live, &inputs[0].0, 1, 0, 4096, 0.125);
    assert_eq!(live.stats().buffer_blocks, 1);
    tick(&mut live, anchor, 1);
    absent(&recv);
    assert_eq!(live.stats().suppressed_blocks, 1);
    assert_eq!(live.stats().buffering_growths, 0);
    tick(&mut live, anchor, 2);
    let output = take(&recv, 0, 0.125);
    assert!(output.block.gaps.is_empty());
    assert_eq!(output.stats.output_blocks, 1);
    assert_eq!(output.stats.buffer_blocks, 1);
}

#[test]
fn warm_capture_binding_influences_readiness_but_never_enters_program_mix() {
    let (mut live, recv, anchor, inputs) = setup(48000, AudioBuffering::default(), &["A"]);
    let warm = binding("Preview-only");
    live.set_capture_bindings(vec![inputs[0].0.clone(), warm.clone()])
        .unwrap();
    feed(&mut live, &inputs[0].0, 1, 0, 4096, 0.125);
    feed(&mut live, &warm, 1, -1024, 4096, 0.75);
    tick(&mut live, anchor, 1);
    absent(&recv);
    assert_eq!(live.stats().buffer_blocks, 1);
    tick(&mut live, anchor, 2);
    let output = take(&recv, 0, 0.125);
    assert!(output.block.gaps.is_empty());
    assert_eq!(output.block.source_meters.len(), 1);
    assert_eq!(output.block.source_meters[0].source, inputs[0].1);
    assert_eq!(output.stats.pending_inputs, 0);
}

#[test]
fn one_nanosecond_origin_normalizes_one_preroll_sample_without_buffer_growth_or_silent_blocks() {
    // These are literal absolute-grid deadline offsets, not multiples of a
    // rounded period: floor((1+n*1024)*1e9/rate) minus the1ns media anchor.
    for (rate, first_ns, second_ns) in [
        (48000, 21_354_165, 42_687_499),
        (44100, 23_242_629, 46_462_584),
    ] {
        let (mut live, recv, anchor, inputs) = setup_at(rate, AudioBuffering::default(), &["A"], 1);
        feed(&mut live, &inputs[0].0, 1, 0, 4096, 0.25);
        let first = anchor + Duration::from_nanos(first_ns);
        assert_eq!(live.next_deadline(), Some(first));
        live.tick(first - Duration::from_nanos(1));
        absent(&recv);
        live.tick(first);
        assert!(live.is_active(), "{:?}", live.stats().problem);
        let output = take(&recv, 1, 0.25);
        assert!(output.block.gaps.is_empty());
        assert_eq!(output.stats.wall_ticks, 1);
        assert_eq!(output.stats.output_blocks, 1);
        assert_eq!(output.stats.late_frames, 1);
        assert_eq!(output.stats.discarded_source_frames, 1025);
        assert_eq!(output.stats.buffer_blocks, 0);
        assert_eq!(output.stats.suppressed_blocks, 0);
        assert_eq!(output.stats.buffering_growths, 0);
        let second = anchor + Duration::from_nanos(second_ns);
        assert_eq!(live.next_deadline(), Some(second));
        live.tick(second);
        let output = take(&recv, 1025, 0.25);
        assert!(output.block.gaps.is_empty());
        assert_eq!(output.stats.late_frames, 1);
        assert_eq!(output.stats.buffer_blocks, 0);
        assert_eq!(output.stats.suppressed_blocks, 0);
        assert_eq!(output.stats.buffering_growths, 0);
    }
}

#[test]
fn ready_source_grid_normalization_does_not_reset_unrelated_pending_input_before_stop_probe() {
    for (rate, deadlines) in [
        (48000, [21_354_165, 42_687_499, 64_020_832]),
        (44100, [23_242_629, 46_462_584, 69_682_538]),
    ] {
        let (mut live, recv, anchor, inputs) =
            setup_at(rate, AudioBuffering::default(), &["A", "B"], 1);
        feed(&mut live, &inputs[0].0, 1, 0, 4096, 0.25);
        feed(&mut live, &inputs[1].0, 1, -5120, 4, 0.75);
        for (index, deadline_ns) in deadlines.into_iter().enumerate() {
            live.tick(anchor + Duration::from_nanos(deadline_ns));
            assert!(live.is_active(), "{:?}", live.stats().problem);
            let output = take(&recv, 1 + index as u64 * 1024, 0.25);
            assert_eq!(output.stats.wall_ticks, index as u64 + 1);
            assert_eq!(output.stats.output_blocks, index as u64 + 1);
            assert_eq!(
                output.stats.late_frames, 1,
                "only the ready source's single preroll sample may be normalized"
            );
            assert_eq!(
                output.stats.reset_inputs, 0,
                "pending B must retain normal pending-stop handling"
            );
            assert_eq!(output.stats.stopped_inputs, u64::from(index == 2));
            assert_eq!(output.stats.pending_inputs, 1);
            assert_eq!(output.stats.buffer_blocks, 0);
            assert_eq!(output.stats.suppressed_blocks, 0);
            assert_eq!(output.stats.buffering_growths, 0);
            assert_eq!(output.block.gaps.len(), 1);
            assert_eq!(output.block.gaps[0].source, inputs[1].1);
            assert_eq!(output.block.gaps[0].missing_frames, 1024);
        }
        assert_eq!(
            live.stats().discarded_source_frames,
            3077,
            "one normalized A sample, three A blocks and four stopped B samples"
        );
        absent(&recv);
    }
}

#[test]
fn suspended_live_source_reports_remaining_pcm_loss_separately_from_skipped_output_time() {
    // One hour gives these literal whole project wall-tick counts.
    // The first callback already ran; leave exactly four callbacks to render.
    for (rate, total_wall, skipped, first_resumed) in [
        (48000, 168750, 168745, 172795904),
        (44100, 155039, 155034, 158755840),
    ] {
        let (mut live, recv, anchor, inputs) = setup(rate, AudioBuffering::default(), &["A"]);
        feed(&mut live, &inputs[0].0, 1, 0, 4096, 0.25);
        tick(&mut live, anchor, 1);
        take(&recv, 0, 0.25);
        let before = live.stats();
        assert_eq!(before.discarded_source_frames, 1024);
        assert_eq!(before.late_frames, 0);
        assert_eq!(before.reset_inputs, 0);
        let resumed = anchor + Duration::from_secs(3600);
        live.tick(resumed);
        assert!(live.is_active(), "{:?}", live.stats().problem);
        let after = live.stats();
        assert_eq!(after.output_blocks - before.output_blocks, 4);
        assert_eq!(after.output_blocks, 5);
        assert_eq!(
            after.discarded_source_frames - before.discarded_source_frames,
            3072
        );
        assert_eq!(after.late_frames - before.late_frames, 3072);
        assert_eq!(after.discarded_source_frames, 4096);
        assert_eq!(after.reset_inputs, 1);
        assert_eq!(after.skipped_wall_ticks, skipped);
        assert_eq!(after.skipped_blocks, skipped);
        assert_eq!(after.skipped_frames, skipped * 1024);
        assert_eq!(after.wall_ticks, total_wall);
        assert_eq!(after.suppressed_blocks, 0);
        assert_eq!(after.dropped_blocks, 0);
        assert!(!live.needs_immediate());
        assert!(live.next_deadline().unwrap() > resumed);
        for index in 0..4 {
            let output = take(&recv, first_resumed + index * 1024, 0.0);
            assert_eq!(output.block.gaps.len(), 1);
            assert_eq!(output.block.gaps[0].missing_frames, 1024);
        }
        absent(&recv);
        let next = first_resumed + 4096;
        feed(&mut live, &inputs[0].0, 2, i128::from(next), 1024, 0.5);
        live.tick(live.next_deadline().unwrap());
        let output = take(&recv, next, 0.5);
        assert!(output.block.gaps.is_empty());
        assert_eq!(output.stats.late_frames, 3072);
        assert_eq!(output.stats.reset_inputs, 1);
    }
}

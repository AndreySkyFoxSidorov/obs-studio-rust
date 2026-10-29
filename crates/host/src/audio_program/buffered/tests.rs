use super::*;
use crate::transition::{FrameComposition, FrameScene};
use skylive_core::{AudioFlow, AudioTarget, Command, SpeakerLayout};

fn binding(name: &str) -> AudioCaptureBinding {
    AudioCaptureBinding::new(
        AudioFlow::Input,
        AudioTarget::WindowsEndpoint { id: name.into() },
        true,
        false,
    )
    .unwrap()
}
fn setup(names: &[&str]) -> (Session, AudioProgram, Vec<AudioCaptureBinding>, Vec<Id>) {
    let mut session = Session::new();
    let scene = session.preview();
    session
        .apply(Command::SetAudioSettings {
            settings: AudioSettings::new(48000, SpeakerLayout::Mono).unwrap(),
        })
        .unwrap();
    let mut bindings = Vec::new();
    let mut sources = Vec::new();
    for name in names {
        let input = binding(name);
        let item = session
            .apply(Command::AddAudioCaptureSource {
                scene,
                name: (*name).into(),
                binding: input.clone(),
            })
            .unwrap()
            .created
            .unwrap();
        sources.push(
            session
                .snapshot()
                .scene(scene)
                .unwrap()
                .items()
                .iter()
                .find(|row| row.id() == item)
                .unwrap()
                .source(),
        );
        bindings.push(input);
    }
    let mut program =
        AudioProgram::new_buffered(session.snapshot().audio(), ProgramAudioLimits::default())
            .unwrap();
    program
        .route(
            &session,
            &FrameComposition::Scene(FrameScene::Live(scene)),
            MediaTime(0),
        )
        .unwrap();
    (session, program, bindings, sources)
}
fn feed(
    program: &mut AudioProgram,
    binding: &AudioCaptureBinding,
    start: i128,
    frames: usize,
    value: f32,
    discontinuity: bool,
) -> AudioIngestReport {
    let ns = start * 1_000_000_000 / 48000;
    program
        .ingest(ProgramInputPacket {
            identity: program.identity().unwrap(),
            binding,
            stream_generation: 1,
            sequence: program.status().data_generation + 1,
            first_media_ns: ns,
            received_media_ns: ns,
            format: NativeAudioFormat {
                sample_rate: 48000,
                channels: 1,
                channel_mask: 4,
            },
            samples: &vec![value; frames],
            flags: NativePacketFlags {
                discontinuity,
                ..Default::default()
            },
        })
        .unwrap()
}

#[test]
fn full_peek_ignores_blocked_source_but_includes_auxiliary_and_zero_timestamp() {
    let (_, mut program, inputs, sources) = setup(&["Healthy", "Blocked"]);
    let auxiliary = binding("Preview only");
    program
        .set_capture_bindings(vec![auxiliary.clone()])
        .unwrap();
    feed(&mut program, &inputs[0], 0, 2048, 0.25, false);
    feed(&mut program, &inputs[1], 0, 512, 0.5, false);
    feed(&mut program, &auxiliary, -1024, 2048, 0.75, false);
    let first = program.buffered_prepare(0).unwrap();
    assert_eq!(first.minimum_frame, Some(-1024));
    assert_eq!(first.pending_inputs, 1);
    assert_eq!(program.status().cursor, None);
    let wait = program.buffered_discard(-1024).unwrap();
    assert_eq!(wait.discarded_frames, 1024);
    let ready = program.buffered_prepare(0).unwrap();
    assert_eq!(ready.minimum_frame, Some(0));
    assert_eq!(ready.pending_inputs, 1);
    let block = program.render(0, 1024).unwrap();
    assert_eq!(block.samples, vec![0.25; 1024]);
    assert_eq!(block.gaps.len(), 1);
    assert_eq!(block.gaps[0].source, sources[1]);
    assert_eq!(block.gaps[0].missing_frames, 1024);
    assert_eq!(
        program.groups[&AudioInputKey::Native(inputs[0].clone())]
            .ring
            .queue_frames(),
        2048,
        "render cannot pop native source FIFO"
    );
    let discarded = program.buffered_discard(0).unwrap();
    assert_eq!(discarded.discarded_frames, 2048);
    assert_eq!(
        program.groups[&AudioInputKey::Native(inputs[0].clone())]
            .ring
            .queue_head(),
        Some(1024)
    );
    assert_eq!(
        program.groups[&AudioInputKey::Native(inputs[1].clone())]
            .ring
            .queue_frames(),
        512
    );
}

#[test]
fn underfull_retime_does_not_pop_and_continuous_data_appends_to_retimed_tail() {
    let (_, mut program, inputs, _) = setup(&["Retime"]);
    let input = &inputs[0];
    feed(&mut program, input, 0, 512, 0.25, false);
    assert_eq!(program.buffered_prepare(0).unwrap().pending_inputs, 1);
    let report = program.buffered_discard(0).unwrap();
    assert_eq!(report.discarded_frames, 0);
    assert_eq!(
        program.groups[&AudioInputKey::Native(input.clone())]
            .ring
            .queue_head(),
        Some(1024)
    );
    assert_eq!(
        program.groups[&AudioInputKey::Native(input.clone())]
            .ring
            .queue_frames(),
        512
    );
    feed(&mut program, input, 512, 512, 0.5, false);
    assert_eq!(
        program.groups[&AudioInputKey::Native(input.clone())]
            .ring
            .queue_head(),
        Some(1024)
    );
    assert_eq!(
        program.groups[&AudioInputKey::Native(input.clone())]
            .ring
            .queue_frames(),
        1024
    );
    assert_eq!(program.buffered_prepare(1024).unwrap().pending_inputs, 0);
    let output = program.render(1024, 1024).unwrap();
    assert_eq!(&output.samples[..512], &[0.25; 512]);
    assert_eq!(&output.samples[512..], &[0.5; 512]);
    assert!(output.gaps.is_empty());
    assert_eq!(
        program.buffered_discard(1024).unwrap().discarded_frames,
        1024
    );
    assert_eq!(
        program.groups[&AudioInputKey::Native(input.clone())]
            .ring
            .queue_frames(),
        0
    );
}

#[test]
fn three_unchanged_pending_observations_clear_stopped_data_and_timestamp() {
    let (_, mut program, inputs, _) = setup(&["Stopped"]);
    let input = &inputs[0];
    feed(&mut program, input, 0, 256, 0.25, false);
    for at in [0, 1024] {
        assert_eq!(program.buffered_prepare(at).unwrap().pending_inputs, 1);
        let report = program.buffered_discard(at).unwrap();
        assert_eq!(report.stopped_inputs, 0);
        assert_eq!(
            program.groups[&AudioInputKey::Native(input.clone())]
                .ring
                .queue_frames(),
            256
        );
    }
    program.buffered_prepare(2048).unwrap();
    let report = program.buffered_discard(2048).unwrap();
    assert_eq!(report.stopped_inputs, 1);
    assert_eq!(report.discarded_frames, 256);
    assert_eq!(report.reset_inputs, 0);
    assert_eq!(
        program.groups[&AudioInputKey::Native(input.clone())]
            .ring
            .queue_head(),
        None
    );
    assert_eq!(
        program.groups[&AudioInputKey::Native(input.clone())]
            .ring
            .queue_frames(),
        0
    );
    assert_eq!(program.buffered_prepare(3072).unwrap().minimum_frame, None);
}

#[test]
fn max_lag_drops_ceil_prefix_or_resets_unrecoverable_processor_with_fence_retained() {
    let (_, mut program, inputs, _) = setup(&["Recoverable", "Unrecoverable"]);
    feed(&mut program, &inputs[0], -512, 2048, 0.25, false);
    feed(&mut program, &inputs[1], -2048, 256, 0.5, false);
    let fence = program.input_cursor(&inputs[1]).unwrap();
    let report = program.buffered_drop_late(0).unwrap();
    assert_eq!(report.discarded_frames, 768);
    assert_eq!(report.reset_inputs, 1);
    assert_eq!(
        program.groups[&AudioInputKey::Native(inputs[0].clone())]
            .ring
            .queue_head(),
        Some(0)
    );
    assert_eq!(
        program.groups[&AudioInputKey::Native(inputs[0].clone())]
            .ring
            .queue_frames(),
        1536
    );
    assert!(
        !program
            .groups
            .contains_key(&AudioInputKey::Native(inputs[1].clone()))
    );
    assert_eq!(program.input_cursor(&inputs[1]), Some(fence));
    assert_eq!(program.buffered_prepare(0).unwrap().minimum_frame, Some(0));
    assert_eq!(program.render(0, 1024).unwrap().samples, vec![0.25; 1024]);
    program.buffered_discard(0).unwrap();
    feed(&mut program, &inputs[1], 1024, 1024, 0.75, false);
    program.buffered_prepare(1024).unwrap();
    assert_eq!(
        program.render(1024, 1024).unwrap().samples,
        vec![0.75; 1024]
    );
}

#[test]
fn gap_zeros_count_for_fifo_readiness_but_are_still_missing_samples() {
    let (_, mut program, inputs, sources) = setup(&["Gap"]);
    let input = &inputs[0];
    feed(&mut program, input, 0, 256, 0.25, false);
    let placed = feed(&mut program, input, 1024, 256, 0.75, true);
    assert_eq!(placed.gap_frames, 768);
    let ready = program.buffered_prepare(0).unwrap();
    assert_eq!(ready.minimum_frame, Some(0));
    assert_eq!(ready.pending_inputs, 0);
    let block = program.render(0, 1024).unwrap();
    assert_eq!(&block.samples[..256], &[0.25; 256]);
    assert_eq!(&block.samples[256..], &[0.0; 768]);
    assert_eq!(block.gaps[0].source, sources[0]);
    assert_eq!(block.gaps[0].missing_frames, 768);
    program.buffered_discard(0).unwrap();
    assert_eq!(
        program.groups[&AudioInputKey::Native(input.clone())]
            .ring
            .queue_head(),
        Some(1024)
    );
    assert_eq!(
        program.groups[&AudioInputKey::Native(input.clone())]
            .ring
            .queue_frames(),
        256
    );
}

#[test]
fn captured_zero_pcm_is_full_ready_data_without_missing_diagnostics() {
    let (_, mut program, inputs, _) = setup(&["Captured silence"]);
    feed(&mut program, &inputs[0], 0, 1024, 0.0, false);
    let ready = program.buffered_prepare(0).unwrap();
    assert_eq!(ready.minimum_frame, Some(0));
    assert_eq!(ready.pending_inputs, 0);
    let output = program.render(0, 1024).unwrap();
    assert_eq!(output.samples, vec![0.0; 1024]);
    assert!(output.gaps.is_empty());
    assert_eq!(program.buffered_discard(0).unwrap().discarded_frames, 1024);
}

#[test]
fn auxiliary_admission_and_buffered_skip_are_bounded_and_transactional() {
    let (_, mut program, _, _) = setup(&[]);
    let inputs: Vec<_> = (0..16)
        .map(|index| binding(&format!("Auxiliary {index}")))
        .collect();
    program.set_capture_bindings(inputs.clone()).unwrap();
    assert_eq!(program.capture_bindings().len(), 16);
    let before = serde_json::to_value(program.status()).unwrap();
    let mut excessive = inputs.clone();
    excessive.push(binding("Seventeenth"));
    assert!(program.set_capture_bindings(excessive).is_err());
    assert_eq!(serde_json::to_value(program.status()).unwrap(), before);
    assert!(program.buffered_prepare(i128::MAX).is_err());
    assert!(program.buffered_discard(i128::MAX).is_err());
    assert!(program.buffered_drop_late(i128::MAX).is_err());
    assert_eq!(serde_json::to_value(program.status()).unwrap(), before);
    feed(&mut program, &inputs[0], 0, 2048, 0.25, false);
    program.buffered_prepare(0).unwrap();
    let output = program.render(0, 1024).unwrap();
    assert_eq!(output.samples, vec![0.0; 1024]);
    assert!(output.gaps.is_empty());
    program.skip_to(4800).unwrap();
    assert_eq!(
        program.groups[&AudioInputKey::Native(inputs[0].clone())]
            .ring
            .queue_frames(),
        0
    );
    assert_eq!(program.buffered_drop_late(4800).unwrap().reset_inputs, 1);
    program.set_capture_bindings(Vec::new()).unwrap();
    assert!(program.groups.is_empty());
    program
        .set_capture_bindings(vec![inputs[0].clone(); 17])
        .unwrap();
    assert_eq!(program.capture_bindings().len(), 1);
    assert_eq!(
        program.capture_bindings.capacity(),
        1,
        "normalized metadata cannot retain input overcapacity"
    );
}

#[test]
fn compound_buffered_skip_accounts_source_loss_before_retiring_pcm_once() {
    let (_, mut program, inputs, _) = setup(&["Suspended source"]);
    let input = &inputs[0];
    feed(&mut program, input, 0, 4096, 0.25, false);
    program.buffered_prepare(0).unwrap();
    assert_eq!(program.render(0, 1024).unwrap().samples, vec![0.25; 1024]);
    assert_eq!(program.buffered_discard(0).unwrap().discarded_frames, 1024);
    assert_eq!(
        program.groups[&AudioInputKey::Native(input.clone())]
            .ring
            .queue_head(),
        Some(1024)
    );
    assert_eq!(
        program.groups[&AudioInputKey::Native(input.clone())]
            .ring
            .queue_frames(),
        3072
    );
    let fence = program.input_cursor(input).unwrap();
    let before = serde_json::to_value(program.status()).unwrap();
    assert!(matches!(
        program.buffered_skip_to(1023),
        Err(ProgramAudioError::ClockRewind)
    ));
    assert!(program.buffered_skip_to(u64::MAX).is_err());
    assert_eq!(serde_json::to_value(program.status()).unwrap(), before);
    let mut preserved = [0.0; 16];
    assert_eq!(
        program.groups[&AudioInputKey::Native(input.clone())]
            .ring
            .read(1024, 16, &mut preserved)
            .unwrap(),
        0
    );
    assert_eq!(preserved, [0.25; 16]);
    let generation = program.status().data_generation;
    let target = 3600 * 48000u64;
    let loss = program.buffered_skip_to(target).unwrap();
    assert_eq!(loss.discarded_frames, 3072);
    assert_eq!(loss.reset_inputs, 1);
    assert_eq!(loss.stopped_inputs, 0);
    assert_eq!(program.status().cursor, Some(target));
    assert_eq!(program.status().data_generation, generation + 1);
    assert!(
        !program
            .groups
            .contains_key(&AudioInputKey::Native(input.clone()))
    );
    assert_eq!(program.input_cursor(input), Some(fence));
    let again = program.buffered_drop_late(i128::from(target)).unwrap();
    assert_eq!(again.discarded_frames, 0);
    assert_eq!(again.reset_inputs, 0);
    feed(&mut program, input, i128::from(target), 1024, 0.75, false);
    assert_eq!(
        program
            .buffered_prepare(i128::from(target))
            .unwrap()
            .minimum_frame,
        Some(i128::from(target))
    );
    let resumed = program.render(target, 1024).unwrap();
    assert_eq!(resumed.samples, vec![0.75; 1024]);
    assert!(resumed.gaps.is_empty());
}

#[test]
fn waiting_only_skip_recovers_late_source_even_when_media_cursor_does_not_move() {
    let (_, mut program, inputs, _) = setup(&["Wait-only recovery"]);
    let input = &inputs[0];
    program.buffered_skip_to(1024).unwrap();
    feed(&mut program, input, 0, 512, 0.25, false);
    let generation = program.status().data_generation;
    let report = program.buffered_skip_to(1024).unwrap();
    assert_eq!(report.discarded_frames, 512);
    assert_eq!(report.reset_inputs, 1);
    assert_eq!(program.status().cursor, Some(1024));
    assert_eq!(program.status().data_generation, generation + 1);
    let report = program.buffered_skip_to(1024).unwrap();
    assert_eq!(report.discarded_frames, 0);
    assert_eq!(report.reset_inputs, 0);
    assert_eq!(program.status().data_generation, generation + 1);
}

#[test]
fn ready_head_normalization_does_not_reset_an_unrelated_pending_source() {
    let (_, mut program, inputs, _) = setup(&["Ready head", "Pending head"]);
    feed(&mut program, &inputs[0], 0, 4096, 0.25, false);
    feed(&mut program, &inputs[1], -5120, 4, 0.5, false);
    let pending_fence = program.input_cursor(&inputs[1]).unwrap();
    let readiness = program.buffered_prepare(1).unwrap();
    assert_eq!(readiness.minimum_frame, Some(0));
    assert_eq!(readiness.pending_inputs, 1);
    let normalized = program.buffered_normalize_ready(1).unwrap();
    assert_eq!(normalized.discarded_frames, 1);
    assert_eq!(normalized.reset_inputs, 0);
    assert_eq!(
        program.groups[&AudioInputKey::Native(inputs[0].clone())]
            .ring
            .queue_head(),
        Some(1)
    );
    assert_eq!(
        program.groups[&AudioInputKey::Native(inputs[0].clone())]
            .ring
            .queue_frames(),
        4095
    );
    let pending = &program.groups[&AudioInputKey::Native(inputs[1].clone())];
    assert_eq!(pending.ring.queue_head(), Some(-5120));
    assert_eq!(pending.ring.queue_frames(), 4);
    assert!(pending.buffered.pending);
    assert_eq!(pending.buffered.last_size, 0);
    assert!(!pending.buffered.pending_stop);
    assert_eq!(program.input_cursor(&inputs[1]), Some(pending_fence));
    // Maximum-buffer recovery still inspects pending sources and resets an
    // unrecoverable one; the ready-only eligibility filter must not leak here.
    let recovery = program.buffered_drop_late(1).unwrap();
    assert_eq!(recovery.discarded_frames, 4);
    assert_eq!(recovery.reset_inputs, 1);
    assert!(
        !program
            .groups
            .contains_key(&AudioInputKey::Native(inputs[1].clone()))
    );
    assert_eq!(program.input_cursor(&inputs[1]), Some(pending_fence));
}

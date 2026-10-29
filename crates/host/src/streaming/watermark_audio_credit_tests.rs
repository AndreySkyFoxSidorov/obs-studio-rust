use super::*;
fn key() -> StreamKey {
    StreamKey {
        session: 1,
        output: 2,
    }
}
fn packet(track: Track, dts: i64) -> Packet {
    Packet {
        track,
        dts,
        pts: dts,
        duration: 1,
        bytes: if track == Track::Video {
            Arc::from([0, 0, 0, 2, 0x65, 0x80])
        } else {
            Arc::from([0x21])
        },
    }
}
fn audio(q: &mut Outbox, first: u64, frames: usize, times: &[i64]) {
    assert!(q.reserve_audio_encode(first, frames).unwrap());
    q.admit_batch(
        Track::Audio,
        times.iter().map(|t| packet(Track::Audio, *t)).collect(),
        false,
    )
    .unwrap();
}
#[test]
fn three_retained_aus_preserve_two_real_eos_credits_within_original_five_slots() {
    let mut q = Outbox::new(key(), true);
    q.audio_offset(0).unwrap();
    assert!(q.reserve(Track::Video, Operation::Encode).unwrap());
    q.admit_batch(Track::Video, vec![packet(Track::Video, 0)], false)
        .unwrap();
    for n in 0..3 {
        audio(&mut q, n * 1024, 1024, &[n as i64 * 10]);
    }
    assert!(matches!(q.front().unwrap(), Some(EncodedPacket::Video(_))));
    q.pop().unwrap();
    assert_eq!(q.state().audio_packets, 3);
    assert_eq!(q.state().audio_reserved_packets, 2);
    let before = q.state();
    assert!(!q.reserve_audio_encode(3072, 1024).unwrap());
    assert_eq!(q.state(), before);
    assert_eq!(q.audio_pcm_frames, 3072);
    assert!(q.reserve_audio_finish().unwrap());
    assert!(!q.state().audio_drained);
    q.admit_batch(
        Track::Audio,
        vec![packet(Track::Audio, 30), packet(Track::Audio, 40)],
        true,
    )
    .unwrap();
    assert!(q.state().audio_drained);
    assert_eq!(q.state().audio_packets, 5);
    assert_eq!(q.state().audio_reserved_packets, 0);
    assert!(
        q.front().unwrap().is_none(),
        "actual Audio EOS does not invent a higher Video watermark"
    );
    assert!(q.reserve(Track::Video, Operation::Encode).unwrap());
    q.admit_batch(Track::Video, vec![packet(Track::Video, 100)], false)
        .unwrap();
    for _ in 0..5 {
        assert!(matches!(q.front().unwrap(), Some(EncodedPacket::Audio(_))));
        q.pop().unwrap();
    }
}
#[test]
fn partial_pcm_and_delayed_output_use_checked_cumulative_bounds_without_guessed_eos() {
    let mut q = Outbox::new(key(), true);
    q.audio_offset(0).unwrap();
    assert!(q.reserve_audio_encode(0, 1).unwrap());
    assert_eq!(q.audio_queue.reserved_packets(), 0);
    let before = q.state();
    assert!(
        q.admit_batch(Track::Audio, vec![packet(Track::Audio, 0)], false)
            .is_err()
    );
    assert_eq!(q.state(), before);
    q.admit_batch(Track::Audio, vec![], false).unwrap();
    audio(&mut q, 1, 1024, &[0]);
    assert_eq!(q.state().audio_reserved_packets, 3, "ceil1025/1024 +2 -1");
    assert!(q.reserve_audio_finish().unwrap());
    assert_eq!(q.audio_queue.reserved_packets(), 3);
    q.admit_batch(
        Track::Audio,
        vec![
            packet(Track::Audio, 10),
            packet(Track::Audio, 20),
            packet(Track::Audio, 30),
        ],
        true,
    )
    .unwrap();
    assert_eq!(q.state().audio_packets, 4);
    assert!(q.state().audio_drained);
}
#[test]
fn invalid_credit_requests_and_invalid_whole_replies_leave_all_credit_state_unchanged() {
    let mut q = Outbox::new(key(), true);
    q.audio_offset(0).unwrap();
    for (first, frames) in [(1, 1), (0, 0), (0, 1025), (u64::MAX, 1)] {
        let before = q.state();
        assert!(q.reserve_audio_encode(first, frames).is_err());
        assert_eq!(q.state(), before);
        assert_eq!(q.audio_pcm_frames, 0);
        assert_eq!(q.audio_future, None);
    }
    assert!(q.reserve_audio_encode(0, 1024).unwrap());
    let before = q.state();
    for (batch, eos) in [
        (
            vec![packet(Track::Audio, 0), packet(Track::Audio, 1)],
            false,
        ),
        (vec![packet(Track::Video, 0)], false),
        (vec![], true),
    ] {
        assert!(q.admit_batch(Track::Audio, batch, eos).is_err());
        assert_eq!(q.state(), before);
        assert_eq!(q.audio_pcm_frames, 1024);
    }
    q.admit_batch(Track::Audio, vec![packet(Track::Audio, -100)], false)
        .unwrap();
    assert_eq!(q.audio_queue.front().unwrap().dts, -100);
    assert_eq!(q.state().audio_reserved_packets, 2);
    assert!(
        q.reserve(Track::Audio, Operation::Encode).is_err(),
        "generic reservation cannot bypass enduring AAC credit"
    );
}
#[test]
fn delayed_three_block_encoder_can_finish_with_four_final_aus_without_cap_changes() {
    let mut q = Outbox::new(key(), true);
    q.audio_offset(0).unwrap();
    for n in 0..3 {
        audio(&mut q, n * 1024, 1024, &[]);
    }
    assert_eq!(q.state().audio_packets, 0);
    assert_eq!(q.state().audio_reserved_packets, 5);
    assert!(q.reserve_audio_finish().unwrap());
    assert_eq!(q.audio_queue.reserved_packets(), 4);
    q.admit_batch(
        Track::Audio,
        (0..4).map(|t| packet(Track::Audio, t)).collect(),
        true,
    )
    .unwrap();
    assert_eq!(q.state().audio_packets, 4);
    assert!(q.state().audio_drained);
    assert!(q.reserve(Track::Video, Operation::Encode).unwrap());
    let s = q.state();
    assert!(
        s.audio_bytes + s.audio_reserved_bytes + s.video_bytes + s.video_reserved_bytes
            <= MAX_OUTBOX_BYTES
    );
    assert!(s.audio_packets + s.audio_reserved_packets <= TRACK_SLOTS);
}

#[test]
fn avc_four_delayed_accepted_frames_keep_exact_reply_credit_and_real_eos() {
    let mut q = Outbox::new(key(), true);
    q.audio_offset(0).unwrap();
    for _ in 0..4 {
        assert!(q.can_reserve_video_encode().unwrap());
        assert!(q.reserve_video_encode().unwrap());
        q.admit_batch(Track::Video, vec![], false).unwrap();
    }
    let before = q.state();
    assert!(q.can_reserve_video_encode().is_err());
    assert!(q.reserve_video_encode().is_err());
    assert_eq!(q.state(), before);
    assert!(q.reserve_video_finish().unwrap());
    let before = q.state();
    assert!(
        q.admit_batch(
            Track::Video,
            (0..3).map(|t| packet(Track::Video, t)).collect(),
            true
        )
        .is_err(),
        "an EOS claiming an unmatched fourth input is invalid"
    );
    assert_eq!(q.state(), before);
    q.admit_batch(
        Track::Video,
        (0..4).map(|t| packet(Track::Video, t)).collect(),
        true,
    )
    .unwrap();
    assert!(q.state().video_drained);
    assert_eq!(q.state().video_packets, 4);
}

#[test]
fn avc_three_actual_aus_finish_with_zero_reserved_tail_without_audio_advancement() {
    let mut q = Outbox::new(key(), true);
    q.audio_offset(0).unwrap();
    for n in 0..3 {
        assert!(q.reserve_video_encode().unwrap());
        q.admit_batch(Track::Video, vec![packet(Track::Video, n * 10)], false)
            .unwrap();
    }
    audio(&mut q, 0, 1024, &[0]);
    assert!(q.front().unwrap().is_none());
    assert!(q.reserve_video_finish().unwrap());
    assert_eq!(q.video.reserved_packets(), 0);
    assert!(!q.state().video_drained);
    q.admit_batch(Track::Video, vec![], true).unwrap();
    assert!(q.state().video_drained);
    assert_eq!(q.state().video_packets, 3);
    audio(&mut q, 1024, 1024, &[7]);
    assert!(matches!(q.front().unwrap(), Some(EncodedPacket::Video(_))));
    q.pop().unwrap();
    assert!(matches!(q.front().unwrap(), Some(EncodedPacket::Audio(_))));
}

#[test]
fn avc_reply_cannot_fabricate_more_frames_than_the_admitted_input() {
    let mut q = Outbox::new(key(), false);
    assert!(q.reserve_video_encode().unwrap());
    let before = q.state();
    assert!(
        q.admit_batch(
            Track::Video,
            vec![packet(Track::Video, 0), packet(Track::Video, 1)],
            false
        )
        .is_err()
    );
    assert_eq!(q.state(), before);
    assert_eq!(q.video_inputs, 1);
    q.admit_batch(Track::Video, vec![packet(Track::Video, 0)], false)
        .unwrap();
    q.video_inputs = u64::MAX;
    q.video.produced = u64::MAX;
    let before = q.state();
    assert!(q.reserve_video_encode().is_err());
    assert_eq!(q.state(), before);
}

#[test]
fn unsupported_aac_lookahead_fails_before_mutation_when_no_queued_packet_can_free_room() {
    let mut q = Outbox::new(key(), true);
    q.audio_offset(0).unwrap();
    for n in 0..3 {
        audio(&mut q, n * 1024, 1024, &[]);
    }
    assert_eq!(q.state().audio_packets, 0);
    assert_eq!(q.state().audio_reserved_packets, 5);
    let before = q.state();
    let old_frames = q.audio_pcm_frames;
    assert!(matches!(
        q.reserve_audio_encode(3072, 1024),
        Err(Error::Limit(
            "AAC lookahead exceeds five-slot tail-credit profile"
        ))
    ));
    assert_eq!(q.state(), before);
    assert_eq!(q.audio_pcm_frames, old_frames);
    assert!(
        !q.state().audio_drained,
        "rejection never fabricates codec EOS"
    );
}

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
        pts: dts,
        dts,
        duration: 1,
        bytes: if track == Track::Video {
            Arc::from([0, 0, 0, 2, 0x65, 0x80])
        } else {
            Arc::from([0x21])
        },
    }
}
fn put(out: &mut Outbox, track: Track, times: &[i64], finish: bool) {
    assert!(
        out.reserve(
            track,
            if finish {
                Operation::Finish
            } else {
                Operation::Encode
            }
        )
        .unwrap()
    );
    out.admit_batch(
        track,
        times.iter().map(|t| packet(track, *t)).collect(),
        finish,
    )
    .unwrap();
}
fn pop(out: &mut Outbox) -> (bool, u64) {
    let p = out.front().unwrap().unwrap();
    let result = (matches!(&p, EncodedPacket::Audio(_)), p.dts_ns());
    out.pop().unwrap();
    result
}
#[test]
fn exact_ties_need_strictly_higher_peer_and_video_precedes_audio() {
    let mut q = Outbox::new(key(), true);
    q.audio_offset(0).unwrap();
    put(&mut q, Track::Audio, &[0], false);
    put(&mut q, Track::Video, &[0], false);
    assert!(q.front().unwrap().is_none());
    assert_eq!(q.state().waiting, InterleaveWait::AudioWatermark);
    put(&mut q, Track::Audio, &[10], false);
    assert_eq!(pop(&mut q), (false, 0));
    assert!(q.front().unwrap().is_none());
    put(&mut q, Track::Video, &[10], false);
    assert_eq!(pop(&mut q), (true, 0));
    assert!(q.front().unwrap().is_none());
    put(&mut q, Track::Audio, &[], true);
    assert_eq!(pop(&mut q), (false, 1000));
    put(&mut q, Track::Video, &[], true);
    assert_eq!(pop(&mut q), (true, 1000));
    assert!(q.is_empty());
}
#[test]
fn negative_clock_anchor_and_native_duration_are_preserved_while_order_is_global() {
    let mut q = Outbox::new(key(), true);
    q.audio_offset(208).unwrap();
    put(&mut q, Track::Video, &[0], false);
    assert!(q.front().unwrap().is_none());
    assert!(q.reserve(Track::Audio, Operation::Encode).unwrap());
    let p = Packet {
        duration: 213335,
        ..packet(Track::Audio, -213333)
    };
    q.admit_batch(Track::Audio, vec![p], false).unwrap();
    assert_eq!(q.audio_queue.front().unwrap().duration, 213335);
    assert_eq!(pop(&mut q), (true, 0));
    assert!(q.front().unwrap().is_none());
    put(&mut q, Track::Audio, &[], true);
    assert_eq!(pop(&mut q), (false, 21_312_500));
    assert!(q.is_empty());
    assert!(q.reserve(Track::Audio, Operation::Encode).is_err());
}
#[test]
fn sub_microsecond_order_refines_obs_clock_without_quantizing_packet_timestamps() {
    let mut q = Outbox::new(key(), true);
    q.audio_offset(0).unwrap();
    put(&mut q, Track::Video, &[2, 4], false);
    put(&mut q, Track::Audio, &[1, 3], false);
    assert_eq!(pop(&mut q), (true, 100));
    assert_eq!(pop(&mut q), (false, 200));
    assert_eq!(pop(&mut q), (true, 300));
    put(&mut q, Track::Audio, &[], true);
    assert_eq!(pop(&mut q), (false, 400));
}
#[test]
fn five_reserved_slots_allow_opposing_four_packet_reply_and_never_steal_peer_capacity() {
    let mut q = Outbox::new(key(), true);
    q.audio_offset(0).unwrap();
    put(&mut q, Track::Video, &[0], false);
    put(&mut q, Track::Audio, &[0], false);
    assert!(q.reserve(Track::Video, Operation::Encode).unwrap());
    assert!(q.reserve(Track::Audio, Operation::Encode).unwrap());
    assert_eq!(q.state().video_reserved_packets, 4);
    assert_eq!(q.state().audio_reserved_packets, 4);
    q.admit_batch(
        Track::Video,
        [1, 2, 3, 4].map(|t| packet(Track::Video, t)).into(),
        false,
    )
    .unwrap();
    q.admit_batch(
        Track::Audio,
        [1, 2, 3, 4].map(|t| packet(Track::Audio, t)).into(),
        false,
    )
    .unwrap();
    assert_eq!(q.state().video_packets + q.state().audio_packets, 10);
    assert!(!q.can_reserve(Track::Video));
    assert!(!q.can_reserve(Track::Audio));
    let mut got = vec![];
    while q.front().unwrap().is_some() {
        got.push(pop(&mut q));
    }
    assert_eq!(
        got,
        [
            (false, 0),
            (true, 0),
            (false, 100),
            (true, 100),
            (false, 200),
            (true, 200),
            (false, 300),
            (true, 300)
        ]
    );
    assert_eq!(q.state().video_packets, 1);
    assert_eq!(q.state().audio_packets, 1);
    put(&mut q, Track::Video, &[], true);
    assert!(q.front().unwrap().is_none());
    put(&mut q, Track::Audio, &[], true);
    assert_eq!(pop(&mut q), (false, 400));
    assert_eq!(pop(&mut q), (true, 400));
    assert!(q.is_empty());
}
#[test]
fn full_network_hold_retains_identical_heads_until_actual_admission() {
    let mut q = Outbox::new(key(), true);
    q.audio_offset(0).unwrap();
    put(&mut q, Track::Video, &[0, 10, 20, 30], false);
    put(&mut q, Track::Audio, &[0, 7, 14, 21], false);
    let state = q.state();
    for _ in 0..100 {
        let head = q.front().unwrap().unwrap();
        assert_eq!(head.dts_ns(), 0);
        assert!(matches!(head, EncodedPacket::Video(_)));
        assert_eq!(q.state(), state);
    }
    let mut values = vec![];
    while q.front().unwrap().is_some() {
        values.push(pop(&mut q));
    }
    assert_eq!(
        values,
        [
            (false, 0),
            (true, 0),
            (true, 700),
            (false, 1000),
            (true, 1400),
            (false, 2000),
            (true, 2100)
        ]
    );
    put(&mut q, Track::Audio, &[], true);
    assert_eq!(pop(&mut q), (false, 3000));
    put(&mut q, Track::Video, &[], true);
}
#[test]
fn invalid_whole_reply_and_overflow_leave_reservation_and_watermarks_unchanged() {
    let mut q = Outbox::new(key(), true);
    q.audio_offset(0).unwrap();
    assert!(q.reserve(Track::Video, Operation::Encode).unwrap());
    let baseline = q.state();
    for batch in [
        vec![packet(Track::Video, 0), packet(Track::Video, 0)],
        vec![packet(Track::Video, 0), packet(Track::Audio, 1)],
        vec![packet(Track::Video, i64::MAX)],
    ] {
        assert!(q.admit_batch(Track::Video, batch, false).is_err());
        assert_eq!(q.state(), baseline);
    }
    assert!(q.admit_batch(Track::Video, vec![], true).is_err());
    assert_eq!(q.state(), baseline);
    q.admit_batch(Track::Video, vec![], false).unwrap();
    assert_eq!(q.state().video_reserved_packets, 0);
    assert!(q.reserve(Track::Video, Operation::Finish).unwrap());
    q.admit_batch(Track::Video, vec![], true).unwrap();
    assert!(q.reserve(Track::Video, Operation::Finish).is_err());
}
#[test]
fn payload_boundaries_charge_five_actual_aus_and_four_reserved_aus() {
    let mut q = Outbox::new(key(), true);
    q.audio_offset(0).unwrap();
    let payload: Arc<[u8]> = vec![0; 2 * 1024 * 1024].into();
    assert!(q.reserve(Track::Video, Operation::Encode).unwrap());
    q.admit_batch(
        Track::Video,
        vec![Packet {
            bytes: payload.clone(),
            ..packet(Track::Video, 0)
        }],
        false,
    )
    .unwrap();
    assert!(q.reserve(Track::Video, Operation::Encode).unwrap());
    assert_eq!(q.state().video_reserved_bytes, 8 * 1024 * 1024);
    assert_eq!(q.state().video_bytes, 2 * 1024 * 1024);
    q.admit_batch(
        Track::Video,
        (1..=4)
            .map(|t| Packet {
                bytes: payload.clone(),
                ..packet(Track::Video, t)
            })
            .collect(),
        false,
    )
    .unwrap();
    assert_eq!(q.state().video_bytes, 10 * 1024 * 1024);
    assert!(!q.can_reserve(Track::Video));
    assert!(q.can_reserve(Track::Audio));
    assert!(q.reserve(Track::Audio, Operation::Encode).unwrap());
    let before = q.state();
    assert!(
        q.admit_batch(
            Track::Audio,
            vec![Packet {
                bytes: vec![0; 64 * 1024 + 1].into(),
                ..packet(Track::Audio, 0)
            }],
            false
        )
        .is_err()
    );
    assert_eq!(q.state(), before);
    assert!(
        before.video_bytes
            + before.video_reserved_bytes
            + before.audio_bytes
            + before.audio_reserved_bytes
            <= MAX_OUTBOX_BYTES
    );
}
#[test]
fn disabled_audio_never_holds_video_and_real_eos_releases_long_peer_tail() {
    let mut video = Outbox::new(key(), false);
    put(&mut video, Track::Video, &[0, 10, 20, 30], false);
    for expected in [0, 1000, 2000, 3000] {
        assert_eq!(pop(&mut video), (false, expected));
    }
    assert!(video.reserve(Track::Audio, Operation::Encode).is_err());
    put(&mut video, Track::Video, &[], true);
    assert!(video.state().audio_drained);
    let mut q = Outbox::new(key(), true);
    q.audio_offset(0).unwrap();
    put(&mut q, Track::Video, &[0], true);
    put(&mut q, Track::Audio, &[0, 10, 20, 30], false);
    assert_eq!(pop(&mut q), (false, 0));
    for expected in [0, 1000, 2000, 3000] {
        assert_eq!(pop(&mut q), (true, expected));
    }
    put(&mut q, Track::Audio, &[40, 50, 60, 70], true);
    for expected in [4000, 5000, 6000, 7000] {
        assert_eq!(pop(&mut q), (true, expected));
    }
    assert!(q.is_empty());
}

#[test]
fn adjusted_nanosecond_overflow_is_rejected_before_any_batch_publication() {
    let mut q = Outbox::new(key(), true);
    q.audio_offset(i64::MAX).unwrap();
    put(&mut q, Track::Video, &[0], false);
    assert!(q.reserve(Track::Audio, Operation::Encode).unwrap());
    let old = q.state();
    assert!(
        q.admit_batch(Track::Audio, vec![packet(Track::Audio, 0)], false)
            .is_err()
    );
    assert_eq!(q.state(), old);
    assert!(q.gated());
}

#[test]
#[ignore = "explicit independently compiled source C ordering fixture; no compiler/process runs in ordinary tests"]
fn supplied_c_ordering_oracle_matches_identical_microsecond_arrivals_and_eos_extension() {
    use std::io::Read;
    let file = std::env::var_os("SKYLIVE_WATERMARK_REFERENCE_PATH")
        .expect("run staged reference tool first");
    let mut bytes = vec![];
    std::fs::File::open(file)
        .unwrap()
        .take(4097)
        .read_to_end(&mut bytes)
        .unwrap();
    assert!(bytes.len() <= 4096);
    let text = std::str::from_utf8(&bytes).unwrap();
    let expected: Vec<_> = text
        .lines()
        .map(|line| {
            let mut fields = line.split_whitespace();
            let audio = match fields.next().unwrap() {
                "A" => true,
                "V" => false,
                _ => panic!("track"),
            };
            let us = fields.next().unwrap().parse::<u64>().unwrap();
            assert!(fields.next().is_none());
            (audio, us * 1000)
        })
        .collect();
    assert_eq!(expected.len(), 14);
    let mut q = Outbox::new(key(), true);
    q.audio_offset(0).unwrap();
    let arrivals = [
        (Track::Video, 0),
        (Track::Video, 30),
        (Track::Audio, 0),
        (Track::Audio, 20),
        (Track::Audio, 40),
        (Track::Video, 60),
        (Track::Audio, 60),
        (Track::Video, 90),
        (Track::Audio, 80),
        (Track::Video, 120),
        (Track::Audio, 100),
        (Track::Audio, 120),
        (Track::Video, 150),
        (Track::Audio, 140),
    ];
    let mut actual = vec![];
    for (track, us) in arrivals {
        put(&mut q, track, &[us * 10], false);
        while q.front().unwrap().is_some() {
            actual.push(pop(&mut q));
        }
    }
    for track in [Track::Video, Track::Audio] {
        put(&mut q, track, &[], true);
        while q.front().unwrap().is_some() {
            actual.push(pop(&mut q));
        }
    }
    assert!(q.is_empty());
    assert_eq!(actual, expected);
}
#[test]
fn finite_adversarial_native_batch_ratios_make_progress_and_preserve_the_complete_sorted_prefix() {
    for (video_count, audio_count, video_batch, audio_batch) in
        [(1, 47, 1, 4), (12, 47, 4, 3), (47, 1, 4, 1), (8, 8, 4, 4)]
    {
        let mut q = Outbox::new(key(), true);
        q.audio_offset(0).unwrap();
        let mut vi = 0;
        let mut ai = 0;
        let mut vd = false;
        let mut ad = false;
        let mut actual = vec![];
        for _ in 0..256 {
            let before = (vi, ai, vd, ad, actual.len());
            for track in [Track::Video, Track::Audio] {
                if q.can_reserve(track) {
                    let (index, total, batch, done) = if track == Track::Video {
                        (&mut vi, video_count, video_batch, &mut vd)
                    } else {
                        (&mut ai, audio_count, audio_batch, &mut ad)
                    };
                    if *index < total {
                        let end = (*index + batch).min(total);
                        let times: Vec<i64> = (*index..end)
                            .map(|n| {
                                if track == Track::Video {
                                    n as i64 * 10_000_000 / 30
                                } else {
                                    n as i64 * 1024 * 10_000_000 / 48000
                                }
                            })
                            .collect();
                        put(&mut q, track, &times, false);
                        *index = end;
                    } else if !*done {
                        put(&mut q, track, &[], true);
                        *done = true;
                    }
                }
            }
            while q.front().unwrap().is_some() {
                actual.push(pop(&mut q));
            }
            if vd && ad && q.is_empty() {
                break;
            }
            assert_ne!(
                (vi, ai, vd, ad, actual.len()),
                before,
                "bounded queues must permit peer input/real EOS even under asymmetric producer lead"
            );
        }
        assert!(vd && ad && q.is_empty());
        let mut expected: Vec<_> = (0..video_count)
            .map(|n| (false, (n as u64 * 10_000_000 / 30) * 100))
            .chain((0..audio_count).map(|n| (true, (n as u64 * 1024 * 10_000_000 / 48000) * 100)))
            .collect();
        expected.sort_by_key(|(audio, dts)| (*dts, *audio));
        assert_eq!(actual, expected);
    }
}

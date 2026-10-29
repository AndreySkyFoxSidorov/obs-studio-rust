use super::*;

fn adaptive(cursor: u64, rate: u32) -> BufferClock {
    BufferClock::new(cursor, rate, AudioBuffering::default()).unwrap()
}
fn fixed(cursor: u64, rate: u32, ms: u32) -> BufferClock {
    BufferClock::new(cursor, rate, AudioBuffering::new(ms, true).unwrap()).unwrap()
}

#[test]
fn adaptive_none_zero_and_future_heads_do_not_add_delay() {
    let mut clock = adaptive(0, 48000);
    for head in [None, Some(0), Some(1), Some(1024)] {
        assert_eq!(
            clock.observe_minimum(head).unwrap(),
            BufferDecision {
                start_frame: 0,
                waiting: false,
                added_ticks: 0
            }
        );
    }
    assert_eq!(clock.wall_ticks(), 0);
    assert_eq!(clock.cursor(), 0);
    assert_eq!(clock.total_ticks(), 0);
    assert_eq!(clock.waiting_ticks(), 0);
    assert_eq!(clock.max_ticks(), 45);
    assert!(!clock.maxed());
    clock.complete_tick().unwrap();
    assert_eq!(clock.wall_ticks(), 1);
    assert_eq!(clock.cursor(), 1024);
}

#[test]
fn fixed_twenty_and_eighty_ms_suppress_exact_startup_ticks_with_signed_preroll() {
    for rate in [44100, 48000] {
        for (ms, waits) in [(20, 1), (80, 4)] {
            let mut clock = fixed(0, rate, ms);
            assert!(clock.maxed());
            assert_eq!(clock.total_ticks(), waits);
            assert_eq!(clock.waiting_ticks(), waits);
            for tick in 0..waits {
                let decision = clock.observe_minimum(Some(-100_000)).unwrap();
                assert_eq!(
                    decision,
                    BufferDecision {
                        start_frame: -i128::from(waits - tick) * 1024,
                        waiting: true,
                        added_ticks: 0
                    }
                );
                clock.complete_tick().unwrap();
                assert_eq!(clock.cursor(), 0);
            }
            assert_eq!(clock.wall_ticks(), u64::from(waits));
            assert_eq!(clock.waiting_ticks(), 0);
            assert_eq!(
                clock.observe_minimum(None).unwrap(),
                BufferDecision {
                    start_frame: 0,
                    waiting: false,
                    added_ticks: 0
                }
            );
            clock.complete_tick().unwrap();
            assert_eq!(clock.cursor(), 1024);
            assert_eq!(clock.total_ticks(), waits);
        }
    }
}

#[test]
fn fixed_default_is_forty_five_while_explicit_one_second_is_rate_dependent() {
    for rate in [44100, 48000] {
        let clock = fixed(123, rate, 0);
        assert_eq!(clock.max_ticks(), 45);
        assert_eq!(clock.waiting_ticks(), 45);
        assert_eq!(clock.next_interval().unwrap(), -45957);
    }
    assert_eq!(fixed(0, 44100, 1000).max_ticks(), 44);
    assert_eq!(fixed(0, 48000, 1000).max_ticks(), 47);
    assert!(BufferClock::new(0, 8000, AudioBuffering::default()).is_err());
}

#[test]
fn literal_positive_timestamp_floors_precede_frame_floor_and_block_ceiling() {
    // At either project rate the first positive frame's nanoseconds are
    // truncated. Converting them back produces ZERO frames, not one tick.
    // Likewise1025timestamped frames round back to1024, still one tick.
    for rate in [44100, 48000] {
        for (cursor, expected) in [(1, 0), (1024, 1), (1025, 1), (1026, 2)] {
            let mut clock = adaptive(cursor, rate);
            let decision = clock.observe_minimum(Some(0)).unwrap();
            assert_eq!(decision.added_ticks, expected, "rate{rate}, cursor{cursor}");
            assert_eq!(
                decision.start_frame,
                i128::from(cursor) - i128::from(expected) * 1024
            );
        }
    }
    assert_eq!(frame_ns(1, 44100).unwrap(), 22675);
    assert_eq!(frame_ns(1025, 44100).unwrap(), 23242630);
    assert_eq!(frame_ns(1, 48000).unwrap(), 20833);
    assert_eq!(frame_ns(1025, 48000).unwrap(), 21354166);
}

#[test]
fn negative_frame_times_use_euclidean_floor_rather_than_truncation_toward_zero() {
    assert_eq!(frame_ns(-1, 44100).unwrap(), -22676);
    assert_eq!(frame_ns(-1, 48000).unwrap(), -20834);
    for rate in [44100, 48000] {
        let mut clock = adaptive(0, rate);
        let decision = clock.observe_minimum(Some(-1)).unwrap();
        assert_eq!(
            decision,
            BufferDecision {
                start_frame: -1024,
                waiting: true,
                added_ticks: 1
            }
        );
        clock.complete_tick().unwrap();
        assert_eq!(clock.next_interval().unwrap(), 0);
        assert_eq!(clock.cursor(), 0);
        assert_eq!(clock.total_ticks(), 1);
    }
}

#[test]
fn growing_during_wait_prepends_more_discard_only_intervals_without_replaying_emitted_time() {
    for rate in [44100, 48000] {
        let mut clock = adaptive(4096, rate);
        assert_eq!(
            clock.observe_minimum(Some(0)).unwrap(),
            BufferDecision {
                start_frame: 0,
                waiting: true,
                added_ticks: 4
            }
        );
        clock.complete_tick().unwrap();
        assert_eq!(clock.next_interval().unwrap(), 1024);
        let decision = clock.observe_minimum(Some(-1024)).unwrap();
        assert_eq!(
            decision,
            BufferDecision {
                start_frame: -1024,
                waiting: true,
                added_ticks: 2
            }
        );
        assert_eq!(clock.total_ticks(), 6);
        assert_eq!(clock.waiting_ticks(), 5);
        let mut discard = Vec::new();
        for _ in 0..5 {
            discard.push(clock.next_interval().unwrap());
            clock.complete_tick().unwrap();
            assert_eq!(clock.cursor(), 4096);
        }
        assert_eq!(discard, [-1024, 0, 1024, 2048, 3072]);
        assert_eq!(clock.wall_ticks(), 6);
        assert_eq!(clock.waiting_ticks(), 0);
        assert_eq!(
            clock.observe_minimum(None).unwrap(),
            BufferDecision {
                start_frame: 4096,
                waiting: false,
                added_ticks: 0
            }
        );
        clock.complete_tick().unwrap();
        assert_eq!(clock.cursor(), 5120);
        assert_eq!(clock.wall_ticks(), 7);
        assert_eq!(clock.total_ticks(), 6);
    }
}

#[test]
fn default_cap_is_forty_five_never_shrinks_and_ignores_further_lag_once_maxed() {
    let mut clock = adaptive(100_000, 48000);
    let decision = clock.observe_minimum(Some(-100_000_000)).unwrap();
    assert_eq!(decision.added_ticks, 45);
    assert_eq!(decision.start_frame, 53920);
    assert!(clock.maxed());
    // No unnecessary timestamp arithmetic is needed once the maximum is hit.
    assert_eq!(
        clock.observe_minimum(Some(i128::MIN)).unwrap().added_ticks,
        0
    );
    for _ in 0..45 {
        clock.complete_tick().unwrap();
    }
    assert_eq!(clock.cursor(), 100_000);
    assert_eq!(clock.total_ticks(), 45);
    assert!(clock.maxed());
    assert_eq!(
        clock.observe_minimum(Some(0)).unwrap(),
        BufferDecision {
            start_frame: 100_000,
            waiting: false,
            added_ticks: 0
        }
    );
}

#[test]
fn configured_cap_can_use_forty_seven_ticks_without_a_default_only_bound() {
    let mut clock = BufferClock::new(0, 48000, AudioBuffering::new(1000, false).unwrap()).unwrap();
    assert_eq!(
        clock.observe_minimum(Some(-1_000_000)).unwrap().added_ticks,
        47
    );
    assert_eq!(clock.waiting_ticks(), 47);
    assert_eq!(clock.next_interval().unwrap(), -48128);
    assert!(clock.maxed());
}

#[test]
fn skip_consumes_waiting_ticks_before_media_and_equal_skip_is_noop() {
    let mut clock = fixed(10, 48000, 80);
    let first = clock.skip_wall_to(2).unwrap();
    assert_eq!(
        first,
        BufferSkip {
            wall_ticks: 2,
            media_frames: 0,
            waiting_ticks: 2
        }
    );
    assert_eq!(clock.cursor(), 10);
    assert_eq!(clock.waiting_ticks(), 2);
    assert_eq!(clock.total_ticks(), 4);
    let before = clock.clone();
    assert_eq!(clock.skip_wall_to(2).unwrap(), BufferSkip::default());
    assert_eq!(clock, before);
    assert_eq!(
        clock.skip_wall_to(5).unwrap(),
        BufferSkip {
            wall_ticks: 3,
            media_frames: 1024,
            waiting_ticks: 2
        }
    );
    assert_eq!(clock.cursor(), 1034);
    assert_eq!(clock.wall_ticks(), 5);
    assert_eq!(clock.waiting_ticks(), 0);
}

#[test]
fn one_hour_suspension_skip_has_literal_counts_and_constant_work() {
    for (rate, target, frames) in [(48000, 168750, 172795904), (44100, 155039, 158755840)] {
        let mut clock = fixed(0, rate, 80);
        assert_eq!(
            clock.skip_wall_to(target).unwrap(),
            BufferSkip {
                wall_ticks: target,
                media_frames: frames,
                waiting_ticks: 4
            }
        );
        assert_eq!(clock.cursor(), frames);
        assert_eq!(clock.wall_ticks(), target);
        assert_eq!(clock.total_ticks(), 4);
        assert_eq!(clock.waiting_ticks(), 0);
        clock.complete_tick().unwrap();
        assert_eq!(clock.cursor(), frames + 1024);
    }
}

#[test]
fn all_overflow_and_backward_failures_preserve_the_complete_clock_candidate() {
    let mut clock = adaptive(0, 48000);
    let before = clock.clone();
    assert_eq!(
        clock.observe_minimum(Some(i128::MIN)),
        Err(AudioCaptureError::TimeOverflow)
    );
    assert_eq!(clock, before);
    assert_eq!(
        clock.skip_wall_to(u64::MAX),
        Err(AudioCaptureError::TimeOverflow)
    );
    assert_eq!(clock, before);
    clock.skip_wall_to(10).unwrap();
    let before = clock.clone();
    assert!(clock.skip_wall_to(9).is_err());
    assert_eq!(clock, before);
    let mut clock = adaptive(u64::MAX - 1023, 48000);
    let before = clock.clone();
    assert_eq!(clock.complete_tick(), Err(AudioCaptureError::TimeOverflow));
    assert_eq!(clock, before);
    let mut clock = fixed(0, 48000, 80);
    clock.wall_ticks = u64::MAX;
    let before = clock.clone();
    assert_eq!(clock.complete_tick(), Err(AudioCaptureError::TimeOverflow));
    assert_eq!(clock, before);
}

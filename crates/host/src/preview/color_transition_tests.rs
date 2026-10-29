//! Pixel and lifecycle checks through actual registry/controller/evaluation.
use super::*;
use crate::{playback::PlaybackClock, transition::Controller};
use skylive_core::{
    Command, QuickTransitionId, TransitionPresetId, TransitionRef, TransitionSelection,
};

struct Fixture {
    session: Session,
    owner: Controller,
    clock: PlaybackClock,
    a: Id,
    b: Id,
    preset: TransitionPresetId,
}
fn kind(rgb: [u8; 3], switch_point: u8) -> TransitionKind {
    TransitionKind::FadeToColor { rgb, switch_point }
}
fn fixture(rgb: [u8; 3], switch_point: u8) -> Fixture {
    let mut session = Session::new();
    let a = session.preview();
    session
        .apply(Command::SetVideo {
            width: 16,
            height: 16,
            fps_num: 30,
            fps_den: 1,
        })
        .unwrap();
    session
        .apply(Command::AddColorSource {
            scene: a,
            name: "Red".into(),
            rgba: [255, 0, 0, 255],
        })
        .unwrap();
    let b = session
        .apply(Command::AddScene {
            name: "Blue scene".into(),
        })
        .unwrap()
        .created
        .unwrap();
    session
        .apply(Command::AddColorSource {
            scene: b,
            name: "Blue".into(),
            rgba: [0, 0, 255, 255],
        })
        .unwrap();
    let preset = session
        .apply(Command::AddTransitionPreset {
            name: "Color transition".into(),
            kind: kind(rgb, switch_point),
        })
        .unwrap()
        .created_transition_preset
        .unwrap();
    session
        .apply(Command::SelectTransition {
            selection: TransitionSelection::new(TransitionRef::Preset(preset), 1000).unwrap(),
        })
        .unwrap();
    let mut owner = Controller::new();
    owner.set_studio(&mut session, true, MediaTime(0)).unwrap();
    owner.select_preview(&mut session, b, MediaTime(0)).unwrap();
    Fixture {
        session,
        owner,
        clock: PlaybackClock::new(),
        a,
        b,
        preset,
    }
}
fn frame(f: &mut Fixture, time: u64, pixel: [u8; 4]) -> crate::transition::TransitionSnapshot {
    let state = f.owner.sync(&mut f.session, MediaTime(time)).unwrap();
    let mut views = state.output.scenes();
    if state.studio {
        views.push(FrameScene::Live(f.session.preview()));
    }
    let playback = f
        .clock
        .update_outputs(&f.session, &views, MediaTime(time))
        .unwrap();
    let image = render_output(&f.session.snapshot(), &state.output, 4, 2, &playback).unwrap();
    assert!(
        image.pixels().chunks_exact(4).all(|actual| actual == pixel),
        "time={time}, actual={:?}, expected={pixel:?}",
        image.pixels()
    );
    state
}
fn edit(f: &mut Fixture, command: Command) {
    f.owner.before_change(&f.session).unwrap();
    f.session.apply(command).unwrap();
}

#[test]
fn configured_color_switch_chooses_one_scene_with_encoded_smoothstep_and_end_swap() {
    for (rgb, samples) in [
        (
            [255, 255, 255],
            [
                [255, 128, 128, 255],
                [255, 255, 255, 255],
                [128, 128, 255, 255],
            ],
        ),
        (
            [0, 255, 0],
            [[128, 128, 0, 255], [0, 255, 0, 255], [0, 128, 128, 255]],
        ),
    ] {
        let mut f = fixture(rgb, 50);
        f.owner.transition(&mut f.session, MediaTime(0)).unwrap();
        frame(&mut f, 0, [255, 0, 0, 255]);
        for (time, pixel) in [250_000_000, 500_000_000, 750_000_000]
            .into_iter()
            .zip(samples)
        {
            let state = frame(&mut f, time, pixel);
            assert!(state.active && state.needs_tick);
            assert!(matches!(
                state.output,
                FrameComposition::Configured {
                    kind: TransitionKind::FadeToColor { .. },
                    ..
                }
            ));
        }
        assert!(!frame(&mut f, 1_000_000_000, [0, 0, 255, 255]).active);
        assert_eq!((f.session.preview(), f.session.program()), (f.a, f.b));
    }
}

#[test]
fn zero_and_hundred_switch_points_have_finite_output_and_exact_runtime_end() {
    for (switch_point, start, half) in [
        (0, [255, 255, 255, 255], [128, 128, 255, 255]),
        (100, [255, 0, 0, 255], [255, 128, 128, 255]),
    ] {
        let mut f = fixture([255, 255, 255], switch_point);
        f.owner.transition(&mut f.session, MediaTime(0)).unwrap();
        frame(&mut f, 0, start);
        frame(&mut f, 500_000_000, half);
        assert!(!frame(&mut f, 1_000_000_000, [0, 0, 255, 255]).active);
    }
}

#[test]
fn color_quick_black_uses_transparent_endpoint_encoded_half128_and_saved_return() {
    let mut f = fixture([255, 255, 255], 50);
    let preset = f.preset;
    edit(
        &mut f,
        Command::UpdateQuickTransitionSelection {
            id: QuickTransitionId::new(3).unwrap(),
            selection: TransitionSelection::new(TransitionRef::Preset(preset), 1000).unwrap(),
            fade_to_black: true,
        },
    );
    f.owner
        .quick(
            &mut f.session,
            QuickTransitionId::new(3).unwrap(),
            MediaTime(0),
        )
        .unwrap();
    frame(&mut f, 250_000_000, [255, 128, 128, 255]);
    frame(&mut f, 500_000_000, [255, 255, 255, 255]);
    frame(&mut f, 750_000_000, [128, 128, 128, 255]);
    frame(&mut f, 1_000_000_000, [0, 0, 0, 255]);
    assert_eq!((f.session.preview(), f.session.program()), (f.b, f.b));
    f.owner
        .quick(
            &mut f.session,
            QuickTransitionId::new(3).unwrap(),
            MediaTime(1_000_000_000),
        )
        .unwrap();
    frame(&mut f, 1_250_000_000, [128, 128, 128, 255]);
    frame(&mut f, 1_500_000_000, [255, 255, 255, 255]);
    frame(&mut f, 1_750_000_000, [128, 128, 255, 255]);
    frame(&mut f, 2_000_000_000, [0, 0, 255, 255]);
}

#[test]
fn held_color_properties_refresh_and_compound_deletion_retains_last_runtime_settings() {
    let mut f = fixture([255, 255, 255], 50);
    let preset = f.preset;
    let b = f.b;
    edit(
        &mut f,
        Command::UpdateQuickTransitionSelection {
            id: QuickTransitionId::new(2).unwrap(),
            selection: TransitionSelection::new(TransitionRef::Preset(preset), 800).unwrap(),
            fade_to_black: false,
        },
    );
    edit(
        &mut f,
        Command::ConfigureSceneTransitionSelection {
            scene: b,
            reference: Some(TransitionRef::Preset(preset)),
            duration_ms: 900,
        },
    );
    f.owner
        .manual_position(&mut f.session, 512, MediaTime(0))
        .unwrap();
    let before = frame(&mut f, 10_000_000_000, [255, 255, 255, 255]);
    f.owner
        .manual_release(&mut f.session, MediaTime(10_000_000_000))
        .unwrap();
    edit(
        &mut f,
        Command::ConfigureTransitionPreset {
            id: preset,
            name: "Green quarter".into(),
            kind: kind([0, 255, 0], 25),
        },
    );
    let changed = frame(&mut f, 10_000_000_000, [0, 189, 66, 255]);
    assert_eq!(before.epoch, changed.epoch);
    assert_ne!(before.output.identity(), changed.output.identity());
    assert!(!changed.needs_tick);
    assert_eq!(changed.manual.as_ref().unwrap().current, 0.5);
    edit(
        &mut f,
        Command::RemoveTransitionPresetAndReferences { id: preset },
    );
    let retained = frame(&mut f, 10_000_000_000, [0, 189, 66, 255]);
    assert_eq!(changed.output.identity(), retained.output.identity());
    assert!(f.session.snapshot().transition_preset(preset).is_none());
    assert_eq!(
        f.session.snapshot().transition().reference(),
        TransitionRef::Fade
    );
    assert!(
        !f.session
            .snapshot()
            .quick_transitions()
            .iter()
            .any(|row| row.config().reference() == TransitionRef::Preset(preset))
    );
    assert_eq!(
        f.session.snapshot().scene(b).unwrap().transition_override(),
        None
    );
    assert_eq!(
        f.session
            .snapshot()
            .scene(b)
            .unwrap()
            .transition_duration_ms(),
        900
    );
    f.owner
        .manual_position(&mut f.session, 1023, MediaTime(10_000_000_000))
        .unwrap();
    f.owner
        .manual_release(&mut f.session, MediaTime(10_000_000_000))
        .unwrap();
    assert!(!frame(&mut f, 20_000_000_000, [0, 0, 255, 255]).active);
}

#[test]
fn invalid_configured_kind_or_percentage_cannot_admit_worker_work() {
    let f = fixture([255, 255, 255], 50);
    let mut worker = PreviewWorker::new().unwrap();
    let playback = PlaybackSnapshot::at_time(&f.session, MediaTime(0));
    for invalid in [
        TransitionKind::Fade,
        TransitionKind::Cut,
        kind([255, 255, 255], 101),
    ] {
        let output = FrameComposition::Configured {
            from: Some(FrameScene::Live(f.a)),
            to: Some(FrameScene::Live(f.b)),
            kind: invalid,
            logical_scene: f.b,
            elapsed_ns: 1,
            duration_ns: 2,
            epoch: 1,
        };
        assert!(render_output(&f.session.snapshot(), &output, 4, 2, &playback).is_err());
        assert!(
            worker
                .request_output(&f.session, 4, 2, playback.clone(), output)
                .is_err()
        );
        assert_eq!(worker.serial, 0);
        assert!(!worker.is_busy());
    }
}

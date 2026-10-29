//! Real host scene/clock/registry evaluation; all raster expectations are literal
//! pixel regions rather than calls back into the media transition algorithm.
use super::*;
use crate::{
    playback::PlaybackClock,
    transition::{Controller, TransitionSnapshot},
};
use skylive_core::{
    Command, QuickTransitionId, TransitionDirection as Dir, TransitionPresetId, TransitionRef,
    TransitionSelection,
};
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

const RED: [u8; 4] = [255, 0, 0, 255];
const BLUE: [u8; 4] = [0, 0, 255, 255];
const BLACK: [u8; 4] = [0, 0, 0, 255];
fn kinds() -> Vec<TransitionKind> {
    [Dir::Left, Dir::Right, Dir::Up, Dir::Down]
        .into_iter()
        .flat_map(|direction| {
            [
                TransitionKind::Swipe {
                    direction,
                    swipe_in: false,
                },
                TransitionKind::Swipe {
                    direction,
                    swipe_in: true,
                },
                TransitionKind::Slide { direction },
            ]
        })
        .collect()
}
fn selection(id: TransitionPresetId) -> TransitionSelection {
    TransitionSelection::new(TransitionRef::Preset(id), 1000).unwrap()
}
struct Fixture {
    session: Session,
    owner: Controller,
    clock: PlaybackClock,
    a: Id,
    b: Id,
    preset: TransitionPresetId,
}
fn fixture(kind: TransitionKind) -> Fixture {
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
            name: "A red".into(),
            rgba: RED,
        })
        .unwrap();
    let b = session
        .apply(Command::AddScene { name: "B".into() })
        .unwrap()
        .created
        .unwrap();
    session
        .apply(Command::AddColorSource {
            scene: b,
            name: "B blue".into(),
            rgba: BLUE,
        })
        .unwrap();
    let preset = session
        .apply(Command::AddTransitionPreset {
            name: "Spatial".into(),
            kind,
        })
        .unwrap()
        .created_transition_preset
        .unwrap();
    session
        .apply(Command::SelectTransition {
            selection: selection(preset),
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
fn sample(
    session: &mut Session,
    owner: &mut Controller,
    clock: &mut PlaybackClock,
    time: u64,
) -> (TransitionSnapshot, PlaybackSnapshot) {
    let state = owner.sync(session, MediaTime(time)).unwrap();
    let mut views = state.output.scenes();
    if state.studio {
        views.push(FrameScene::Live(session.preview()));
    }
    let playback = clock
        .update_outputs(session, &views, MediaTime(time))
        .unwrap();
    (state, playback)
}
fn frame(f: &mut Fixture, time: u64) -> (TransitionSnapshot, RgbaFrame) {
    let (state, playback) = sample(&mut f.session, &mut f.owner, &mut f.clock, time);
    let image = render_output(&f.session.snapshot(), &state.output, 8, 4, &playback).unwrap();
    (state, image)
}
fn pixels(image: &RgbaFrame, expected: impl Fn(u32, u32) -> [u8; 4]) {
    assert_eq!((image.width(), image.height()), (8, 4));
    for y in 0..4 {
        for x in 0..8 {
            let offset = ((y * 8 + x) * 4) as usize;
            assert_eq!(
                &image.pixels()[offset..offset + 4],
                &expected(x, y),
                "pixel({x},{y})"
            );
        }
    }
}
fn midpoint(image: &RgbaFrame, kind: TransitionKind, a: [u8; 4], b: [u8; 4]) {
    let direction = match kind {
        TransitionKind::Swipe { direction, .. } | TransitionKind::Slide { direction } => direction,
        _ => panic!("spatial fixture"),
    };
    pixels(image, |x, y| {
        if match direction {
            Dir::Left => x < 4,
            Dir::Right => x >= 4,
            Dir::Up => y < 2,
            Dir::Down => y >= 2,
        } {
            a
        } else {
            b
        }
    });
}
fn edit(f: &mut Fixture, command: Command) {
    f.owner.before_change(&f.session).unwrap();
    f.session.apply(command).unwrap();
}

#[test]
fn all_twelve_named_variants_render_quarter_midpoint_end_and_studio_swap() {
    for kind in kinds() {
        let mut f = fixture(kind);
        f.owner.transition(&mut f.session, MediaTime(0)).unwrap();
        let (_, start) = frame(&mut f, 0);
        pixels(&start, |_, _| RED);
        let (state, quarter) = frame(&mut f, 250_000_000);
        assert!(state.active);
        assert!(
            matches!(state.output,FrameComposition::Directional{kind:actual,..}if actual==kind)
        );
        // ease(1/4)=1/16. Only horizontal Swipe In has its entering texture
        // exactly on an inclusive UV boundary in this8x4 solid-scene fixture.
        pixels(&quarter, |x, _| match kind {
            TransitionKind::Swipe {
                direction: Dir::Left,
                swipe_in: true,
            } if x == 7 => BLUE,
            TransitionKind::Swipe {
                direction: Dir::Right,
                swipe_in: true,
            } if x == 0 => BLUE,
            _ => RED,
        });
        let (state, middle) = frame(&mut f, 500_000_000);
        assert!(state.active);
        midpoint(&middle, kind, RED, BLUE);
        let (state, end) = frame(&mut f, 1_000_000_000);
        assert!(!state.active);
        pixels(&end, |_, _| BLUE);
        assert_eq!((f.session.preview(), f.session.program()), (f.a, f.b));
    }
}

#[test]
fn live_preset_direction_and_provider_edits_update_keys_without_restarting_then_survive_deletion() {
    let mut f = fixture(TransitionKind::Swipe {
        direction: Dir::Left,
        swipe_in: false,
    });
    f.owner.transition(&mut f.session, MediaTime(0)).unwrap();
    let (first, image) = frame(&mut f, 500_000_000);
    midpoint(
        &image,
        TransitionKind::Slide {
            direction: Dir::Left,
        },
        RED,
        BLUE,
    );
    let preset = f.preset;
    edit(
        &mut f,
        Command::SetTransitionPresetKind {
            id: preset,
            kind: TransitionKind::Swipe {
                direction: Dir::Right,
                swipe_in: false,
            },
        },
    );
    let (second, image) = frame(&mut f, 500_000_000);
    assert_eq!(first.epoch, second.epoch);
    assert_ne!(first.output.identity(), second.output.identity());
    assert_ne!(first.output.sample_key(), second.output.sample_key());
    midpoint(
        &image,
        TransitionKind::Slide {
            direction: Dir::Right,
        },
        RED,
        BLUE,
    );
    edit(
        &mut f,
        Command::SetTransitionPresetKind {
            id: preset,
            kind: TransitionKind::Slide { direction: Dir::Up },
        },
    );
    let (third, image) = frame(&mut f, 500_000_000);
    assert_eq!(second.epoch, third.epoch);
    assert_ne!(second.output.identity(), third.output.identity());
    midpoint(
        &image,
        TransitionKind::Slide { direction: Dir::Up },
        RED,
        BLUE,
    );
    // No intervening sync: the next canonical edit must refresh the latest
    // provider metadata before removing its final authored reference.
    edit(
        &mut f,
        Command::SetTransitionPresetKind {
            id: preset,
            kind: TransitionKind::Swipe {
                direction: Dir::Down,
                swipe_in: true,
            },
        },
    );
    edit(
        &mut f,
        Command::SetTransition {
            kind: TransitionKind::Fade,
            duration_ms: 300,
        },
    );
    edit(&mut f, Command::RemoveTransitionPreset { id: preset });
    assert!(f.session.snapshot().transition_presets().is_empty());
    let (retained, image) = frame(&mut f, 500_000_000);
    assert_eq!(first.epoch, retained.epoch);
    assert!(matches!(
        retained.output,
        FrameComposition::Directional {
            kind: TransitionKind::Swipe {
                direction: Dir::Down,
                swipe_in: true
            },
            elapsed_ns: 500_000_000,
            ..
        }
    ));
    midpoint(
        &image,
        TransitionKind::Slide {
            direction: Dir::Down,
        },
        RED,
        BLUE,
    );
    let (end, image) = frame(&mut f, 1_000_000_000);
    assert!(!end.active);
    pixels(&image, |_, _| BLUE);
}

#[test]
fn manual_uses_selected_named_provider_and_held_progress_tracks_shared_property_edits() {
    let mut f = fixture(TransitionKind::Slide {
        direction: Dir::Left,
    });
    f.owner
        .manual_position(&mut f.session, 512, MediaTime(0))
        .unwrap();
    let (_, image) = frame(&mut f, 0);
    pixels(&image, |_, _| RED);
    let (held, image) = frame(&mut f, 10_000_000_000);
    midpoint(
        &image,
        TransitionKind::Slide {
            direction: Dir::Left,
        },
        RED,
        BLUE,
    );
    assert_eq!(held.manual.as_ref().unwrap().current, 0.5);
    assert!(held.active && !held.needs_tick);
    assert!(matches!(
        held.output,
        FrameComposition::Manual {
            kind: TransitionKind::Slide {
                direction: Dir::Left
            },
            progress_num: 1,
            progress_den: 2,
            ..
        }
    ));
    f.owner
        .manual_release(&mut f.session, MediaTime(10_000_000_000))
        .unwrap();
    let preset = f.preset;
    edit(
        &mut f,
        Command::SetTransitionPresetKind {
            id: preset,
            kind: TransitionKind::Swipe {
                direction: Dir::Right,
                swipe_in: true,
            },
        },
    );
    let (changed, image) = frame(&mut f, 10_000_000_000);
    assert_eq!(held.epoch, changed.epoch);
    assert_eq!(held.manual, changed.manual);
    assert_ne!(held.output.identity(), changed.output.identity());
    assert_ne!(held.output.sample_key(), changed.output.sample_key());
    midpoint(
        &image,
        TransitionKind::Slide {
            direction: Dir::Right,
        },
        RED,
        BLUE,
    );
    assert_eq!(
        f.session.snapshot().transition().reference(),
        TransitionRef::Preset(preset)
    );
}

#[test]
fn directional_null_fade_to_black_and_return_are_opaque_output_with_prepared_b() {
    for kind in kinds() {
        let mut f = fixture(kind);
        let preset = f.preset;
        edit(
            &mut f,
            Command::UpdateQuickTransitionSelection {
                id: QuickTransitionId::new(3).unwrap(),
                selection: selection(preset),
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
        let (state, image) = frame(&mut f, 500_000_000);
        assert_eq!(state.output.scenes().len(), 1);
        midpoint(&image, kind, RED, BLACK);
        let (state, image) = frame(&mut f, 1_000_000_000);
        assert!(matches!(state.output, FrameComposition::Black { .. }));
        pixels(&image, |_, _| BLACK);
        assert_eq!((f.session.preview(), f.session.program()), (f.b, f.b));
        f.owner
            .quick(
                &mut f.session,
                QuickTransitionId::new(3).unwrap(),
                MediaTime(1_000_000_000),
            )
            .unwrap();
        let (_, image) = frame(&mut f, 1_500_000_000);
        midpoint(&image, kind, BLACK, BLUE);
        let (state, image) = frame(&mut f, 2_000_000_000);
        assert!(!state.active);
        pixels(&image, |_, _| BLUE);
        assert_eq!((f.session.preview(), f.session.program()), (f.b, f.b));
    }
}

#[test]
fn invalid_directional_samples_reject_before_worker_admission_and_identity_mutation() {
    let f = fixture(TransitionKind::Slide {
        direction: Dir::Left,
    });
    let mut worker = PreviewWorker::new().unwrap();
    let playback = PlaybackSnapshot::at_time(&f.session, MediaTime(0));
    let output = |kind, elapsed_ns, duration_ns| FrameComposition::Directional {
        from: Some(FrameScene::Live(f.a)),
        to: Some(FrameScene::Live(f.b)),
        kind,
        logical_scene: f.b,
        elapsed_ns,
        duration_ns,
        epoch: 1,
    };
    for invalid in [
        output(TransitionKind::Fade, 1, 2),
        output(TransitionKind::Cut, 1, 2),
        output(
            TransitionKind::Slide {
                direction: Dir::Left,
            },
            0,
            0,
        ),
        output(
            TransitionKind::Swipe {
                direction: Dir::Up,
                swipe_in: true,
            },
            3,
            2,
        ),
    ] {
        assert_eq!(
            render_output(&f.session.snapshot(), &invalid, 8, 4, &playback),
            Err(PreviewError::Media(MediaError::InvalidTransition))
        );
        assert_eq!(
            worker.request_output(&f.session, 8, 4, playback.clone(), invalid),
            Err(PreviewError::Media(MediaError::InvalidTransition))
        );
        assert!(!worker.is_busy());
        assert_eq!(worker.serial, 0);
        assert!(worker.desired.is_none());
    }
    worker
        .request_output(
            &f.session,
            8,
            4,
            playback,
            output(
                TransitionKind::Slide {
                    direction: Dir::Left,
                },
                1,
                2,
            ),
        )
        .unwrap();
    assert_eq!(worker.serial, 1);
    worker.join().unwrap();
}

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static SERIAL: AtomicU64 = AtomicU64::new(1);
        let p = std::env::temp_dir().join(format!(
            "skylive-directional-{}-{}",
            std::process::id(),
            SERIAL.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&p).unwrap();
        Self(p)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn gif(colors: [[u8; 3]; 2]) -> Vec<u8> {
    let mut bytes = b"GIF89a".to_vec();
    bytes.extend_from_slice(&[1, 0, 1, 0, 0x80, 0, 0]);
    for color in colors {
        bytes.extend_from_slice(&color);
    }
    bytes.extend_from_slice(b"\x21\xff\x0bNETSCAPE2.0\x03\x01\x00\x00\x00");
    for data in [0x44, 0x4c] {
        bytes.extend_from_slice(&[0x21, 0xf9, 4, 0, 10, 0, 0, 0]);
        bytes.extend_from_slice(&[0x2c, 0, 0, 0, 0, 1, 0, 1, 0, 0, 2, 2, data, 1, 0]);
    }
    bytes.push(0x3b);
    bytes
}
#[test]
fn directional_frame_sampling_uses_each_gif_activation_clock_instead_of_transition_time() {
    let temp = Temp::new();
    let a_path = temp.0.join("a.gif");
    let b_path = temp.0.join("b.gif");
    fs::write(&a_path, gif([[255, 0, 0], [0, 255, 0]])).unwrap();
    fs::write(&b_path, gif([[0, 0, 255], [255, 255, 255]])).unwrap();
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
    let item_a = session
        .apply(Command::AddImageSource {
            scene: a,
            name: "A GIF".into(),
            asset: crate::assets::prepare_image(&a_path).unwrap().asset,
        })
        .unwrap()
        .created
        .unwrap();
    session
        .apply(Command::SetTransform {
            scene: a,
            item: item_a,
            transform: skylive_core::Transform::full(16, 16),
        })
        .unwrap();
    let b = session
        .apply(Command::AddScene { name: "B".into() })
        .unwrap()
        .created
        .unwrap();
    let item_b = session
        .apply(Command::AddImageSource {
            scene: b,
            name: "B GIF".into(),
            asset: crate::assets::prepare_image(&b_path).unwrap().asset,
        })
        .unwrap()
        .created
        .unwrap();
    session
        .apply(Command::SetTransform {
            scene: b,
            item: item_b,
            transform: skylive_core::Transform::full(16, 16),
        })
        .unwrap();
    let preset = session
        .apply(Command::AddTransitionPreset {
            name: "Slide".into(),
            kind: TransitionKind::Slide {
                direction: Dir::Left,
            },
        })
        .unwrap()
        .created_transition_preset
        .unwrap();
    session
        .apply(Command::SelectTransition {
            selection: selection(preset),
        })
        .unwrap();
    let mut owner = Controller::new();
    let mut clock = PlaybackClock::new();
    owner.set_studio(&mut session, true, MediaTime(0)).unwrap();
    sample(&mut session, &mut owner, &mut clock, 0);
    owner
        .select_preview(&mut session, b, MediaTime(50_000_000))
        .unwrap();
    sample(&mut session, &mut owner, &mut clock, 50_000_000);
    owner
        .transition(&mut session, MediaTime(50_000_000))
        .unwrap();
    let (state, playback) = sample(&mut session, &mut owner, &mut clock, 550_000_000);
    let image = render_output(&session.snapshot(), &state.output, 8, 4, &playback).unwrap();
    midpoint(
        &image,
        TransitionKind::Slide {
            direction: Dir::Left,
        },
        [0, 255, 0, 255],
        BLUE,
    );
    let (state, playback) = sample(&mut session, &mut owner, &mut clock, 650_000_000);
    let image = render_output(&session.snapshot(), &state.output, 8, 4, &playback).unwrap();
    pixels(&image, |x, _| if x < 2 { RED } else { [255; 4] });
}

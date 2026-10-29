use super::*;
use crate::transition::Controller;
use skylive_core::{AudioFlow, AudioSlot, AudioTarget, Command, TransitionDirection};
use skylive_media::MediaTime;
use std::sync::Arc;

fn binding() -> AudioCaptureBinding {
    AudioCaptureBinding::new(AudioFlow::Input, AudioTarget::Default, false, false).unwrap()
}
fn add(session: &mut Session, scene: Id, name: &str) -> (Id, Id) {
    let item = session
        .apply(Command::AddAudioCaptureSource {
            scene,
            name: name.into(),
            binding: binding(),
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
        .find(|i| i.id() == item)
        .unwrap()
        .source();
    (source, item)
}
fn scene(session: &mut Session, name: &str) -> Id {
    session
        .apply(Command::AddScene { name: name.into() })
        .unwrap()
        .created
        .unwrap()
}
fn request(frames: usize) -> AudioMixRequest {
    AudioMixRequest {
        sample_rate: 8000,
        channels: 1,
        frames,
        advance_ns: 0,
    }
}
fn pcm(source: Id, samples: &[f32]) -> PcmInput<'_> {
    PcmInput::Samples {
        source,
        sample_rate: 8000,
        channels: 1,
        samples,
    }
}
fn fade(from: Id, to: Id, elapsed: u64, duration: u64) -> FrameComposition {
    FrameComposition::Fade {
        from: FrameScene::Live(from),
        to: FrameScene::Live(to),
        elapsed_ns: elapsed,
        duration_ns: duration,
        epoch: 11,
    }
}
fn ftc(from: Id, to: Id) -> FrameComposition {
    FrameComposition::Configured {
        from: Some(FrameScene::Live(from)),
        to: Some(FrameScene::Live(to)),
        kind: TransitionKind::FadeToColor {
            rgb: [255, 255, 255],
            switch_point: 50,
        },
        logical_scene: to,
        elapsed_ns: 50,
        duration_ns: 100,
        epoch: 4,
    }
}

#[test]
fn separate_source_ids_on_one_endpoint_mix_separately_and_clip_only_final_sum() {
    let mut session = Session::new();
    let live = session.preview();
    let (a, _) = add(&mut session, live, "A");
    let (b, _) = add(&mut session, live, "B");
    assert_eq!(
        session.snapshot().source(a).unwrap().kind(),
        session.snapshot().source(b).unwrap().kind()
    );
    let plan =
        derive_audio_plan(&session, &FrameComposition::Scene(FrameScene::Live(live))).unwrap();
    let output = plan
        .mix(request(2), &[pcm(a, &[1.5, 1.5]), pcm(b, &[-1.25, 0.5])])
        .unwrap();
    assert_eq!(output.samples, [0.25, 1.0]);
    assert_eq!(output.clipped_samples, [1]);
    assert_eq!(output.source_meters[0].channels[0].peak, 1.5);
    assert_eq!(output.program_meters[0].peak, 1.0);
    assert_eq!(output.session, session.identity());
    assert_eq!(output.revision, session.snapshot().revision());
}

#[test]
fn fade_evaluates_every_sample_endpoint_and_advance_with_integer_timestamps() {
    let mut session = Session::new();
    let a_scene = session.preview();
    let b_scene = scene(&mut session, "B scene");
    let (a, _) = add(&mut session, a_scene, "A");
    let (b, _) = add(&mut session, b_scene, "B");
    let plan = derive_audio_plan(&session, &fade(a_scene, b_scene, 0, 250_000)).unwrap();
    let output = plan
        .mix(request(4), &[pcm(a, &[1.0; 4]), pcm(b, &[0.0; 4])])
        .unwrap();
    assert_eq!(output.samples, [1.0, 0.5, 0.0, 0.0]);
    assert_eq!(
        output.source_meters[0].channels[0].rms, 1.0,
        "source fader meter is before transition"
    );
    let output = plan
        .mix(
            AudioMixRequest {
                advance_ns: 125_000,
                ..request(2)
            },
            &[pcm(a, &[1.0; 2]), pcm(b, &[0.0; 2])],
        )
        .unwrap();
    assert_eq!(output.samples, [0.5, 0.0]);
    let r = AudioMixRequest {
        sample_rate: 44100,
        ..request(3)
    };
    let a_pcm = [1.0; 3];
    let b_pcm = [0.0; 3];
    let inputs = [
        PcmInput::Samples {
            source: a,
            sample_rate: 44100,
            channels: 1,
            samples: &a_pcm,
        },
        PcmInput::Samples {
            source: b,
            sample_rate: 44100,
            channels: 1,
            samples: &b_pcm,
        },
    ];
    let output = plan.mix(r, &inputs).unwrap();
    for (index, value) in output.samples.iter().enumerate() {
        let ns = (index as u64 * 1_000_000_000) / 44100;
        assert_eq!(*value, fade_audio_gains(ns, 250_000).unwrap().0);
    }
}

#[test]
fn shared_ids_and_duplicate_items_are_unity_roots_through_fade_to_color_dip() {
    let mut session = Session::new();
    let a_scene = session.preview();
    let b_scene = scene(&mut session, "B scene");
    let (shared, _) = add(&mut session, a_scene, "Shared");
    session
        .apply(Command::AddExistingSource {
            scene: b_scene,
            source: shared,
        })
        .unwrap();
    let (a, _) = add(&mut session, a_scene, "A");
    let (b, _) = add(&mut session, b_scene, "B");
    let plan = derive_audio_plan(&session, &ftc(a_scene, b_scene)).unwrap();
    let shared_plan = plan.sources().iter().find(|s| s.source == shared).unwrap();
    assert!(shared_plan.routes.from && shared_plan.routes.to);
    assert_eq!(shared_plan.initial_weight, 1.0);
    let output = plan
        .mix(
            request(1),
            &[pcm(shared, &[0.25]), pcm(a, &[1.0]), pcm(b, &[1.0])],
        )
        .unwrap();
    assert_eq!(output.samples, [0.25]);
    assert_eq!(
        output
            .source_meters
            .iter()
            .find(|m| m.source == a)
            .unwrap()
            .channels[0]
            .peak,
        1.0
    );
    session
        .apply(Command::AddExistingSource {
            scene: a_scene,
            source: a,
        })
        .unwrap();
    let plan = derive_audio_plan(&session, &ftc(a_scene, b_scene)).unwrap();
    assert!(
        plan.sources()
            .iter()
            .find(|s| s.source == a)
            .unwrap()
            .routes
            .duplicated
    );
    assert_eq!(
        plan.mix(
            request(1),
            &[pcm(shared, &[0.25]), pcm(a, &[0.5]), pcm(b, &[1.0])]
        )
        .unwrap()
        .samples,
        [0.75]
    );
}

#[test]
fn global_roots_override_scene_fades_once_and_survive_black_but_preview_is_excluded() {
    let mut session = Session::new();
    let a_scene = session.preview();
    let b_scene = scene(&mut session, "Preview");
    let (a, _) = add(&mut session, a_scene, "A");
    let (preview, _) = add(&mut session, b_scene, "Preview only");
    session
        .apply(Command::SetGlobalAudio {
            slot: AudioSlot::Mic1,
            source: Some(a),
        })
        .unwrap();
    session
        .apply(Command::SetGlobalAudio {
            slot: AudioSlot::Mic2,
            source: Some(a),
        })
        .unwrap();
    session.select_preview(b_scene).unwrap();
    let plan = derive_audio_plan(
        &session,
        &FrameComposition::Scene(FrameScene::Live(a_scene)),
    )
    .unwrap();
    assert_eq!(plan.sources().len(), 1);
    assert_eq!(plan.sources()[0].source, a);
    assert!(plan.sources()[0].routes.global);
    assert_eq!(
        plan.mix(request(1), &[pcm(a, &[0.5])]).unwrap().samples,
        [0.5]
    );
    assert_eq!(
        plan.mix(request(1), &[pcm(a, &[0.5]), pcm(preview, &[1.0])]),
        Err(AudioError::UnexpectedInput(preview))
    );
    let black = derive_audio_plan(
        &session,
        &FrameComposition::Black {
            logical_scene: b_scene,
            epoch: 9,
        },
    )
    .unwrap();
    assert_eq!(black.sources().len(), 1);
    assert_eq!(
        black.mix(request(1), &[pcm(a, &[0.5])]).unwrap().samples,
        [0.5]
    );
    let transition = derive_audio_plan(&session, &fade(a_scene, b_scene, 50, 100)).unwrap();
    assert_eq!(
        transition
            .sources()
            .iter()
            .find(|s| s.source == a)
            .unwrap()
            .initial_weight,
        1.0
    );
}

#[test]
fn hidden_items_are_excluded_and_scene_alias_uses_visual_source_not_logical_selection() {
    let mut session = Session::new();
    let a_scene = session.preview();
    let b_scene = scene(&mut session, "Logical");
    let (a, item) = add(&mut session, a_scene, "A");
    let (_b, _) = add(&mut session, b_scene, "B");
    let alias = FrameComposition::SceneAlias {
        scene: FrameScene::Live(a_scene),
        logical_scene: b_scene,
        epoch: 1,
    };
    assert_eq!(
        derive_audio_plan(&session, &alias).unwrap().sources()[0].source,
        a
    );
    session
        .apply(Command::SetVisible {
            scene: a_scene,
            item,
            visible: false,
        })
        .unwrap();
    let plan = derive_audio_plan(&session, &alias).unwrap();
    assert!(plan.sources().is_empty());
    assert_eq!(plan.mix(request(2), &[]).unwrap().samples, [0.0, 0.0]);
}

#[test]
fn single_null_fades_and_manual_directional_progress_match_media_factors() {
    let mut session = Session::new();
    let live = session.preview();
    let (source, _) = add(&mut session, live, "A");
    for (incoming, expected) in [(false, 0.75), (true, 0.25)] {
        let output = FrameComposition::SingleFade {
            scene: FrameScene::Live(live),
            logical_scene: live,
            incoming,
            elapsed_ns: 1,
            duration_ns: 4,
            epoch: 7,
        };
        assert_eq!(
            derive_audio_plan(&session, &output)
                .unwrap()
                .mix(request(1), &[pcm(source, &[1.0])])
                .unwrap()
                .samples,
            [expected]
        );
    }
    let manual = FrameComposition::Manual {
        from: Some(FrameScene::Live(live)),
        to: None,
        kind: TransitionKind::Slide {
            direction: TransitionDirection::Left,
        },
        logical_scene: live,
        progress_num: 1,
        progress_den: 3,
        epoch: 9,
        next_after_ns: None,
    };
    let plan = derive_audio_plan(&session, &manual).unwrap();
    assert_eq!(
        plan.mix(
            AudioMixRequest {
                advance_ns: 1_000_000,
                ..request(3)
            },
            &[pcm(source, &[1.0; 3])]
        )
        .unwrap()
        .samples,
        [directional_audio_gains(1, 3).unwrap().0; 3]
    );
    let timed = FrameComposition::Directional {
        from: Some(FrameScene::Live(live)),
        to: None,
        kind: TransitionKind::Swipe {
            direction: TransitionDirection::Up,
            swipe_in: true,
        },
        logical_scene: live,
        elapsed_ns: 0,
        duration_ns: 500_000,
        epoch: 10,
    };
    let output = derive_audio_plan(&session, &timed)
        .unwrap()
        .mix(request(3), &[pcm(source, &[1.0; 3])])
        .unwrap();
    for (index, sample) in output.samples.iter().enumerate() {
        assert_eq!(
            *sample,
            directional_audio_gains(index as u64 * 125_000, 500_000)
                .unwrap()
                .0
        );
    }
}

#[test]
fn gain_mute_and_independent_channel_meters_use_pretransition_source_values() {
    let mut session = Session::new();
    let live = session.preview();
    let (source, _) = add(&mut session, live, "A");
    session
        .apply(Command::SetAudio {
            source,
            gain: 2.0,
            muted: false,
        })
        .unwrap();
    let plan = derive_audio_plan(
        &session,
        &FrameComposition::SingleFade {
            scene: FrameScene::Live(live),
            logical_scene: live,
            incoming: false,
            elapsed_ns: 1,
            duration_ns: 2,
            epoch: 1,
        },
    )
    .unwrap();
    let samples = [0.5, -0.25, -0.5, 0.75];
    let r = AudioMixRequest {
        channels: 2,
        ..request(2)
    };
    let output = plan
        .mix(
            r,
            &[PcmInput::Samples {
                source,
                sample_rate: 8000,
                channels: 2,
                samples: &samples,
            }],
        )
        .unwrap();
    assert_eq!(output.samples, [0.5, -0.25, 0.0, 0.0]);
    assert_eq!(
        output.source_meters[0].channels[0],
        ChannelMeter {
            peak: 1.0,
            rms: 1.0
        }
    );
    assert_eq!(output.source_meters[0].channels[1].peak, 1.5);
    assert_eq!(output.program_meters[0].peak, 0.5);
    assert_eq!(output.program_meters[1].peak, 0.25);
    session
        .apply(Command::SetAudio {
            source,
            gain: 2.0,
            muted: true,
        })
        .unwrap();
    let muted =
        derive_audio_plan(&session, &FrameComposition::Scene(FrameScene::Live(live))).unwrap();
    let output = muted.mix(request(1), &[pcm(source, &[f32::MAX])]).unwrap();
    assert_eq!(output.samples, [0.0]);
    assert_eq!(output.source_meters[0].channels[0], ChannelMeter::default());
}

#[test]
fn missing_duplicate_unrelated_format_and_buffer_inputs_are_explicit_errors() {
    let mut session = Session::new();
    let live = session.preview();
    let (source, _) = add(&mut session, live, "A");
    let plan =
        derive_audio_plan(&session, &FrameComposition::Scene(FrameScene::Live(live))).unwrap();
    assert_eq!(
        plan.mix(request(1), &[]),
        Err(AudioError::MissingInput(source))
    );
    assert_eq!(
        plan.mix(request(1), &[pcm(source, &[0.0]), pcm(source, &[0.0])]),
        Err(AudioError::DuplicateInput(source))
    );
    assert_eq!(
        plan.mix(
            request(1),
            &[PcmInput::Silence {
                source: Id::new(u64::MAX)
            }]
        ),
        Err(AudioError::UnexpectedInput(Id::new(u64::MAX)))
    );
    assert_eq!(
        plan.mix(
            request(1),
            &[PcmInput::Samples {
                source,
                sample_rate: 48000,
                channels: 1,
                samples: &[0.0]
            }]
        ),
        Err(AudioError::FormatMismatch(source))
    );
    assert_eq!(
        plan.mix(
            request(1),
            &[PcmInput::Samples {
                source,
                sample_rate: 8000,
                channels: 2,
                samples: &[0.0]
            }]
        ),
        Err(AudioError::FormatMismatch(source))
    );
    assert_eq!(
        plan.mix(request(1), &[pcm(source, &[])]),
        Err(AudioError::InvalidBuffer(source))
    );
    assert_eq!(
        plan.mix(request(1), &[PcmInput::Silence { source }])
            .unwrap()
            .samples,
        [0.0]
    );
}

#[test]
fn all_pcm_is_finite_validated_even_when_muted_or_zero_weight_and_extremes_stay_finite() {
    let mut session = Session::new();
    let live = session.preview();
    let (source, _) = add(&mut session, live, "A");
    session
        .apply(Command::SetAudio {
            source,
            gain: 2.0,
            muted: true,
        })
        .unwrap();
    let plan =
        derive_audio_plan(&session, &FrameComposition::Scene(FrameScene::Live(live))).unwrap();
    for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        assert_eq!(
            plan.mix(request(1), &[pcm(source, &[bad])]),
            Err(AudioError::NonFiniteSample { source, index: 0 })
        );
    }
    session
        .apply(Command::SetAudio {
            source,
            gain: 2.0,
            muted: false,
        })
        .unwrap();
    let zero = FrameComposition::SingleFade {
        scene: FrameScene::Live(live),
        logical_scene: live,
        incoming: true,
        elapsed_ns: 0,
        duration_ns: 2,
        epoch: 1,
    };
    assert_eq!(
        derive_audio_plan(&session, &zero)
            .unwrap()
            .mix(request(1), &[pcm(source, &[f32::NAN])]),
        Err(AudioError::NonFiniteSample { source, index: 0 })
    );
    let plan =
        derive_audio_plan(&session, &FrameComposition::Scene(FrameScene::Live(live))).unwrap();
    let output = plan
        .mix(request(2), &[pcm(source, &[f32::MAX, -f32::MAX])])
        .unwrap();
    assert_eq!(output.samples, [1.0, -1.0]);
    assert_eq!(output.clipped_samples, [2]);
    assert!(output.source_meters[0].channels[0].peak.is_finite());
    assert!(output.source_meters[0].channels[0].rms.is_finite());
    serde_json::to_vec(&output).unwrap();
}

#[test]
fn request_count_shape_byte_and_time_bounds_reject_before_output_or_state_changes() {
    let mut session = Session::new();
    let live = session.preview();
    let (source, _) = add(&mut session, live, "A");
    let plan =
        derive_audio_plan(&session, &FrameComposition::Scene(FrameScene::Live(live))).unwrap();
    let before = session.snapshot();
    for r in [
        AudioMixRequest {
            sample_rate: 7999,
            ..request(1)
        },
        AudioMixRequest {
            sample_rate: 192001,
            ..request(1)
        },
        AudioMixRequest {
            channels: 0,
            ..request(1)
        },
        AudioMixRequest {
            channels: 9,
            ..request(1)
        },
        request(0),
        request(4097),
    ] {
        assert!(matches!(
            plan.mix(r, &[PcmInput::Silence { source }]),
            Err(AudioError::InvalidRequest(_))
        ));
    }
    assert!(matches!(
        plan.mix(request(1), &vec![PcmInput::Silence { source }; 65]),
        Err(AudioError::Limit(_))
    ));
    assert!(matches!(
        plan.mix_with_limits(
            request(1),
            &[PcmInput::Silence { source }],
            AudioMixLimits { max_total_bytes: 1 }
        ),
        Err(AudioError::Limit(_))
    ));
    assert_eq!(
        plan.mix(
            AudioMixRequest {
                advance_ns: u64::MAX,
                ..request(2)
            },
            &[PcmInput::Silence { source }]
        ),
        Err(AudioError::TimeOverflow)
    );
    let fade_plan = derive_audio_plan(
        &session,
        &FrameComposition::SingleFade {
            scene: FrameScene::Live(live),
            logical_scene: live,
            incoming: false,
            elapsed_ns: 1,
            duration_ns: 2,
            epoch: 0,
        },
    )
    .unwrap();
    assert_eq!(
        fade_plan.mix(
            AudioMixRequest {
                advance_ns: u64::MAX,
                ..request(1)
            },
            &[PcmInput::Silence { source }]
        ),
        Err(AudioError::TimeOverflow)
    );
    assert!(Arc::ptr_eq(&before, &session.snapshot()));
}

#[test]
fn plan_source_limit_is_checked_before_gathering_the_sixty_fifth_source() {
    let mut session = Session::new();
    let live = session.preview();
    for n in 0..65 {
        add(&mut session, live, &format!("Mic {n}"));
    }
    assert!(matches!(
        derive_audio_plan(&session, &FrameComposition::Scene(FrameScene::Live(live))),
        Err(AudioError::Limit(_))
    ));
    let last = session.snapshot().scene(live).unwrap().items()[64].id();
    session
        .apply(Command::SetVisible {
            scene: live,
            item: last,
            visible: false,
        })
        .unwrap();
    assert_eq!(
        derive_audio_plan(&session, &FrameComposition::Scene(FrameScene::Live(live)))
            .unwrap()
            .sources()
            .len(),
        64
    );
}

#[test]
fn invalid_composition_scalars_and_foreign_closed_sessions_are_rejected() {
    let mut session = Session::new();
    let live = session.preview();
    add(&mut session, live, "A");
    for output in [
        fade(live, live, 3, 2),
        fade(live, live, 0, 0),
        FrameComposition::Manual {
            from: None,
            to: None,
            logical_scene: live,
            kind: TransitionKind::Fade,
            progress_num: 2,
            progress_den: 1,
            epoch: 0,
            next_after_ns: None,
        },
        FrameComposition::Manual {
            from: None,
            to: None,
            logical_scene: live,
            kind: TransitionKind::Cut,
            progress_num: 0,
            progress_den: 1,
            epoch: 0,
            next_after_ns: None,
        },
    ] {
        assert!(matches!(
            derive_audio_plan(&session, &output),
            Err(AudioError::InvalidTransition)
        ));
    }
    let mut controller = Controller::new();
    controller
        .set_studio(&mut session, true, MediaTime(0))
        .unwrap();
    let frozen = controller.sync(&mut session, MediaTime(0)).unwrap().output;
    assert!(matches!(
        derive_audio_plan(&Session::new(), &frozen),
        Err(AudioError::MissingScene(_))
            | Err(AudioError::Transition(
                TransitionError::ForeignSession { .. }
            ))
    ));
    session.close();
    assert!(matches!(
        derive_audio_plan(&session, &frozen),
        Err(AudioError::Closed)
    ));
}

#[test]
fn retained_plan_is_owned_at_its_revision_and_frozen_layout_keeps_retired_audio() {
    let mut session = Session::new();
    let live = session.preview();
    let (source, item) = add(&mut session, live, "A");
    let plan =
        derive_audio_plan(&session, &FrameComposition::Scene(FrameScene::Live(live))).unwrap();
    let revision = plan.revision();
    session
        .apply(Command::SetAudio {
            source,
            gain: 0.5,
            muted: false,
        })
        .unwrap();
    assert_eq!(
        plan.mix(request(1), &[pcm(source, &[0.5])])
            .unwrap()
            .samples,
        [0.5]
    );
    assert_eq!(plan.revision(), revision);
    assert_ne!(revision, session.snapshot().revision());
    let mut controller = Controller::new();
    controller
        .set_studio(&mut session, true, MediaTime(0))
        .unwrap();
    controller.before_change(&session).unwrap();
    session
        .apply(Command::RemoveItem { scene: live, item })
        .unwrap();
    assert!(session.snapshot().source(source).is_none());
    let frozen = controller.sync(&mut session, MediaTime(0)).unwrap().output;
    let retained = derive_audio_plan(&session, &frozen).unwrap();
    assert_eq!(retained.sources()[0].source, source);
    assert_eq!(retained.sources()[0].gain, 0.5);
    assert_eq!(
        retained
            .mix(request(1), &[pcm(source, &[0.5])])
            .unwrap()
            .samples,
        [0.25]
    );
}

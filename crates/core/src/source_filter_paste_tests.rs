use crate::*;
use std::sync::Arc;
fn media(s: &mut Session, name: &str, video: bool, audio: bool) -> Id {
    s.apply(Command::AddMediaSource {
        scene: s.preview(),
        name: name.into(),
        media: MediaSource::new(
            format!("{name}.mp4"),
            if video { 16 } else { 0 },
            if video { 16 } else { 0 },
            audio,
        )
        .unwrap(),
    })
    .unwrap();
    s.snapshot().sources().last().unwrap().id()
}
fn video(name: &str) -> VideoFilter {
    VideoFilter::new(VideoEffect::color_correction())
        .unwrap()
        .with_name(name.into())
        .unwrap()
}
fn assert_undo_content(before: &StudioSnapshot, pasted: &StudioSnapshot, actual: &StudioSnapshot) {
    let mut expected = before.collection().clone();
    expected.next_id = pasted.collection().next_id;
    assert_eq!(
        actual.collection(),
        &expected,
        "Undo restores all authored content while retaining issued-ID high-water"
    );
}
#[test]
fn both_chains_prepend_atomically_with_reverse_name_allocation_fresh_ids_one_undo_and_redo() {
    let mut s = Session::new();
    let from = media(&mut s, "A", true, true);
    let to = media(&mut s, "B", true, true);
    let mut disabled = video("Foo");
    disabled.enabled = false;
    s.apply(Command::SetSourceVideoFilters {
        source: from,
        filters: vec![disabled, video("Foo 2")],
    })
    .unwrap();
    s.apply(Command::SetSourceAudioFilters {
        source: from,
        filters: vec![AudioFilter::obs_gain()],
    })
    .unwrap();
    s.apply(Command::SetSourceVideoFilters {
        source: to,
        filters: vec![video("Foo")],
    })
    .unwrap();
    s.apply(Command::SetSourceAudioFilters {
        source: to,
        filters: vec![AudioFilter::obs_polarity_invert()],
    })
    .unwrap();
    let before = s.snapshot();
    let old_id = before.source(to).unwrap().video_filters()[0].id();
    let src_ids: Vec<_> = before
        .source(from)
        .unwrap()
        .video_filters()
        .iter()
        .map(|f| f.id())
        .collect();
    let changed = s.apply(Command::PasteSourceFilters { from, to }).unwrap();
    assert!(changed.changed);
    assert_eq!(changed.revision, before.revision() + 1);
    let after = s.snapshot();
    let filters = after.source(to).unwrap().video_filters();
    assert_eq!(
        filters.iter().map(|f| f.name()).collect::<Vec<_>>(),
        ["Foo 3", "Foo 2", "Foo"]
    );
    assert!(!filters[0].enabled);
    assert_eq!(filters[2].id(), old_id);
    assert!(
        filters[..2]
            .iter()
            .all(|f| !src_ids.contains(&f.id()) && f.id() != old_id)
    );
    assert_ne!(filters[0].id(), filters[1].id());
    assert_eq!(
        after.source(to).unwrap().audio_filters(),
        [AudioFilter::obs_gain(), AudioFilter::obs_polarity_invert()]
    );
    assert_eq!(after.source(from).unwrap(), before.source(from).unwrap());
    s.apply(Command::Undo).unwrap();
    assert_undo_content(&before, &after, &s.snapshot());
    s.apply(Command::Redo).unwrap();
    assert_eq!(s.snapshot().collection(), after.collection());
}
#[test]
fn incompatible_paste_rejects_both_chains_and_preserves_snapshot_history_and_ids() {
    let mut s = Session::new();
    let from = media(&mut s, "AV", true, true);
    let to = media(&mut s, "Video", true, false);
    s.apply(Command::SetSourceVideoFilters {
        source: from,
        filters: vec![video("V")],
    })
    .unwrap();
    s.apply(Command::SetSourceAudioFilters {
        source: from,
        filters: vec![AudioFilter::obs_gain()],
    })
    .unwrap();
    let before = s.snapshot();
    let high = s.high_water;
    assert!(s.apply(Command::PasteSourceFilters { from, to }).is_err());
    assert!(Arc::ptr_eq(&before, &s.snapshot()));
    assert_eq!(s.high_water, high);
    let audio_only = media(&mut s, "Audio", false, true);
    let before = s.snapshot();
    assert!(
        s.apply(Command::PasteSourceFilters {
            from,
            to: audio_only
        })
        .is_err()
    );
    assert!(Arc::ptr_eq(&before, &s.snapshot()));
}
#[test]
fn every_new_provider_is_checked_even_bypassed_and_existing_disabled_filters_are_retained() {
    let mut s = Session::new();
    let from = media(&mut s, "A", true, true);
    let to = media(&mut s, "B", true, true);
    let mut effect = VideoFilter::new(VideoEffect::brightness_contrast()).unwrap();
    effect.enabled = false;
    s.apply(Command::SetSourceVideoFilters {
        source: from,
        filters: vec![effect.clone()],
    })
    .unwrap();
    s.apply(Command::SetSourceVideoFilters {
        source: to,
        filters: vec![effect],
    })
    .unwrap();
    s.apply(Command::SetPluginEnabled {
        plugin: CRAFT_VIDEO_PLUGIN.into(),
        enabled: false,
    })
    .unwrap();
    let before = s.snapshot();
    assert!(matches!(
        s.apply(Command::PasteSourceFilters { from, to }),
        Err(Error::Unavailable(_))
    ));
    assert!(Arc::ptr_eq(&before, &s.snapshot()));
    assert!(
        !s.apply(Command::PasteSourceFilters { from: to, to })
            .unwrap()
            .changed
    );
    s.apply(Command::SetSourceVideoFilters {
        source: from,
        filters: vec![video("Native")],
    })
    .unwrap();
    s.apply(Command::PasteSourceFilters { from, to }).unwrap();
    assert_eq!(s.snapshot().source(to).unwrap().video_filters().len(), 2);
}
#[test]
fn total_chain_cap_and_name_overflow_reject_before_any_model_or_history_publication() {
    let mut s = Session::new();
    let from = media(&mut s, "A", true, true);
    let to = media(&mut s, "B", true, true);
    s.apply(Command::SetSourceVideoFilters {
        source: from,
        filters: vec![video("V")],
    })
    .unwrap();
    s.apply(Command::SetSourceVideoFilters {
        source: to,
        filters: (0..8).map(|n| video(&format!("F{n}"))).collect(),
    })
    .unwrap();
    let before = s.snapshot();
    assert!(s.apply(Command::PasteSourceFilters { from, to }).is_err());
    assert!(Arc::ptr_eq(&before, &s.snapshot()));
    let name = "я".repeat(128);
    s.apply(Command::SetSourceVideoFilters {
        source: from,
        filters: vec![video(&name)],
    })
    .unwrap();
    s.apply(Command::SetSourceVideoFilters {
        source: to,
        filters: vec![video(&name)],
    })
    .unwrap();
    let before = s.snapshot();
    assert!(s.apply(Command::PasteSourceFilters { from, to }).is_err());
    assert!(Arc::ptr_eq(&before, &s.snapshot()));
}
#[test]
fn empty_source_and_self_paste_are_noops_and_stale_revision_is_rejected() {
    let mut s = Session::new();
    let from = media(&mut s, "A", true, true);
    let to = media(&mut s, "B", true, true);
    s.apply(Command::SetSourceVideoFilters {
        source: to,
        filters: vec![video("Keep")],
    })
    .unwrap();
    let before = s.snapshot();
    assert!(
        !s.apply(Command::PasteSourceFilters { from, to })
            .unwrap()
            .changed
    );
    assert!(
        !s.apply(Command::PasteSourceFilters { from: to, to })
            .unwrap()
            .changed
    );
    assert!(Arc::ptr_eq(&before, &s.snapshot()));
    assert!(matches!(
        s.apply_at(
            Some(before.revision() - 1),
            Command::PasteSourceFilters { from, to }
        ),
        Err(Error::Conflict { .. })
    ));
    assert!(Arc::ptr_eq(&before, &s.snapshot()));
}

#[test]
fn visual_scene_and_group_owners_receive_new_filters() {
    let mut s = Session::new();
    let from = media(&mut s, "Visual", true, false);
    s.apply(Command::SetSourceVideoFilters {
        source: from,
        filters: vec![video("Shared visual effect")],
    })
    .unwrap();
    let target_scene = s
        .apply(Command::AddScene {
            name: "Target scene".into(),
        })
        .unwrap()
        .created
        .unwrap();
    s.apply(Command::SetSceneVideoFilters {
        scene: target_scene,
        filters: vec![video("Existing scene effect")],
    })
    .unwrap();
    let scene_source = s.snapshot().scene_source(target_scene).unwrap().id();
    s.apply(Command::PasteSourceFilters {
        from,
        to: scene_source,
    })
    .unwrap();
    assert_eq!(
        s.snapshot()
            .scene_source(target_scene)
            .unwrap()
            .video_filters()
            .len(),
        2
    );
    let group_item = s
        .apply(Command::AddGroup {
            scene: s.preview(),
            name: "Group".into(),
        })
        .unwrap()
        .created
        .unwrap();
    let group_source = s.snapshot().item_container(group_item).unwrap().1.source();
    s.apply(Command::PasteSourceFilters {
        from,
        to: group_source,
    })
    .unwrap();
    assert_eq!(
        s.snapshot().source(group_source).unwrap().video_filters()[0].name(),
        "Shared visual effect"
    );
}

#[test]
fn source11_optional_video_activation_cannot_be_bypassed_by_pasting_disabled_filters() {
    for (provider, effect) in [
        (
            PERSON_BACKGROUND_PLUGIN,
            VideoEffect::PersonBackgroundRemoval {
                threshold: 0.71,
                softness: 0.24,
            },
        ),
        (
            CRAFT_VIDEO_PLUGIN,
            VideoEffect::BrightnessContrast {
                brightness: 23.0,
                contrast: -7.0,
                legacy: false,
            },
        ),
    ] {
        for bypassed in [false, true] {
            let mut s = Session::new();
            let from = media(&mut s, "Optional source", true, true);
            let to = media(&mut s, "Optional destination", true, true);
            s.apply(Command::SetPluginEnabled {
                plugin: provider.into(),
                enabled: false,
            })
            .unwrap();
            let mut copied = VideoFilter::new(effect)
                .unwrap()
                .with_name("Copied optional effect".into())
                .unwrap();
            copied.enabled = !bypassed;
            let before = s.snapshot();
            assert!(matches!(
                s.apply(Command::SetSourceVideoFilters {
                    source: from,
                    filters: vec![copied.clone()]
                }),
                Err(Error::Unavailable(_))
            ));
            assert!(
                Arc::ptr_eq(&before, &s.snapshot()),
                "enable before authoring, even bypassed"
            );
            s.apply(Command::SetPluginEnabled {
                plugin: provider.into(),
                enabled: true,
            })
            .unwrap();
            s.apply(Command::SetSourceVideoFilters {
                source: from,
                filters: vec![copied],
            })
            .unwrap();
            s.apply(Command::SetSourceAudioFilters {
                source: from,
                filters: vec![AudioFilter::obs_gain()],
            })
            .unwrap();
            s.apply(Command::SetSourceVideoFilters {
                source: to,
                filters: vec![video("Existing builtin")],
            })
            .unwrap();
            let original = s.snapshot().source(from).unwrap().video_filters()[0].clone();
            s.apply(Command::SetPluginEnabled {
                plugin: provider.into(),
                enabled: false,
            })
            .unwrap();
            let before = s.snapshot();
            let high = s.high_water;
            assert!(matches!(
                before.check_filter_paste(from, to),
                Err(Error::Unavailable(_))
            ));
            assert!(matches!(
                s.apply(Command::PasteSourceFilters { from, to }),
                Err(Error::Unavailable(_))
            ));
            assert!(Arc::ptr_eq(&before, &s.snapshot()));
            assert_eq!(s.high_water, high);
            assert!(
                s.snapshot().source(to).unwrap().audio_filters().is_empty(),
                "reject both chains atomically"
            );
            s.apply(Command::SetPluginEnabled {
                plugin: provider.into(),
                enabled: true,
            })
            .unwrap();
            let before = s.snapshot();
            s.apply(Command::PasteSourceFilters { from, to }).unwrap();
            let after = s.snapshot();
            let copied = &after.source(to).unwrap().video_filters()[0];
            assert_eq!(copied.effect, original.effect);
            assert_eq!(copied.enabled, original.enabled);
            assert_ne!(copied.id(), original.id());
            assert_eq!(
                after.source(to).unwrap().audio_filters(),
                [AudioFilter::obs_gain()]
            );
            assert_eq!(after.revision(), before.revision() + 1);
            s.apply(Command::Undo).unwrap();
            assert_undo_content(&before, &after, &s.snapshot());
            s.apply(Command::Redo).unwrap();
            assert_eq!(s.snapshot().collection(), after.collection());
        }
    }
}

#[test]
fn source11_disabled_nn_destination_is_retained_while_new_builtin_filters_are_pasted() {
    let mut s = Session::new();
    let from = media(&mut s, "Builtin source", true, false);
    let to = media(&mut s, "Neural destination", true, false);
    s.apply(Command::SetSourceVideoFilters {
        source: from,
        filters: vec![video("Builtin")],
    })
    .unwrap();
    s.apply(Command::SetPluginEnabled {
        plugin: PERSON_BACKGROUND_PLUGIN.into(),
        enabled: true,
    })
    .unwrap();
    let mut neural = VideoFilter::new(VideoEffect::PersonBackgroundRemoval {
        threshold: 0.73,
        softness: 0.19,
    })
    .unwrap()
    .with_name("Retained neural".into())
    .unwrap();
    neural.enabled = false;
    s.apply(Command::SetSourceVideoFilters {
        source: to,
        filters: vec![neural],
    })
    .unwrap();
    let retained = s.snapshot().source(to).unwrap().video_filters()[0].clone();
    s.apply(Command::SetPluginEnabled {
        plugin: PERSON_BACKGROUND_PLUGIN.into(),
        enabled: false,
    })
    .unwrap();
    s.apply(Command::PasteSourceFilters { from, to }).unwrap();
    let model = s.snapshot();
    assert_eq!(model.source(to).unwrap().video_filters().len(), 2);
    assert_eq!(model.source(to).unwrap().video_filters()[1], retained);
    assert!(!model.preferences().plugin_enabled(PERSON_BACKGROUND_PLUGIN));
    let before = s.snapshot();
    assert!(matches!(
        s.apply(Command::PasteSourceFilters { from: to, to: from }),
        Err(Error::Unavailable(_))
    ));
    assert!(
        Arc::ptr_eq(&before, &s.snapshot()),
        "retained bypassed NN may not become a new copy elsewhere"
    );
}

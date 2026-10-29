use super::*;
use crate::views::dialogs::{self, Dialog};
use skylive_core::{
    PERSON_BACKGROUND_PLUGIN, Session, VideoCaptureBinding, VideoCaptureMode, VideoCaptureTarget,
    VideoPixelFormat,
};

fn fixture() -> (Session, Id) {
    let mut owner = Session::new();
    let scene = owner.preview();
    owner
        .apply(Command::AddVideoCaptureSource {
            scene,
            name: "Person camera".into(),
            binding: VideoCaptureBinding::new(
                VideoCaptureTarget::Windows {
                    symbolic_link: "inert-neural-ui-camera".into(),
                },
                VideoCaptureMode::new(16, 16, 30, 1, VideoPixelFormat::Rgb32).unwrap(),
                false,
            )
            .unwrap(),
        })
        .unwrap();
    let source = owner.snapshot().sources()[0].id();
    (owner, source)
}
fn slot(owner: &Session, source: Id) -> Option<Dialog> {
    Some(Dialog::VideoFilters(
        VideoFilterDraft::capture(owner.identity(), &owner.snapshot(), source).unwrap(),
    ))
}
fn frame(
    ctx: &egui::Context,
    owner: &Session,
    slot: &mut Option<Dialog>,
    events: Vec<egui::Event>,
) -> (egui::FullOutput, Vec<Intent>) {
    crate::i18n::install(
        ctx,
        crate::i18n::english().unwrap(),
        std::path::Path::new("headless-translations.csv"),
    );
    let mut intents = vec![];
    let out = ctx.run(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1000.0, 800.0),
            )),
            events,
            ..Default::default()
        },
        |ctx| dialogs::show(ctx, slot, &mut intents, true, &owner.snapshot()),
    );
    (out, intents)
}
fn point(shape: &egui::epaint::Shape, label: &str) -> Option<egui::Pos2> {
    match shape {
        egui::epaint::Shape::Text(text) if text.galley.text() == label => {
            Some(text.pos + text.galley.size() * 0.5)
        }
        egui::epaint::Shape::Vec(shapes) => shapes.iter().find_map(|shape| point(shape, label)),
        _ => None,
    }
}
fn click(
    ctx: &egui::Context,
    owner: &Session,
    slot: &mut Option<Dialog>,
    label: &str,
) -> Vec<Intent> {
    frame(ctx, owner, slot, vec![]);
    let (out, _) = frame(ctx, owner, slot, vec![]);
    let p = out
        .shapes
        .iter()
        .rev()
        .find_map(|shape| point(&shape.shape, label).filter(|p| shape.clip_rect.contains(*p)))
        .unwrap_or_else(|| panic!("visible neural control {label}"));
    frame(
        ctx,
        owner,
        slot,
        vec![
            egui::Event::PointerMoved(p),
            egui::Event::PointerButton {
                pos: p,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
        ],
    );
    frame(
        ctx,
        owner,
        slot,
        vec![
            egui::Event::PointerMoved(p),
            egui::Event::PointerButton {
                pos: p,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            },
        ],
    )
    .1
}
fn draft(slot: &mut Option<Dialog>) -> &mut VideoFilterDraft {
    match slot {
        Some(Dialog::VideoFilters(draft)) => draft,
        _ => panic!("filter draft"),
    }
}
#[test]
fn neural_add_is_visible_in_own_group_default_off_while_craft_still_works() {
    let (owner, source) = fixture();
    let ctx = egui::Context::default();
    crate::theme::install(&ctx);
    let mut dialog = slot(&owner, source);
    let before = owner.snapshot();
    click(&ctx, &owner, &mut dialog, "Add Filter");
    let (out, _) = frame(&ctx, &owner, &mut dialog, vec![]);
    assert!(
        out.shapes
            .iter()
            .any(|shape| point(&shape.shape, "Neural filters · disabled").is_some())
    );
    click(&ctx, &owner, &mut dialog, "AI Background Removal (Person)");
    assert!(draft(&mut dialog).rows.is_empty());
    // Exercise the enabled Craft choice in a fresh editor; disabled menu
    // dismissal is independent of provider activation and deferred by egui.
    let ctx = egui::Context::default();
    crate::theme::install(&ctx);
    let mut dialog = slot(&owner, source);
    click(&ctx, &owner, &mut dialog, "Add Filter");
    click(&ctx, &owner, &mut dialog, "Brightness & Contrast");
    assert!(
        matches!(draft(&mut dialog).rows.as_slice(),[row]if matches!(row.filter.effect,VideoEffect::BrightnessContrast{..}))
    );
    click(&ctx, &owner, &mut dialog, "Cancel");
    assert!(dialog.is_none());
    assert!(std::sync::Arc::ptr_eq(&before, &owner.snapshot()));
}
#[test]
fn real_person_camera_editor_parameters_reset_validate_and_apply_one_fenced_edit() {
    let (mut owner, source) = fixture();
    owner
        .apply(Command::SetPluginEnabled {
            plugin: PERSON_BACKGROUND_PLUGIN.into(),
            enabled: true,
        })
        .unwrap();
    owner
        .apply(Command::SetPluginEnabled {
            plugin: skylive_core::CRAFT_VIDEO_PLUGIN.into(),
            enabled: false,
        })
        .unwrap();
    let before = owner.snapshot();
    let ctx = egui::Context::default();
    crate::theme::install(&ctx);
    let mut dialog = slot(&owner, source);
    click(&ctx, &owner, &mut dialog, "Add Filter");
    click(&ctx, &owner, &mut dialog, "AI Background Removal (Person)");
    assert!(
        matches!(draft(&mut dialog).rows.as_slice(),[row]if row.filter.effect==VideoEffect::person_background_removal())
    );
    frame(&ctx, &owner, &mut dialog, vec![]);
    frame(&ctx, &owner, &mut dialog, vec![]);
    assert!(ctx.data(|data| {
        data.get_temp::<bool>(egui::Id::new(("person-filter-enabled", "Person Threshold")))
            .unwrap()
    }));
    draft(&mut dialog).rows[0].filter.effect = VideoEffect::PersonBackgroundRemoval {
        threshold: 0.73,
        softness: 0.12,
    };
    draft(&mut dialog).rows[0].filter.enabled = false;
    click(&ctx, &owner, &mut dialog, "Reset Filter");
    assert_eq!(
        draft(&mut dialog).rows[0].filter.effect,
        VideoEffect::person_background_removal()
    );
    assert!(!draft(&mut dialog).rows[0].filter.enabled);
    draft(&mut dialog).rows[0].filter.effect = VideoEffect::PersonBackgroundRemoval {
        threshold: 0.7,
        softness: 0.0,
    };
    let intents = click(&ctx, &owner, &mut dialog, "Apply");
    assert!(
        intents
            .iter()
            .any(|intent| matches!(intent, Intent::DialogError(_)))
    );
    assert!(matches!(
        draft(&mut dialog).rows[0].filter.effect,
        VideoEffect::PersonBackgroundRemoval { softness: 0.0, .. }
    ));
    assert!(std::sync::Arc::ptr_eq(&before, &owner.snapshot()));
    draft(&mut dialog).rows[0].filter.effect = VideoEffect::PersonBackgroundRemoval {
        threshold: 0.7,
        softness: 0.2,
    };
    let intent = click(&ctx, &owner, &mut dialog, "Apply")
        .into_iter()
        .next()
        .unwrap();
    let Intent::ApplyVideoFilters {
        session,
        revision,
        source: target,
        filters,
    } = intent
    else {
        panic!("fenced Person Apply")
    };
    assert_eq!(session, owner.identity());
    assert_eq!(revision, before.revision());
    assert_eq!(target, source);
    owner
        .apply_at(
            Some(revision),
            Command::SetSourceVideoFilters { source, filters },
        )
        .unwrap();
    assert_eq!(owner.snapshot().revision(), before.revision() + 1);
    owner.apply(Command::Undo).unwrap();
    assert!(
        owner
            .snapshot()
            .source(source)
            .unwrap()
            .video_filters()
            .is_empty()
    );
    assert!(
        owner
            .snapshot()
            .preferences()
            .plugin_enabled(PERSON_BACKGROUND_PLUGIN)
    );
}
#[test]
fn disabled_neural_rows_stay_readable_but_craft_activation_cannot_unlock_their_controls() {
    let (mut owner, source) = fixture();
    owner
        .apply(Command::SetPluginEnabled {
            plugin: PERSON_BACKGROUND_PLUGIN.into(),
            enabled: true,
        })
        .unwrap();
    owner
        .apply(Command::SetSourceVideoFilters {
            source,
            filters: vec![
                VideoFilter::new(VideoEffect::PersonBackgroundRemoval {
                    threshold: 0.71,
                    softness: 0.24,
                })
                .unwrap(),
            ],
        })
        .unwrap();
    owner
        .apply(Command::SetPluginEnabled {
            plugin: PERSON_BACKGROUND_PLUGIN.into(),
            enabled: false,
        })
        .unwrap();
    let before = owner.snapshot();
    assert!(
        before
            .preferences()
            .plugin_enabled(skylive_core::CRAFT_VIDEO_PLUGIN)
    );
    let ctx = egui::Context::default();
    crate::theme::install(&ctx);
    let mut dialog = slot(&owner, source);
    frame(&ctx, &owner, &mut dialog, vec![]);
    frame(&ctx, &owner, &mut dialog, vec![]);
    assert!(!ctx.data(|data| {
        data.get_temp::<bool>(egui::Id::new(("person-filter-enabled", "Person Threshold")))
            .unwrap()
    }));
    click(&ctx, &owner, &mut dialog, "Reset Filter");
    assert!(
        matches!(draft(&mut dialog).rows[0].filter.effect,VideoEffect::PersonBackgroundRemoval{threshold,softness}if threshold==0.71&&softness==0.24)
    );
    click(&ctx, &owner, &mut dialog, "Remove Filter");
    let intent = click(&ctx, &owner, &mut dialog, "Apply")
        .into_iter()
        .next()
        .unwrap();
    let Intent::ApplyVideoFilters {
        revision, filters, ..
    } = intent
    else {
        panic!("disabled removal Apply")
    };
    owner
        .apply_at(
            Some(revision),
            Command::SetSourceVideoFilters { source, filters },
        )
        .unwrap();
    assert!(
        owner
            .snapshot()
            .source(source)
            .unwrap()
            .video_filters()
            .is_empty()
    );
}

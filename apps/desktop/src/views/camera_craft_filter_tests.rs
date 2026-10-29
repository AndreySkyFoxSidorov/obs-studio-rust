//! Actual toolbar/Add/category routing for authored camera sources; no camera backend.
use super::*;
use crate::views::dialogs;
use skylive_core::{
    Session, VideoCaptureBinding, VideoCaptureMode, VideoCaptureTarget, VideoEffect,
    VideoPixelFormat,
};

fn fixture() -> (Session, Id, Id) {
    let mut owner = Session::new();
    let scene = owner.preview();
    owner
        .apply(Command::AddVideoCaptureSource {
            scene,
            name: "Camera under test".into(),
            binding: VideoCaptureBinding::new(
                VideoCaptureTarget::Windows {
                    symbolic_link: "inert-camera-route-fixture".into(),
                },
                VideoCaptureMode::new(2, 2, 30, 1, VideoPixelFormat::Rgb32).unwrap(),
                false,
            )
            .unwrap(),
        })
        .unwrap();
    let item = owner.snapshot().scene(scene).unwrap().items()[0].id();
    let source = owner.snapshot().sources()[0].id();
    (owner, item, source)
}
fn input(events: Vec<egui::Event>) -> egui::RawInput {
    egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1000.0, 800.0),
        )),
        events,
        ..Default::default()
    }
}
fn toolbar(
    ctx: &egui::Context,
    owner: &Session,
    item: Id,
    events: Vec<egui::Event>,
) -> Vec<Intent> {
    let mut intents = vec![];
    let _ = ctx.run(input(events), |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| {
            super::super::dock_toolbar::begin(ui);
            sources(
                ui,
                &owner.snapshot(),
                owner.preview(),
                Some(item),
                owner.identity(),
                None,
                &mut intents,
            );
        });
    });
    intents
}
fn dialog(
    ctx: &egui::Context,
    owner: &Session,
    slot: &mut Option<Dialog>,
    events: Vec<egui::Event>,
) -> (egui::FullOutput, Vec<Intent>) {
    let mut intents = vec![];
    let out = ctx.run(input(events), |ctx| {
        dialogs::show(ctx, slot, &mut intents, true, &owner.snapshot())
    });
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
    dialog(ctx, owner, slot, vec![]);
    let (out, _) = dialog(ctx, owner, slot, vec![]);
    let p = out
        .shapes
        .iter()
        .rev()
        .find_map(|shape| point(&shape.shape, label).filter(|p| shape.clip_rect.contains(*p)))
        .unwrap_or_else(|| {
            let labels: Vec<_> = out
                .shapes
                .iter()
                .filter_map(|shape| {
                    if let egui::epaint::Shape::Text(t) = &shape.shape {
                        Some((t.galley.text(), shape.clip_rect, t.pos))
                    } else {
                        None
                    }
                })
                .collect();
            panic!(
                "visible camera filter action {label}, popup={}, labels={labels:?}",
                ctx.memory(|memory| memory.any_popup_open())
            )
        });
    dialog(
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
    dialog(
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
#[test]
fn camera_sources_have_direct_filter_toolbar_and_add_real_craft_types_in_one_apply() {
    let (mut owner, item, source) = fixture();
    let ctx = egui::Context::default();
    crate::theme::install(&ctx);
    crate::i18n::install(
        &ctx,
        crate::i18n::english().unwrap(),
        std::path::Path::new("headless-translations.csv"),
    );
    let before = owner.snapshot();
    assert!(
        before
            .preferences()
            .plugin_enabled(skylive_core::CRAFT_VIDEO_PLUGIN)
    );
    toolbar(&ctx, &owner, item, vec![]);
    toolbar(&ctx, &owner, item, vec![]);
    let p = ctx
        .data(|data| {
            data.get_temp::<Vec<crate::views::dock_toolbar::Probe>>(egui::Id::new(
                "dock-toolbar-probes",
            ))
            .unwrap()
        })
        .iter()
        .find(|probe| probe.scope == "Sources" && probe.action == "Filters")
        .unwrap()
        .rect
        .center();
    toolbar(
        &ctx,
        &owner,
        item,
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
    let intents = toolbar(
        &ctx,
        &owner,
        item,
        vec![
            egui::Event::PointerMoved(p),
            egui::Event::PointerButton {
                pos: p,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            },
        ],
    );
    let mut slot = Some(match intents.into_iter().next().unwrap() {
        Intent::OpenDialog(Dialog::VideoFilters(draft)) => Dialog::VideoFilters(draft),
        _ => panic!("camera video filter editor"),
    });
    for label in [
        "Linear Color Key",
        "Brightness & Contrast",
        "Spill Suppression",
    ] {
        click(&ctx, &owner, &mut slot, "Add Filter");
        click(&ctx, &owner, &mut slot, label);
    }
    assert!(std::sync::Arc::ptr_eq(&before, &owner.snapshot()));
    let intent = click(&ctx, &owner, &mut slot, "Apply")
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
        panic!("fenced camera Apply")
    };
    assert_eq!(session, owner.identity());
    assert_eq!(revision, before.revision());
    assert_eq!(target, source);
    assert!(
        matches!(filters.as_slice(),[a,b,c] if matches!(a.effect,VideoEffect::LinearColorKey{..})&&matches!(b.effect,VideoEffect::BrightnessContrast{..})&&matches!(c.effect,VideoEffect::SpillSuppression{..}))
    );
    owner
        .apply_at(
            Some(revision),
            Command::SetSourceVideoFilters { source, filters },
        )
        .unwrap();
    assert_eq!(owner.snapshot().revision(), before.revision() + 1);
    assert_eq!(
        owner
            .snapshot()
            .source(source)
            .unwrap()
            .video_filters()
            .len(),
        3
    );
    owner.apply(Command::Undo).unwrap();
    assert!(
        owner
            .snapshot()
            .source(source)
            .unwrap()
            .video_filters()
            .is_empty()
    );
}
#[test]
fn camera_builtin_filters_stay_available_while_disabled_craft_cannot_be_added() {
    let (mut owner, _, source) = fixture();
    owner
        .apply(Command::SetPluginEnabled {
            plugin: skylive_core::CRAFT_VIDEO_PLUGIN.into(),
            enabled: false,
        })
        .unwrap();
    let ctx = egui::Context::default();
    crate::theme::install(&ctx);
    crate::i18n::install(
        &ctx,
        crate::i18n::english().unwrap(),
        std::path::Path::new("headless-translations.csv"),
    );
    let before = owner.snapshot();
    let mut slot = Some(Dialog::VideoFilters(
        crate::views::video_filters::VideoFilterDraft::capture(owner.identity(), &before, source)
            .unwrap(),
    ));
    click(&ctx, &owner, &mut slot, "Add Filter");
    click(&ctx, &owner, &mut slot, "Brightness & Contrast");
    let Some(Dialog::VideoFilters(draft)) = &slot else {
        panic!("camera draft")
    };
    assert!(
        matches!(draft.command().unwrap(),Command::SetSourceVideoFilters{filters,..}if filters.is_empty())
    );
    // Use a fresh editor to test built-in selection independently of the
    // disabled menu item's dismissal/hover policy.
    let ctx = egui::Context::default();
    crate::theme::install(&ctx);
    crate::i18n::install(
        &ctx,
        crate::i18n::english().unwrap(),
        std::path::Path::new("headless-translations.csv"),
    );
    let mut slot = Some(Dialog::VideoFilters(
        crate::views::video_filters::VideoFilterDraft::capture(owner.identity(), &before, source)
            .unwrap(),
    ));
    click(&ctx, &owner, &mut slot, "Add Filter");
    click(&ctx, &owner, &mut slot, "Color Correction");
    let Some(Dialog::VideoFilters(draft)) = &slot else {
        panic!("camera draft")
    };
    assert!(
        matches!(draft.command().unwrap(),Command::SetSourceVideoFilters{filters,..}if matches!(filters.as_slice(),[filter]if matches!(filter.effect,VideoEffect::ColorCorrection{..})))
    );
    click(&ctx, &owner, &mut slot, "Cancel");
    assert!(slot.is_none());
    assert!(std::sync::Arc::ptr_eq(&before, &owner.snapshot()));
}

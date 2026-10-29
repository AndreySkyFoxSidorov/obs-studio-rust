use super::*;

#[test]
fn size_admission_preserves_valid_presentation_for_bad_values_and_reset_clears_bounded_panel_ids() {
    let ctx = egui::Context::default();
    let mut layout = DockLayout::default();
    layout.set_size(&ctx, DockSize::SideWidth, 280);
    assert_eq!(layout.size(&ctx, DockSize::SideWidth), 280);
    layout.set_size(&ctx, DockSize::SideWidth, u16::MAX);
    assert_eq!(layout.size(&ctx, DockSize::SideWidth), 280);
    assert_eq!(layout.effective(1280.0), DockPreset::Obs);
    assert_eq!(layout.effective(640.0), DockPreset::BottomRow);
    ctx.data_mut(|data| {
        for id in [SIDE, SCENES, BOTTOM] {
            data.insert_persisted(
                egui::Id::new(id),
                egui::containers::panel::PanelState {
                    rect: egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(300.0, 300.0)),
                },
            );
        }
    });
    forget_sizes(&ctx);
    for id in [SIDE, SCENES, BOTTOM] {
        assert!(egui::containers::panel::PanelState::load(&ctx, egui::Id::new(id)).is_none());
    }
}

#[test]
fn locked_columns_keep_compact_tabs_operable_and_controls_pinned() {
    let ctx = egui::Context::default();
    crate::theme::install(&ctx);
    let mut columns = super::super::dock_columns::DockColumns::default();
    let mut plan = Plan { rects: [None; 5] };
    let mut render = |events: Vec<egui::Event>| {
        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(600.0, 300.0),
                )),
                events,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let area = ui.available_rect_before_wrap();
                    plan = columns.show_locked(ui, area, [true; 5], true);
                });
            },
        );
    };
    render(vec![]);
    render(vec![]);
    let target = ctx.data(|data| {
        data.get_temp::<egui::Rect>(egui::Id::new(("compact-dock-tab", 2_usize)))
            .unwrap()
            .center()
    });
    for pressed in [true, false] {
        render(vec![
            egui::Event::PointerMoved(target),
            egui::Event::PointerButton {
                pos: target,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            },
        ]);
    }
    assert!(
        plan.rects[2].is_some(),
        "Lock must not disable compact content selection"
    );
    assert!(
        plan.rects[4].is_some(),
        "Stop controls remain pinned while locked"
    );
}

use super::*;
fn area() -> egui::Rect {
    egui::Rect::from_min_size(egui::pos2(8.0, 520.0), egui::vec2(1264.0, 240.0))
}
fn input(time: f64, events: Vec<egui::Event>) -> egui::RawInput {
    egui::RawInput {
        time: Some(time),
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1280.0, 800.0),
        )),
        events,
        ..Default::default()
    }
}
fn pointer(pos: egui::Pos2, pressed: bool) -> egui::Event {
    egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    }
}
fn render(
    columns: &mut DockColumns,
    ctx: &egui::Context,
    area: egui::Rect,
    visible: [bool; 5],
    raw: egui::RawInput,
) -> (Plan, egui::FullOutput) {
    let mut plan = Plan { rects: [None; 5] };
    let output = ctx.run(raw, |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| {
            plan = columns.show(ui, area, visible);
        });
    });
    (plan, output)
}
fn setup() -> (egui::Context, DockColumns, Plan) {
    let ctx = egui::Context::default();
    crate::theme::install(&ctx);
    let mut columns = DockColumns::default();
    let _ = render(&mut columns, &ctx, area(), [true; 5], input(0.0, vec![]));
    let (plan, _) = render(&mut columns, &ctx, area(), [true; 5], input(0.01, vec![]));
    (ctx, columns, plan)
}
fn boundary(plan: Plan, left: usize, right: usize) -> egui::Pos2 {
    egui::pos2(
        (plan.rects[left].unwrap().right() + plan.rects[right].unwrap().left()) * 0.5,
        area().center().y,
    )
}
fn line(shape: &egui::epaint::Shape, x: f32, y: std::ops::RangeInclusive<f32>) -> bool {
    match shape {
        egui::epaint::Shape::LineSegment { points, .. } => {
            points.iter().all(|p| (p.x - x).abs() < 0.01)
                && (points[0].y - *y.start()).abs() < 0.01
                && (points[1].y - *y.end()).abs() < 0.01
        }
        egui::epaint::Shape::Vec(v) => v.iter().any(|s| line(s, x, y.clone())),
        _ => false,
    }
}

#[test]
fn actual_drag_previews_then_commits_one_bounded_pair_and_paints_real_splitter() {
    let (ctx, mut columns, plan) = setup();
    let original = columns.weights();
    let p = boundary(plan, 1, 2);
    let _ = render(
        &mut columns,
        &ctx,
        area(),
        [true; 5],
        input(0.1, vec![egui::Event::PointerMoved(p), pointer(p, true)]),
    );
    let moved = p + egui::vec2(50.0, 0.0);
    let (preview, _) = render(
        &mut columns,
        &ctx,
        area(),
        [true; 5],
        input(0.2, vec![egui::Event::PointerMoved(moved)]),
    );
    assert_eq!(
        columns.weights(),
        original,
        "drag draft is not the committed window layout"
    );
    assert!(
        (preview.rects[1].unwrap().width() - plan.rects[1].unwrap().width() - 50.0).abs() < 0.1
    );
    let (committed, output) = render(
        &mut columns,
        &ctx,
        area(),
        [true; 5],
        input(0.3, vec![pointer(moved, false)]),
    );
    assert_ne!(columns.weights(), original);
    assert_eq!(
        columns.weights().into_iter().map(u32::from).sum::<u32>(),
        10000
    );
    for index in [0, 3, 4] {
        assert_eq!(columns.weights()[index], original[index]);
    }
    assert!(committed.rects[4].unwrap().width() >= 250.0);
    let x = boundary(committed, 1, 2).x;
    assert!(
        output
            .shapes
            .iter()
            .any(|s| line(&s.shape, x, area().top()..=area().bottom())),
        "divider must actually paint, not only exist as metadata"
    );
}

#[test]
fn coalesced_press_move_release_uses_button_origin_even_when_release_is_outside_column() {
    let (ctx, mut columns, plan) = setup();
    let p = boundary(plan, 1, 2);
    let q = p + egui::vec2(70.0, -80.0);
    let (next, _) = render(
        &mut columns,
        &ctx,
        area(),
        [true; 5],
        input(
            0.1,
            vec![
                egui::Event::PointerMoved(p),
                pointer(p, true),
                egui::Event::PointerMoved(q),
                pointer(q, false),
            ],
        ),
    );
    assert!((next.rects[1].unwrap().width() - plan.rects[1].unwrap().width() - 70.0).abs() < 0.1);
    assert_ne!(columns.weights(), DEFAULT);
}

#[test]
fn escape_focus_mask_size_and_scale_cancel_resize_without_committing() {
    for invalidation in 0..5 {
        let (ctx, mut columns, plan) = setup();
        let original = columns.weights();
        let p = boundary(plan, 1, 2);
        let q = p + egui::vec2(60.0, 0.0);
        let _ = render(
            &mut columns,
            &ctx,
            area(),
            [true; 5],
            input(0.1, vec![egui::Event::PointerMoved(p), pointer(p, true)]),
        );
        let _ = render(
            &mut columns,
            &ctx,
            area(),
            [true; 5],
            input(0.2, vec![egui::Event::PointerMoved(q)]),
        );
        let mut raw = input(0.3, vec![]);
        let mut visible = [true; 5];
        let mut rect = area();
        match invalidation {
            0 => raw.events.push(egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }),
            1 => raw.focused = false,
            2 => visible[0] = false,
            3 => rect.max.x -= 1.0,
            4 => ctx.set_zoom_factor(1.5),
            _ => unreachable!(),
        }
        let _ = render(&mut columns, &ctx, rect, visible, raw);
        assert_eq!(columns.weights(), original, "invalidation={invalidation}");
        assert!(columns.gesture.is_none());
    }
}

#[test]
fn focused_splitter_supports_keyboard_and_home_without_leaking_arrow_to_preview() {
    let (ctx, mut columns, plan) = setup();
    let p = boundary(plan, 1, 2);
    let _ = render(
        &mut columns,
        &ctx,
        area(),
        [true; 5],
        input(0.1, vec![egui::Event::PointerMoved(p), pointer(p, true)]),
    );
    let _ = render(
        &mut columns,
        &ctx,
        area(),
        [true; 5],
        input(0.2, vec![pointer(p, false)]),
    );
    assert!(columns.reserves_keyboard(&ctx));
    let before = columns.rects(area(), [true; 5]);
    let modifiers = egui::Modifiers::SHIFT;
    let (next, _) = render(
        &mut columns,
        &ctx,
        area(),
        [true; 5],
        input(
            0.3,
            vec![egui::Event::Key {
                key: egui::Key::ArrowRight,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers,
            }],
        ),
    );
    assert!((next.rects[1].unwrap().width() - before[1].unwrap().width() - 10.0).abs() < 0.1);
    assert!(!ctx.input(|input| input.events.iter().any(|e| matches!(
        e,
        egui::Event::Key {
            key: egui::Key::ArrowRight,
            pressed: true,
            ..
        }
    ))));
    let _ = render(
        &mut columns,
        &ctx,
        area(),
        [true; 5],
        input(
            0.4,
            vec![egui::Event::Key {
                key: egui::Key::Home,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
        ),
    );
    assert_eq!(columns.weights(), DEFAULT);
}

#[test]
fn compact_navigation_keeps_controls_pinned_and_restores_exact_wide_weights() {
    let (ctx, mut columns, plan) = setup();
    let p = boundary(plan, 1, 2);
    let q = p + egui::vec2(45.0, 0.0);
    let _ = render(
        &mut columns,
        &ctx,
        area(),
        [true; 5],
        input(
            0.1,
            vec![
                pointer(p, true),
                egui::Event::PointerMoved(q),
                pointer(q, false),
            ],
        ),
    );
    let wider = columns.weights();
    let narrow = egui::Rect::from_min_size(egui::pos2(8.0, 20.0), egui::vec2(384.0, 240.0));
    let (plan, _) = render(&mut columns, &ctx, narrow, [true; 5], input(0.2, vec![]));
    assert_eq!(plan.rects[4].unwrap().width(), 250.0);
    assert!(plan.rects[1].is_some());
    assert!(plan.rects[0].is_none() && plan.rects[2].is_none());
    let _ = render(&mut columns, &ctx, area(), [true; 5], input(0.3, vec![]));
    assert_eq!(columns.weights(), wider);
    let mut hidden = [true; 5];
    hidden[2] = false;
    let _ = render(&mut columns, &ctx, area(), hidden, input(0.4, vec![]));
    assert_eq!(columns.weights(), wider);
}

#[test]
fn pair_admission_never_hides_controls_or_breaks_integer_weight_boundaries() {
    for width in [746.0, 800.0, 1280.0, 4096.0] {
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, 240.0));
        let plan = DockColumns::wide(rect, [true; 5], DEFAULT);
        let widths = std::array::from_fn(|i| plan.rects[i].unwrap().width());
        for left in 0..4 {
            for delta in [-1e20, -1000.0, -1.0, 0.0, 1.0, 1000.0, 1e20, f32::NAN] {
                let next = adjust(DEFAULT, widths, left, left + 1, delta);
                assert!(next.into_iter().all(|v| v > 0));
                assert_eq!(next.into_iter().map(u32::from).sum::<u32>(), 10000);
                let plan = DockColumns::wide(rect, [true; 5], next);
                for (i, r) in plan.rects.into_iter().enumerate() {
                    assert!(r.unwrap().width() + 0.01 >= MIN[i]);
                }
            }
        }
    }
}

#[test]
fn actual_double_click_restores_default_widths() {
    let (ctx, mut columns, plan) = setup();
    let p = boundary(plan, 1, 2);
    let q = p + egui::vec2(40.0, 0.0);
    let (plan, _) = render(
        &mut columns,
        &ctx,
        area(),
        [true; 5],
        input(
            0.1,
            vec![
                pointer(p, true),
                egui::Event::PointerMoved(q),
                pointer(q, false),
            ],
        ),
    );
    assert_ne!(columns.weights(), DEFAULT);
    let p = boundary(plan, 1, 2);
    for (time, pressed) in [(0.5, true), (0.51, false), (0.6, true), (0.61, false)] {
        let _ = render(
            &mut columns,
            &ctx,
            area(),
            [true; 5],
            input(
                time,
                vec![egui::Event::PointerMoved(p), pointer(p, pressed)],
            ),
        );
    }
    assert_eq!(columns.weights(), DEFAULT);
}

#[test]
fn narrow_compact_tabs_can_be_scrolled_and_selected_with_real_mouse_without_snap_back() {
    let (ctx, mut columns, _) = setup();
    let area = egui::Rect::from_min_size(egui::pos2(8.0, 20.0), egui::vec2(384.0, 240.0));
    for time in [0.1, 0.2, 0.3] {
        let _ = render(&mut columns, &ctx, area, [true; 5], input(time, vec![]));
    }
    let p = egui::pos2(30.0, 30.0);
    let _ = render(
        &mut columns,
        &ctx,
        area,
        [true; 5],
        input(
            0.4,
            vec![
                egui::Event::PointerMoved(p),
                egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta: egui::vec2(-80.0, 0.0),
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        ),
    );
    for time in [0.5, 0.6, 0.7] {
        let _ = render(&mut columns, &ctx, area, [true; 5], input(time, vec![]));
    }
    let rect = ctx.data(|data| {
        data.get_temp::<egui::Rect>(egui::Id::new(("compact-dock-tab", 2_usize)))
            .unwrap()
    });
    let viewport = egui::Rect::from_min_size(area.min, egui::vec2(130.0, 40.0));
    assert!(
        viewport.contains(rect.center()),
        "mouse scroll must reveal the mixer tab, not snap back to Sources: {rect:?}"
    );
    let p = rect.center();
    for (time, pressed) in [(0.8, true), (0.9, false)] {
        let _ = render(
            &mut columns,
            &ctx,
            area,
            [true; 5],
            input(
                time,
                vec![egui::Event::PointerMoved(p), pointer(p, pressed)],
            ),
        );
    }
    let (plan, _) = render(&mut columns, &ctx, area, [true; 5], input(1.0, vec![]));
    assert_eq!(columns.selected(), 2);
    assert!(plan.rects[2].is_some() && plan.rects[1].is_none());
    assert_eq!(plan.rects[4].unwrap().width(), 250.0);
}

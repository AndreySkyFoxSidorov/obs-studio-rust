//! Copy to views/streaming_controls_tests.rs and register as a child of docks.rs.
//! No native backend, socket, device, file I/O or application construction.
use super::*;
use skylive_core::{ProfileCommand, Session, StreamingSettings};
use skylive_host::streaming::Phase;

fn configured() -> Session {
    let mut session = Session::new();
    let id = session.snapshot().profiles().selected_id();
    session
        .apply(Command::Profile {
            command: ProfileCommand::SetStreaming {
                id,
                settings: StreamingSettings::new(
                    "rtmp://127.0.0.1/live".into(),
                    "synthetic-never-connected".into(),
                    2500,
                    2,
                    false,
                    160,
                )
                .unwrap(),
            },
        })
        .unwrap();
    session
}

fn text<'a>(shape: &'a egui::epaint::Shape, expected: &str) -> Option<&'a egui::epaint::TextShape> {
    match shape {
        egui::epaint::Shape::Text(value) if value.galley.text() == expected => Some(value),
        egui::epaint::Shape::Vec(items) => items.iter().find_map(|item| text(item, expected)),
        _ => None,
    }
}

fn context(locale: &str) -> egui::Context {
    let context = egui::Context::default();
    crate::theme::install(&context);
    let catalog =
        std::sync::Arc::new(skylive_host::localization::Localization::embedded(locale).unwrap());
    crate::i18n::install(&context, catalog, std::path::Path::new("translations.csv"));
    context
}

fn frame(
    ctx: &egui::Context,
    session: &Session,
    state: &super::super::StreamingViewState,
    viewport: egui::Vec2,
    rect: egui::Rect,
    events: Vec<egui::Event>,
    intents: &mut Vec<Intent>,
) -> egui::FullOutput {
    ctx.run(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, viewport)),
            events,
            ..Default::default()
        },
        |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let mut child = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(rect)
                        .layout(egui::Layout::top_down(egui::Align::Min)),
                );
                child.set_clip_rect(rect.intersect(ui.clip_rect()));
                child.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);
                controls_with_outputs(
                    &mut child,
                    &session.snapshot(),
                    false,
                    super::super::RecordingViewState {
                        available: true,
                        active: true,
                        ..Default::default()
                    },
                    state.clone(),
                    ctx.data(|data| {
                        data.get_temp::<bool>(egui::Id::new("streaming-test-editing-enabled"))
                    })
                    .unwrap_or(true),
                    intents,
                );
            });
        },
    )
}

fn click(session: &Session, state: super::super::StreamingViewState, caption: &str) -> Vec<Intent> {
    click_when(session, state, caption, true)
}
fn click_when(
    session: &Session,
    state: super::super::StreamingViewState,
    caption: &str,
    editing_enabled: bool,
) -> Vec<Intent> {
    let ctx = context("en-US");
    ctx.data_mut(|data| {
        data.insert_temp(
            egui::Id::new("streaming-test-editing-enabled"),
            editing_enabled,
        )
    });
    let viewport = egui::vec2(800.0, 560.0);
    let rect = egui::Rect::from_min_size(egui::pos2(20.0, 20.0), egui::vec2(300.0, 360.0));
    let mut intents = Vec::new();
    let _ = frame(&ctx, session, &state, viewport, rect, vec![], &mut intents);
    let output = frame(&ctx, session, &state, viewport, rect, vec![], &mut intents);
    let label = crate::i18n::tr_context(&ctx, caption);
    let shape = output
        .shapes
        .iter()
        .find_map(|shape| text(&shape.shape, &label).map(|text| (text, shape.clip_rect)))
        .expect("actual painted streaming control");
    let bounds = egui::Rect::from_min_size(shape.0.pos, shape.0.galley.size());
    assert!(
        shape.1.contains_rect(bounds),
        "streaming caption is fully painted"
    );
    let pos = bounds.center();
    for pressed in [true, false] {
        let _ = frame(
            &ctx,
            session,
            &state,
            viewport,
            rect,
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
            &mut intents,
        );
    }
    intents
}

#[test]
fn streaming_unconfigured_and_unavailable_buttons_never_emit_start() {
    let available = super::super::StreamingViewState {
        available: true,
        ..Default::default()
    };
    assert!(click(&Session::new(), available, "Start Streaming").is_empty());
    // Inject the macOS/current-unavailable presentation explicitly, independent of build OS.
    assert!(click(&configured(), Default::default(), "Start Streaming").is_empty());
}

#[test]
fn configured_streaming_click_emits_only_start_and_does_not_stop_recording() {
    let intents = click(
        &configured(),
        super::super::StreamingViewState {
            available: true,
            ..Default::default()
        },
        "Start Streaming",
    );
    assert!(matches!(intents.as_slice(), [Intent::StartStreaming]));
}

#[test]
fn initializing_connecting_ready_and_publishing_click_emit_only_streaming_stop() {
    for phase in [
        Phase::Initializing,
        Phase::Connecting,
        Phase::ReadyAwaitingInput,
        Phase::Publishing,
    ] {
        let caption = if phase == Phase::Publishing {
            "Stop Streaming"
        } else {
            "Cancel Streaming Start"
        };
        let intents = click(
            &configured(),
            super::super::StreamingViewState {
                available: true,
                active: true,
                phase: Some(phase),
                ..Default::default()
            },
            caption,
        );
        assert!(
            matches!(intents.as_slice(), [Intent::StopStreaming]),
            "phase={phase:?}"
        );
    }
}

#[test]
fn finishing_streaming_disables_repeated_stop_click() {
    assert!(
        click(
            &configured(),
            super::super::StreamingViewState {
                available: true,
                active: true,
                finishing: true,
                phase: Some(Phase::Finishing),
                ..Default::default()
            },
            "Stopping streaming…"
        )
        .is_empty()
    );
}

#[test]
fn all_output_controls_fit_real_dock_column_at_scaled_minimum_window_in_five_locales() {
    let session = configured();
    for locale in ["en-US", "es-ES", "ru-RU", "uk-UA", "de-DE"] {
        for scale in [1.5_f32, 2.0] {
            // RawInput is logical coordinates; these correspond to physical 800×560.
            let viewport = egui::vec2(800.0 / scale, 560.0 / scale);
            let ctx = context(locale);
            let dock_area = egui::Rect::from_min_max(
                egui::pos2(8.0, (viewport.y - 260.0).max(8.0)),
                egui::pos2(viewport.x - 8.0, viewport.y - 8.0),
            );
            // Use the production compact/wide allocation. The legacy pure
            // five-column helper deliberately has no compact tabs and would
            // produce an unrealistically narrow Controls column here.
            let mut columns = super::super::dock_columns::DockColumns::default();
            let mut rect = egui::Rect::NOTHING;
            let _ = ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, viewport)),
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        rect = columns.show(ui, dock_area, [true; 5]).rects[4]
                            .unwrap()
                            .shrink(5.0);
                    });
                },
            );
            for (phase, caption) in [
                (Phase::Initializing, "Cancel Streaming Start"),
                (Phase::Publishing, "Stop Streaming"),
                (Phase::Finishing, "Stopping streaming…"),
            ] {
                let state = super::super::StreamingViewState {
                    available: true,
                    active: true,
                    finishing: phase == Phase::Finishing,
                    phase: Some(phase),
                    ..Default::default()
                };
                let mut intents = Vec::new();
                let _ = frame(&ctx, &session, &state, viewport, rect, vec![], &mut intents);
                let output = frame(&ctx, &session, &state, viewport, rect, vec![], &mut intents);
                for english in [
                    caption,
                    "Stop Recording",
                    "Start Virtual Camera",
                    "Start Replay Buffer",
                    "Studio Mode",
                    "Settings",
                    "Exit",
                ] {
                    let expected = crate::i18n::tr_context(&ctx, english);
                    let (paint, clip) = output
                        .shapes
                        .iter()
                        .find_map(|shape| {
                            text(&shape.shape, &expected).map(|text| (text, shape.clip_rect))
                        })
                        .unwrap_or_else(|| {
                            panic!("missing {english}; locale={locale} scale={scale}")
                        });
                    let bounds = egui::Rect::from_min_size(paint.pos, paint.galley.size());
                    assert!(
                        rect.contains_rect(bounds) && clip.contains_rect(bounds),
                        "{english}: locale={locale} scale={scale} bounds={bounds:?} rect={rect:?} clip={clip:?}"
                    );
                }
                assert!(intents.is_empty());
            }
        }
    }
}

#[test]
fn document_io_keeps_stop_and_cancel_clickable_but_prevents_starting_another_output() {
    let session = configured();
    for (phase, caption) in [
        (Phase::Initializing, "Cancel Streaming Start"),
        (Phase::Publishing, "Stop Streaming"),
    ] {
        let state = super::super::StreamingViewState {
            available: true,
            active: true,
            phase: Some(phase),
            ..Default::default()
        };
        assert!(matches!(
            click_when(&session, state, caption, false).as_slice(),
            [Intent::StopStreaming]
        ));
    }
    let state = super::super::StreamingViewState {
        available: true,
        ..Default::default()
    };
    assert!(click_when(&session, state, "Start Streaming", false).is_empty());
}

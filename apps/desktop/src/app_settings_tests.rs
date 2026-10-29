use super::*;
use std::{
    fs,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};
static NEXT: AtomicU64 = AtomicU64::new(1);
struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "skylive-settings-flow-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn csv(&self) -> PathBuf {
        self.0.join("translations.csv")
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_file(self.csv());
        let _ = fs::remove_dir(&self.0);
    }
}
fn frame(app: &mut StudioApp, ctx: &egui::Context, events: Vec<egui::Event>) -> egui::FullOutput {
    ctx.run(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1280.0, 800.0),
            )),
            events,
            ..Default::default()
        },
        |ctx| app.frame_at(ctx, MediaTime(0)),
    )
}
fn point(shape: &egui::epaint::Shape, label: &str) -> Option<egui::Pos2> {
    match shape {
        egui::epaint::Shape::Text(t) if t.galley.text() == label => {
            Some(t.pos + t.galley.size() * 0.5)
        }
        egui::epaint::Shape::Vec(v) => v.iter().find_map(|s| point(s, label)),
        _ => None,
    }
}
fn click(app: &mut StudioApp, ctx: &egui::Context, english: &str) {
    let _ = frame(app, ctx, vec![]);
    let output = frame(app, ctx, vec![]);
    let label = crate::i18n::tr_context(ctx, english);
    let p = output
        .shapes
        .iter()
        .find_map(|s| point(&s.shape, &label).filter(|p| s.clip_rect.contains(*p)))
        .unwrap_or_else(|| panic!("actual Settings control {label}"));
    for pressed in [true, false] {
        let _ = frame(
            app,
            ctx,
            vec![
                egui::Event::PointerMoved(p),
                egui::Event::PointerButton {
                    pos: p,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
    }
}
fn app(ctx: &egui::Context) -> StudioApp {
    let mut app = StudioApp::headless(ctx).unwrap();
    app.apply(Command::SetVideo {
        width: 64,
        height: 48,
        fps_num: 30,
        fps_den: 1,
    });
    app
}
fn open(app: &mut StudioApp, ctx: &egui::Context, page: SettingsPage) {
    app.intent(
        Intent::OpenDialog(settings::fresh(page, &app.session.snapshot())),
        ctx,
    );
}
fn pump(app: &mut StudioApp, ctx: &egui::Context) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while app.io.busy() {
        app.poll_io(ctx);
        assert!(
            Instant::now() < deadline,
            "Settings I/O completion: {}",
            app.status
        );
        std::thread::yield_now();
    }
}
fn general_language(app: &mut StudioApp, value: &str) {
    match &mut app.dialog {
        Some(Dialog::General { language,.. }) => *language = value.into(),
        _ => panic!("General draft"),
    }
}

#[test]
fn actual_audio_apply_keeps_page_refreshes_baseline_then_cancel_only_discards_later_changes() {
    let ctx = egui::Context::default();
    let mut app = app(&ctx);
    let original = app.session.snapshot();
    open(&mut app, &ctx, SettingsPage::Audio);
    click(&mut app, &ctx, "48 kHz");
    click(&mut app, &ctx, "44.1 kHz");
    assert!(Arc::ptr_eq(&original, &app.session.snapshot()));
    click(&mut app, &ctx, "Apply");
    assert_eq!(app.session.snapshot().audio().sample_rate(), 44100);
    assert_eq!(app.session.snapshot().revision(), original.revision() + 1);
    assert!(matches!(
        &app.dialog,
        Some(Dialog::AudioSettings {
            sample_rate: 44100,
            ..
        })
    ));
    assert!(!app.session.can_undo());
    // Navigation is now enabled in the SAME Settings window after Apply.
    click(&mut app, &ctx, "Video");
    assert!(matches!(app.dialog, Some(Dialog::Video { .. })));
    click(&mut app, &ctx, "Audio");
    click(&mut app, &ctx, "44.1 kHz");
    click(&mut app, &ctx, "48 kHz");
    click(&mut app, &ctx, "Cancel");
    assert!(app.dialog.is_none());
    assert_eq!(app.session.snapshot().audio().sample_rate(), 44100);
    let after = app.session.snapshot();
    open(&mut app, &ctx, SettingsPage::Audio);
    click(&mut app, &ctx, "OK");
    assert!(app.dialog.is_none());
    assert!(Arc::ptr_eq(&after, &app.session.snapshot()));
}

#[test]
fn invalid_video_apply_retains_exact_draft_and_success_is_one_edit_with_unchanged_apply_disabled() {
    let ctx = egui::Context::default();
    let mut app = app(&ctx);
    open(&mut app, &ctx, SettingsPage::Video);
    if let Some(Dialog::Video { width, .. }) = &mut app.dialog {
        *width = 0;
    }
    let before = app.session.snapshot();
    click(&mut app, &ctx, "Apply");
    assert!(app.error);
    assert!(Arc::ptr_eq(&before, &app.session.snapshot()));
    assert!(matches!(app.dialog, Some(Dialog::Video { width: 0, .. })));
    if let Some(Dialog::Video { width, .. }) = &mut app.dialog {
        *width = 128;
    }
    click(&mut app, &ctx, "Apply");
    assert!(!app.error, "{}", app.status);
    let changed = app.session.snapshot();
    assert_eq!(changed.video().width(), 128);
    assert_eq!(changed.revision(), before.revision() + 1);
    assert!(!app.session.can_undo());
    assert!(matches!(&app.dialog,Some(dialog)if settings::unchanged(dialog,&changed)));
    click(&mut app, &ctx, "Apply");
    assert!(Arc::ptr_eq(&changed, &app.session.snapshot()));
    click(&mut app, &ctx, "OK");
    assert!(app.dialog.is_none());
    assert!(Arc::ptr_eq(&changed, &app.session.snapshot()));
}

#[test]
fn stale_settings_page_cannot_publish_or_close_a_newer_revision() {
    let ctx = egui::Context::default();
    let mut app = app(&ctx);
    open(&mut app, &ctx, SettingsPage::Video);
    if let Some(Dialog::Video { width, .. }) = &mut app.dialog {
        *width = 128;
    }
    app.session
        .apply(Command::AddScene {
            name: "New authored revision".into(),
        })
        .unwrap();
    let newer = app.session.snapshot();
    click(&mut app, &ctx, "Apply");
    assert!(app.error);
    assert!(Arc::ptr_eq(&newer, &app.session.snapshot()));
    assert!(matches!(app.dialog, Some(Dialog::Video { width: 128, .. })));
    click(&mut app, &ctx, "Cancel");
    assert!(app.dialog.is_none());
}

#[test]
fn general_apply_loads_asynchronously_keeps_page_and_failure_retains_exact_language_draft() {
    let dir = Directory::new();
    let ctx = egui::Context::default();
    let mut app = app(&ctx);
    app.localization_path = dir.csv();
    let csv = "key,en-US,ru-RU\nSettings,Settings,Настройки проверки\nSkyLive.LanguageName,English,Русский\n";
    fs::write(dir.csv(), csv).unwrap();
    open(&mut app, &ctx, SettingsPage::General);
    general_language(&mut app, "ru-RU");
    click(&mut app, &ctx, "Apply");
    assert!(app.io.busy());
    assert_eq!(app.session.snapshot().preferences().language(), "en-US");
    pump(&mut app, &ctx);
    assert_eq!(app.session.snapshot().preferences().language(), "ru-RU");
    assert_eq!(app.localization.language(), "ru-RU");
    assert_eq!(
        crate::i18n::tr_context(&ctx, "Settings"),
        "Настройки проверки"
    );
    assert!(matches!(&app.dialog,Some(Dialog::General{language,..})if language=="ru-RU"));
    let applied = app.session.snapshot();
    fs::write(
        dir.csv(),
        csv.replace("Настройки проверки", "Настройки после перезагрузки"),
    )
    .unwrap();
    click(&mut app, &ctx, "Reload localization CSV");
    pump(&mut app, &ctx);
    assert!(Arc::ptr_eq(&applied, &app.session.snapshot()));
    assert_eq!(
        crate::i18n::tr_context(&ctx, "Settings"),
        "Настройки после перезагрузки"
    );
    assert!(matches!(&app.dialog,Some(Dialog::General{language,..})if language=="ru-RU"));
    general_language(&mut app, "en-US");
    fs::write(dir.csv(), "key,en-US,ru-RU\nA,a,b\nA,c,d\n").unwrap();
    click(&mut app, &ctx, "Apply");
    pump(&mut app, &ctx);
    assert!(app.error);
    assert!(Arc::ptr_eq(&applied, &app.session.snapshot()));
    assert_eq!(app.localization.language(), "ru-RU");
    assert!(matches!(&app.dialog,Some(Dialog::General{language,..})if language=="en-US"));
    fs::write(dir.csv(), csv).unwrap();
    click(&mut app, &ctx, "OK");
    pump(&mut app, &ctx);
    assert!(app.dialog.is_none());
    assert_eq!(app.session.snapshot().preferences().language(), "en-US");
    assert_eq!(app.localization.language(), "en-US");
    assert!(!app.session.can_undo());
}

#[test]
fn obsolete_async_settings_reply_cannot_install_catalog_or_close_replacement_dialog() {
    let dir = Directory::new();
    let ctx = egui::Context::default();
    let mut app = app(&ctx);
    app.localization_path = dir.csv();
    fs::write(
        dir.csv(),
        "key,en-US,ru-RU\nSettings,Settings,Изменённые настройки\n",
    )
    .unwrap();
    open(&mut app, &ctx, SettingsPage::General);
    general_language(&mut app, "ru-RU");
    click(&mut app, &ctx, "OK");
    assert!(app.io.busy());
    // A different owner/page while the I/O result is unread is not an acknowledgement.
    app.dialog = Some(settings::fresh(
        SettingsPage::Appearance,
        &app.session.snapshot(),
    ));
    let before = app.session.snapshot();
    pump(&mut app, &ctx);
    assert!(app.error);
    assert!(Arc::ptr_eq(&before, &app.session.snapshot()));
    assert_eq!(app.localization.language(), "en-US");
    assert!(matches!(app.dialog, Some(Dialog::Appearance(_))));
}

#[test]
fn active_stream_format_guard_preserves_settings_draft_then_retry_succeeds_after_actual_closed() {
    use crate::app::streaming;
    let ctx = egui::Context::default();
    let mut app = app(&ctx);
    let state = Arc::new(streaming::test_support::State::default());
    app.streaming =
        streaming::Runtime::with_factory(streaming::test_support::factory(state.clone()));
    streaming::test_support::configure(&mut app);
    app.intent(Intent::StartStreaming, &ctx);
    assert!(app.streaming.active());
    open(&mut app, &ctx, SettingsPage::Audio);
    click(&mut app, &ctx, "48 kHz");
    click(&mut app, &ctx, "44.1 kHz");
    let before = app.session.snapshot();
    click(&mut app, &ctx, "Apply");
    assert!(app.error);
    assert!(Arc::ptr_eq(&before, &app.session.snapshot()));
    assert!(matches!(
        app.dialog,
        Some(Dialog::AudioSettings {
            sample_rate: 44100,
            ..
        })
    ));
    state.closed.store(true, Ordering::SeqCst);
    app.poll_streaming(&ctx);
    click(&mut app, &ctx, "Apply");
    assert!(!app.error, "{}", app.status);
    assert_eq!(app.session.snapshot().audio().sample_rate(), 44100);
    assert!(app.dialog.is_some());
}

#[test]
fn stale_ok_from_cancelled_same_page_cannot_close_or_apply_new_window_generation() {
    let ctx = egui::Context::default();
    let mut app = app(&ctx);
    open(&mut app, &ctx, SettingsPage::Video);
    let token = app.settings_flow.token();
    let before = app.session.snapshot();
    click(&mut app, &ctx, "Cancel");
    open(&mut app, &ctx, SettingsPage::Video);
    assert_ne!(token, app.settings_flow.token());
    if let Some(Dialog::Video { width, .. }) = &mut app.dialog {
        *width = 256;
    }
    app.intent(
        Intent::SubmitSettings {
            page: SettingsPage::Video,
            action: SettingsAction::Ok,
            token,
        },
        &ctx,
    );
    assert!(app.error);
    assert!(Arc::ptr_eq(&before, &app.session.snapshot()));
    assert!(matches!(app.dialog, Some(Dialog::Video { width: 256, .. })));
    click(&mut app, &ctx, "OK");
    assert!(app.dialog.is_none());
    assert_eq!(app.session.snapshot().video().width(), 256);
}

#[test]
fn async_ok_for_reopened_identical_general_page_cannot_close_new_generation() {
    let dir = Directory::new();
    let ctx = egui::Context::default();
    let mut app = app(&ctx);
    app.localization_path = dir.csv();
    fs::write(dir.csv(), "key,en-US,ru-RU\nSettings,Settings,Настройки\n").unwrap();
    open(&mut app, &ctx, SettingsPage::General);
    general_language(&mut app, "ru-RU");
    click(&mut app, &ctx, "OK");
    let old = app.settings_flow.token();
    assert!(app.io.busy());
    // Simulate an externally replaced UI generation while an old reply is unread.
    let new = Dialog::General {
        snapping:crate::views::preview_snapping::Draft::new(skylive_core::PreviewSnapping::default()),
        projectors:crate::views::projector_settings::Draft::new(skylive_core::ProjectorPreferences::default()),
        language: "ru-RU".into(),
    };
    app.settings_flow.opened(&app.session, &new).unwrap();
    app.dialog = Some(new);
    assert_ne!(old, app.settings_flow.token());
    let before = app.session.snapshot();
    pump(&mut app, &ctx);
    assert!(app.error);
    assert!(Arc::ptr_eq(&before, &app.session.snapshot()));
    assert_eq!(app.localization.language(), "en-US");
    assert!(matches!(&app.dialog,Some(Dialog::General{language,..})if language=="ru-RU"));
}

use super::*;
use std::sync::atomic::{AtomicU64, Ordering};
use views::setup::{SetupAction, SetupOrigin};

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
fn fixture(ctx: &egui::Context) -> StudioApp {
    let mut app = StudioApp::headless(ctx).unwrap();
    app.recording.view.available = true;
    app.apply(Command::SetVideo {
        width: 64,
        height: 48,
        fps_num: 30,
        fps_den: 1,
    });
    app
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
        .unwrap_or_else(|| panic!("visible setup/menu control: {label}"));
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
struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        static SERIAL: AtomicU64 = AtomicU64::new(1);
        let path = std::env::temp_dir().join(format!(
            "skylive-app-setup-{}-{}",
            std::process::id(),
            SERIAL.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn file(&self) -> PathBuf {
        self.0.join("owned-preferences.json")
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        if self.file().exists() {
            std::fs::remove_file(self.file()).unwrap();
        }
        std::fs::remove_dir(&self.0).unwrap();
    }
}

#[test]
fn startup_gate_distinguishes_missing_from_legacy_existing_and_bad_installed_files_without_headless_auto_open()
 {
    let ctx = egui::Context::default();
    let mut app = StudioApp::headless(&ctx).unwrap();
    assert!(!app.setup.is_open());
    let directory = Directory::new();
    assert!(app.load_setup_preferences(&directory.file()).unwrap());
    assert!(
        !app.setup.is_open(),
        "only native constructor consumes the missing-file gate"
    );
    let legacy = serde_json::to_vec(&serde_json::json!({
        "schema": 1, "preferences": skylive_core::UserPreferences::default(),
    }))
    .unwrap();
    std::fs::write(directory.file(), &legacy).unwrap();
    assert!(
        !app.load_setup_preferences(&directory.file()).unwrap(),
        "legacy format1 preferences without metadata do not trigger setup"
    );
    assert_eq!(
        std::fs::read(directory.file()).unwrap(),
        legacy,
        "loading legacy preferences does not rewrite or promote their file"
    );
    let future = serde_json::to_vec(&serde_json::json!({
        "schema": skylive_host::user_preferences::CURRENT_SCHEMA + 1,
        "preferences": skylive_core::UserPreferences::default(),
    }))
    .unwrap();
    std::fs::write(directory.file(), &future).unwrap();
    assert!(!app.load_setup_preferences(&directory.file()).unwrap());
    assert_eq!(std::fs::read(directory.file()).unwrap(), future);
    assert!(!app.setup.is_open());
    let bad = b"broken original preferences";
    std::fs::write(directory.file(), bad).unwrap();
    assert!(!app.load_setup_preferences(&directory.file()).unwrap());
    assert_eq!(std::fs::read(directory.file()).unwrap(), bad);
    assert!(app.error);
    assert!(!app.setup.is_open());
}

#[test]
fn actual_tools_menu_wizard_back_review_apply_is_one_typed_publication_without_starting_outputs() {
    let ctx = egui::Context::default();
    let mut app = fixture(&ctx);
    click(&mut app, &ctx, "Tools");
    click(&mut app, &ctx, "Auto-Configuration Wizard");
    assert!(app.setup.is_open());
    let before = app.session.snapshot();
    let revision = before.revision();
    click(&mut app, &ctx, "Next");
    click(&mut app, &ctx, "1280 × 720 · 30 FPS");
    click(&mut app, &ctx, "Back");
    click(&mut app, &ctx, "Next");
    assert!(Arc::ptr_eq(&before, &app.session.snapshot()));
    click(&mut app, &ctx, "Next");
    click(&mut app, &ctx, "Next");
    click(&mut app, &ctx, "Apply");
    assert!(!app.error, "{}", app.status);
    assert!(!app.setup.is_open());
    let after = app.session.snapshot();
    assert_eq!((after.video().width(), after.video().height()), (1280, 720));
    assert_eq!(after.revision(), revision + 1);
    assert_eq!(
        after.preferences().setup_state(),
        skylive_core::SetupState::Completed
    );
    assert_eq!(after.scenes(), before.scenes());
    assert_eq!(after.sources(), before.sources());
    assert!(!app.recording.active() && !app.streaming.active());
    assert!(
        !app.session.can_undo(),
        "profile setup does not inject scene Undo"
    );
}

#[test]
fn first_run_cancel_is_unchanged_skip_persists_through_real_owned_preferences_worker() {
    let ctx = egui::Context::default();
    let mut app = fixture(&ctx);
    app.open_setup(SetupOrigin::FirstRun);
    let before = app.session.snapshot();
    click(&mut app, &ctx, "Cancel");
    assert!(Arc::ptr_eq(&before, &app.session.snapshot()));
    let directory = Directory::new();
    app.preferences_path = Some(directory.file());
    app.saved_preferences = before.preferences().clone();
    app.open_setup(SetupOrigin::FirstRun);
    click(&mut app, &ctx, "Skip setup");
    assert!(!app.setup.is_open());
    assert_eq!(
        app.session.snapshot().preferences().setup_state(),
        skylive_core::SetupState::Skipped
    );
    assert_eq!(app.session.snapshot().profiles(), before.profiles());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while (app.io.busy() || app.preferences_unsaved()) && std::time::Instant::now() < deadline {
        let _ = frame(&mut app, &ctx, vec![]);
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert!(
        !app.io.busy() && !app.preferences_unsaved(),
        "{}",
        app.status
    );
    assert_eq!(
        skylive_host::user_preferences::load(&directory.file())
            .unwrap()
            .unwrap()
            .setup_state(),
        skylive_core::SetupState::Skipped
    );
}

#[test]
fn older_wizard_apply_and_skip_cannot_change_reopened_or_replaced_target() {
    let ctx = egui::Context::default();
    let mut app = fixture(&ctx);
    app.open_setup(SetupOrigin::FirstRun);
    let old = app.setup.token().unwrap();
    app.setup_action(old, SetupAction::Cancel);
    app.open_setup(SetupOrigin::Tools);
    let before = app.session.snapshot();
    app.setup_action(old, SetupAction::Skip);
    assert!(app.setup.is_open());
    assert!(Arc::ptr_eq(&before, &app.session.snapshot()));
    let current = app.setup.token().unwrap();
    app.apply(Command::RenameCollection {
        name: "Unrelated revision".into(),
    });
    let changed = app.session.snapshot();
    app.setup_action(current, SetupAction::Apply);
    assert!(Arc::ptr_eq(&changed, &app.session.snapshot()));
    app.setup_action(current, SetupAction::Cancel);
    assert!(!app.setup.is_open());
}

#[test]
fn missing_preferences_do_not_restart_skipped_or_completed_setup() {
    let ctx = egui::Context::default();
    let mut app = fixture(&ctx);
    let directory = Directory::new();
    app.apply(Command::Setup {
        command: skylive_core::SetupCommand::Skip,
    });
    let skipped = app.session.snapshot();
    assert!(!app.load_setup_preferences(&directory.file()).unwrap());
    assert!(Arc::ptr_eq(&skipped, &app.session.snapshot()));
    let profile = skipped.profiles().selected();
    let configuration = skylive_core::SetupConfiguration::new(
        profile.id(),
        profile.video().clone(),
        profile.recording().clone(),
        profile.streaming().clone(),
    )
    .unwrap();
    app.apply(Command::Setup {
        command: skylive_core::SetupCommand::Apply { configuration },
    });
    let completed = app.session.snapshot();
    assert_eq!(
        completed.preferences().setup_state(),
        skylive_core::SetupState::Completed
    );
    assert!(!app.load_setup_preferences(&directory.file()).unwrap());
    assert!(Arc::ptr_eq(&completed, &app.session.snapshot()));
    assert!(!app.setup.is_open());
}

#[test]
fn pending_owned_io_blocks_setup_publication_but_current_cancel_still_closes() {
    let ctx = egui::Context::default();
    let mut app = fixture(&ctx);
    let directory = Directory::new();
    app.open_setup(SetupOrigin::FirstRun);
    let token = app.setup.token().unwrap();
    let before = app.session.snapshot();
    app.io
        .submit(
            app.session.identity(),
            Job::SavePreferences {
                path: directory.file(),
                preferences: before.preferences().clone(),
            },
        )
        .unwrap();
    assert!(
        app.io.busy(),
        "unconsumed completion is still an admitted operation"
    );
    app.setup_action(token, SetupAction::Skip);
    assert!(app.setup.is_open());
    assert!(Arc::ptr_eq(&before, &app.session.snapshot()));
    app.setup_action(token, SetupAction::Cancel);
    assert!(!app.setup.is_open());
    assert!(Arc::ptr_eq(&before, &app.session.snapshot()));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while app.io.busy() && std::time::Instant::now() < deadline {
        let _ = frame(&mut app, &ctx, vec![]);
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert!(!app.io.busy());
    assert_eq!(
        skylive_host::user_preferences::load(&directory.file())
            .unwrap()
            .unwrap()
            .setup_state(),
        skylive_core::SetupState::Unseen
    );
}

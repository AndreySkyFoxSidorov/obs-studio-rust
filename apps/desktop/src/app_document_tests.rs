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
            "skylive-document-flow-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        if self.0.parent() == Some(std::env::temp_dir().as_path()) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
}
fn pump(app: &mut StudioApp, context: &egui::Context) {
    let until = Instant::now() + Duration::from_secs(5);
    while app.io.busy() {
        app.poll_io(context);
        assert!(Instant::now() < until, "{}", app.status);
        std::thread::yield_now();
    }
}
fn current(context: &egui::Context) -> StudioApp {
    let mut app = StudioApp::headless(context).unwrap();
    app.apply(Command::SetVideo {
        width: 16,
        height: 16,
        fps_num: 30,
        fps_den: 1,
    });
    app.apply(Command::AddColorSource {
        scene: app.session.preview(),
        name: "Current red".into(),
        rgba: [255, 0, 0, 255],
    });
    app
}
fn blue(path: &std::path::Path) {
    let mut session = Session::new();
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
            scene: session.preview(),
            name: "Candidate blue".into(),
            rgba: [0, 0, 255, 255],
        })
        .unwrap();
    SaveJob::new(&session, path.into()).run().unwrap();
}
fn color(app: &StudioApp) -> [u8; 4] {
    skylive_host::preview::render(&app.session.snapshot(), app.session.program(), 16, 16)
        .unwrap()
        .pixels()[..4]
        .try_into()
        .unwrap()
}
#[test]
fn dirty_open_saves_bound_a_not_edited_target_b_then_atomically_publishes_candidate() {
    let dir = Directory::new();
    let a = dir.0.join("a.json");
    let b = dir.0.join("b.json");
    blue(&b);
    let original_b = fs::read(&b).unwrap();
    let context = egui::Context::default();
    let mut app = current(&context);
    app.intent(
        Intent::SaveAsTo {
            path: a.to_string_lossy().into_owned(),
        },
        &context,
    );
    pump(&mut app, &context);
    assert!(!app.error, "{}", app.status);
    app.apply(Command::AddScene {
        name: "New unsaved scene".into(),
    });
    app.apply(Command::Undo);
    assert!(
        app.session.is_document_dirty(),
        "Undo does not fake a matching save receipt"
    );
    let old_id = app.session.identity();
    let old = app.session.snapshot();
    app.document_path = b.to_string_lossy().into_owned();
    app.intent(Intent::Load, &context);
    pump(&mut app, &context);
    assert!(matches!(app.documents.prompt, Some(Prompt::Replace { .. })));
    assert_eq!(app.session.identity(), old_id);
    assert!(Arc::ptr_eq(&old, &app.session.snapshot()));
    assert_eq!(color(&app), [255, 0, 0, 255]);
    app.intent(Intent::ResolveDocument(Choice::Save), &context);
    assert_eq!(app.session.identity(), old_id);
    pump(&mut app, &context);
    assert!(!app.error, "{}", app.status);
    assert_ne!(app.session.identity(), old_id);
    assert_eq!(color(&app), [0, 0, 255, 255]);
    assert_eq!(fs::read(&b).unwrap(), original_b);
    assert_eq!(
        app.active_document_path,
        Some(fs::canonicalize(&b).unwrap())
    );
    let saved = skylive_host::storage::load(&a).unwrap();
    assert_eq!(saved.snapshot().sources()[0].name(), "Current red");
}
#[test]
fn malformed_candidate_and_cancel_preserve_dirty_session_program_and_undo() {
    let dir = Directory::new();
    let bad = dir.0.join("bad.json");
    fs::write(&bad, b"{invalid}").unwrap();
    let context = egui::Context::default();
    let mut app = current(&context);
    let id = app.session.identity();
    let model = app.session.snapshot();
    app.document_path = bad.to_string_lossy().into_owned();
    app.intent(Intent::Load, &context);
    pump(&mut app, &context);
    assert!(app.error);
    assert!(Arc::ptr_eq(&model, &app.session.snapshot()));
    assert_eq!(app.session.identity(), id);
    assert!(app.session.is_document_dirty() && app.session.can_undo());
    assert!(app.documents.prompt.is_none());
    app.intent(Intent::NewCollection, &context);
    assert!(matches!(app.documents.prompt, Some(Prompt::Replace { .. })));
    app.intent(Intent::ResolveDocument(Choice::Cancel), &context);
    assert!(Arc::ptr_eq(&model, &app.session.snapshot()));
    assert_eq!(color(&app), [255, 0, 0, 255]);
}
#[test]
fn untitled_save_before_open_never_defaults_to_target_and_failed_save_keeps_candidate() {
    let dir = Directory::new();
    let b = dir.0.join("b.json");
    blue(&b);
    let original = fs::read(&b).unwrap();
    let context = egui::Context::default();
    let mut app = current(&context);
    app.document_path = b.to_string_lossy().into_owned();
    let id = app.session.identity();
    app.intent(Intent::Load, &context);
    pump(&mut app, &context);
    app.intent(Intent::ResolveDocument(Choice::Save), &context);
    assert!(
        matches!(&app.documents.prompt,Some(Prompt::SaveAs{path})if path=="untitled.skylive.json")
    );
    assert!(app.active_document_path.is_none());
    let bad = dir.0.join("missing").join("save.json");
    app.intent(
        Intent::SaveAsTo {
            path: bad.to_string_lossy().into_owned(),
        },
        &context,
    );
    pump(&mut app, &context);
    assert!(app.error);
    assert_eq!(app.session.identity(), id);
    assert!(app.session.is_document_dirty());
    assert!(matches!(app.documents.prompt, Some(Prompt::Replace { .. })));
    assert_eq!(fs::read(&b).unwrap(), original);
    app.intent(Intent::ResolveDocument(Choice::Discard), &context);
    assert_eq!(color(&app), [0, 0, 255, 255]);
    assert_eq!(
        app.active_document_path,
        Some(fs::canonicalize(&b).unwrap())
    );
}
#[test]
fn import_has_no_native_binding_and_recovery_keeps_its_checkpoint_until_real_save() {
    let dir = Directory::new();
    let obs = dir.0.join("reference.obs.json");
    let bytes = include_bytes!("../../../crates/host/tests/fixtures/obs-color-absolute.json");
    fs::write(&obs, bytes).unwrap();
    let context = egui::Context::default();
    let mut app = StudioApp::headless(&context).unwrap();
    app.intent(
        Intent::ImportObs {
            path: obs.to_string_lossy().into_owned(),
        },
        &context,
    );
    pump(&mut app, &context);
    assert!(!app.error, "{}", app.status);
    assert!(app.active_document_path.is_none());
    app.intent(Intent::Save, &context);
    assert!(
        matches!(&app.documents.prompt,Some(Prompt::SaveAs{path})if path=="untitled.skylive.json")
    );
    assert_eq!(fs::read(&obs).unwrap(), bytes);
    app.intent(Intent::CancelDocumentSave, &context);
    let base = dir.0.join("recovered.json");
    let mut recover = Session::new();
    recover
        .apply(Command::AddScene {
            name: "Recovered graph".into(),
        })
        .unwrap();
    SaveJob::new(&recover, base.clone()).checkpoint().unwrap();
    app.document_path = base.to_string_lossy().into_owned();
    app.intent(Intent::Recover, &context);
    pump(&mut app, &context);
    assert!(matches!(app.documents.prompt, Some(Prompt::Replace { .. })));
    app.intent(Intent::ResolveDocument(Choice::Discard), &context);
    assert!(app.session.is_document_dirty());
    assert!(skylive_host::storage::recovery_path(&base).exists());
    assert_eq!(
        app.active_document_path,
        Some(fs::canonicalize(&dir.0).unwrap().join("recovered.json"))
    );
    app.intent(Intent::Save, &context);
    pump(&mut app, &context);
    assert!(!app.error, "{}", app.status);
    assert!(!app.session.is_document_dirty());
    assert!(base.is_file());
}
#[test]
fn obsolete_prepared_load_and_receipt_cannot_replace_or_rebind_newer_owner() {
    let context = egui::Context::default();
    let mut app = current(&context);
    let old_session = app.session.identity();
    let old_revision = app.session.snapshot().revision();
    let stale = Candidate {
        session: old_session,
        revision: old_revision,
        label: "obsolete".into(),
        value: Prepared::New(Session::new()),
    };
    app.documents.candidate = Some(stale);
    app.apply(Command::AddScene {
        name: "Newer edit".into(),
    });
    let latest = app.session.snapshot();
    app.publish_document(&context);
    assert!(Arc::ptr_eq(&latest, &app.session.snapshot()));
    assert_eq!(app.session.identity(), old_session);
    let dir = Directory::new();
    let path = dir.0.join("receipt.json");
    let receipt = SaveJob::new(&app.session, path.clone()).run().unwrap();
    let revision = app.session.snapshot().revision();
    app.documents.save = Some(Save {
        request: 12,
        session: old_session,
        revision,
    });
    app.saved_document(11, path.clone(), receipt, None, &context);
    assert!(app.documents.save.is_some());
    assert!(app.active_document_path.is_none());
    let receipt = SaveJob::new(&app.session, path.clone()).run().unwrap();
    app.apply(Command::AddScene {
        name: "Later edit".into(),
    });
    app.saved_document(12, path, receipt, None, &context);
    assert!(app.active_document_path.is_none());
    assert!(app.session.is_document_dirty());
}
#[test]
fn replacement_and_explicit_save_footers_fit_five_languages_at_minimum_and_cancel_by_real_pointer()
{
    for scale in [1.0, 1.5, 2.0] {
        for language in ["en-US", "es-ES", "ru-RU", "uk-UA", "de-DE"] {
            let context = egui::Context::default();
            crate::theme::install(&context);
            context.set_zoom_factor(scale);
            let catalog =
                Arc::new(skylive_host::localization::Localization::embedded(language).unwrap());
            crate::i18n::install(&context, catalog, &crate::i18n::workspace_path());
            for save in [false, true] {
                let mut prompt = if save {
                    Prompt::SaveAs {
                        path: "C:/example/native.skylive.json".into(),
                    }
                } else {
                    Prompt::Replace {
                        target: "C:/example/candidate.skylive.json".into(),
                    }
                };
                let viewport = egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(800.0 / scale, 560.0 / scale),
                );
                let mut result = None;
                for _ in 0..3 {
                    let _ = context.run(
                        egui::RawInput {
                            screen_rect: Some(viewport),
                            ..Default::default()
                        },
                        |ctx| {
                            result = views::document::show(ctx, &mut prompt, false);
                        },
                    );
                }
                let last = if save { 1 } else { 2 };
                for index in 0..=last {
                    let rect = context
                        .data(|d| {
                            d.get_temp::<egui::Rect>(egui::Id::new(("modal-action-rect", index)))
                        })
                        .unwrap();
                    assert!(
                        viewport.expand(1.0).contains_rect(rect),
                        "{language} scale={scale} save={save} {rect:?}"
                    );
                }
                let rect = context
                    .data(|d| d.get_temp::<egui::Rect>(egui::Id::new(("modal-action-rect", last))))
                    .unwrap();
                for pressed in [true, false] {
                    let _ = context.run(
                        egui::RawInput {
                            screen_rect: Some(viewport),
                            events: vec![
                                egui::Event::PointerMoved(rect.center()),
                                egui::Event::PointerButton {
                                    pos: rect.center(),
                                    button: egui::PointerButton::Primary,
                                    pressed,
                                    modifiers: Default::default(),
                                },
                            ],
                            ..Default::default()
                        },
                        |ctx| {
                            result = views::document::show(ctx, &mut prompt, false);
                        },
                    );
                }
                assert!(
                    matches!(
                        result,
                        Some(Intent::CancelDocumentSave)
                            | Some(Intent::ResolveDocument(Choice::Cancel))
                    ),
                    "actual Cancel click {language} save={save}"
                );
            }
        }
    }
}

#[test]
fn stray_confirmed_save_cannot_bypass_replacement_choice_or_hide_candidate() {
    let dir = Directory::new();
    let path = dir.0.join("must-not-save.json");
    let context = egui::Context::default();
    let mut app = current(&context);
    let snapshot = app.session.snapshot();
    app.intent(Intent::NewCollection, &context);
    assert!(matches!(app.documents.prompt, Some(Prompt::Replace { .. })));
    app.intent(
        Intent::SaveAsTo {
            path: path.to_string_lossy().into_owned(),
        },
        &context,
    );
    assert!(app.error);
    assert!(!app.io.busy());
    assert!(!path.exists());
    assert!(app.active_document_path.is_none());
    assert!(Arc::ptr_eq(&snapshot, &app.session.snapshot()));
    assert!(matches!(app.documents.prompt, Some(Prompt::Replace { .. })));
    assert!(app.documents.candidate.is_some());
}
#[test]
fn failed_io_submission_preserves_save_then_replace_or_close_for_a_real_retry() {
    for replace in [true, false] {
        let dir = Directory::new();
        let path = dir.0.join("retry.json");
        let context = egui::Context::default();
        let mut app = current(&context);
        let old = app.session.identity();
        if replace {
            app.documents.candidate = Some(Candidate {
                session: old,
                revision: app.session.snapshot().revision(),
                label: "New collection".into(),
                value: Prepared::New(app.session.new_collection().unwrap()),
            });
        }
        let after = if replace {
            AfterIo::SaveAndReplace
        } else {
            AfterIo::SaveAndClose
        };
        app.open_save_as(Some(after));
        app.io.close();
        app.intent(
            Intent::SaveAsTo {
                path: path.to_string_lossy().into_owned(),
            },
            &context,
        );
        assert!(app.error);
        assert!(!path.exists());
        assert!(matches!(app.documents.prompt, Some(Prompt::SaveAs { .. })));
        assert!(if replace {
            matches!(app.documents.save_after, Some(AfterIo::SaveAndReplace))
        } else {
            matches!(app.documents.save_after, Some(AfterIo::SaveAndClose))
        });
        app.io = IoWorker::new(context.clone()).unwrap();
        app.intent(
            Intent::SaveAsTo {
                path: path.to_string_lossy().into_owned(),
            },
            &context,
        );
        pump(&mut app, &context);
        assert!(path.is_file());
        if replace {
            assert_ne!(app.session.identity(), old);
            assert!(app.session.snapshot().sources().is_empty());
        } else {
            assert!(app.allow_close && app.session.is_closed());
        }
    }
}

include!("app_file_ui_tests.rs");

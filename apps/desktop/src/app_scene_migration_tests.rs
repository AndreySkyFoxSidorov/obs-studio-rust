//! Actual worker load completion exposes semantic migration without scene undo.
use super::*;
#[test]
fn schema3_scene_filter_load_opens_bounded_receipt_and_stays_dirty_until_save() {
    static SERIAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let path = std::env::temp_dir().join(format!(
        "skylive-scene-migration-{}-{}.json",
        std::process::id(),
        SERIAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    struct File(std::path::PathBuf);
    impl Drop for File {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    let file = File(path);
    let mut source = Session::new();
    let scene = source.preview();
    source
        .apply(Command::SetSceneVideoFilters {
            scene,
            filters: vec![
                skylive_core::VideoFilter::new(skylive_core::VideoEffect::color_correction())
                    .unwrap(),
            ],
        })
        .unwrap();
    let mut value: serde_json::Value =
        serde_json::from_slice(&skylive_host::storage::encode(&source.snapshot()).unwrap())
            .unwrap();
    value["schema"] = serde_json::json!(3);
    std::fs::write(&file.0, serde_json::to_vec(&value).unwrap()).unwrap();
    let context = egui::Context::default();
    let mut app = StudioApp::headless(&context).unwrap();
    assert!(
        app.session
            .mark_saved(app.session.identity(), app.session.snapshot().revision())
    );
    app.document_path = file.0.to_string_lossy().into_owned();
    app.intent(Intent::Load, &context);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while app.io.busy() {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(2));
        app.poll_io(&context);
    }
    let Some(Dialog::NativeMigration(report)) = &app.dialog else {
        panic!(
            "semantic load must show compatibility receipt: {}",
            app.status
        )
    };
    assert_eq!(report.scene_source_filters.len(), 1);
    assert_eq!(report.scene_source_filters[0].scene, scene);
    assert!(report.retained_bytes() <= skylive_host::storage::MAX_MIGRATION_REPORT_BYTES);
    assert!(app.session.is_dirty());
    assert!(app.session.is_document_dirty());
    assert!(app.error);
    assert!(app.status.contains("compatibility"));
    assert!(matches!(
        app.session.apply(Command::Undo),
        Err(skylive_core::Error::EmptyHistory)
    ));
    assert_eq!(
        app.session
            .snapshot()
            .scene_source(scene)
            .unwrap()
            .video_filters()
            .len(),
        1
    );
}

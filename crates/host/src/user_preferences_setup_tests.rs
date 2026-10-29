use super::*;
use skylive_core::{Command, Session, SetupCommand, SetupState};
use std::sync::atomic::{AtomicU64, Ordering};

#[test]
fn setup_installed_preferences_owned_save_reopen_and_malformed_file_preservation() {
    static SERIAL: AtomicU64 = AtomicU64::new(1);
    let directory = std::env::temp_dir().join(format!(
        "skylive-setup-prefs-{}-{}",
        std::process::id(),
        SERIAL.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&directory).unwrap();
    let path = directory.join("owned-preferences.json");
    assert!(load(&path).unwrap().is_none());
    let mut session = Session::new();
    session
        .apply(Command::Setup {
            command: SetupCommand::Skip,
        })
        .unwrap();
    let preferences = session.snapshot().preferences().clone();
    save(&path, &preferences).unwrap();
    let loaded = load(&path).unwrap().unwrap();
    assert_eq!(loaded, preferences);
    assert_eq!(loaded.setup_state(), SetupState::Skipped);
    let wire: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(wire["schema"], CURRENT_SCHEMA);
    let mut bad = wire;
    bad["preferences"]["setup_state"] = "future-value".into();
    let bad = serde_json::to_vec(&bad).unwrap();
    fs::write(&path, &bad).unwrap();
    assert!(load(&path).is_err());
    assert_eq!(fs::read(&path).unwrap(), bad);
    fs::remove_file(&path).unwrap();
    fs::remove_dir(&directory).unwrap();
}

#[test]
fn installed_format1_defaults_stay_exact_but_nondefault_setup_and_future_formats_reject() {
    static SERIAL: AtomicU64 = AtomicU64::new(1);
    let directory = std::env::temp_dir().join(format!(
        "skylive-setup-format-{}-{}",
        std::process::id(),
        SERIAL.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&directory).unwrap();
    let path = directory.join("owned-preferences.json");
    let legacy = serde_json::to_vec(&Envelope {
        schema: 1,
        preferences: UserPreferences::default(),
    })
    .unwrap();
    fs::write(&path, &legacy).unwrap();
    let loaded = load(&path).unwrap().unwrap();
    assert_eq!(loaded, UserPreferences::default());
    assert_eq!(loaded.setup_state(), SetupState::Unseen);
    assert!(loaded.setup_on_new_profile());
    assert_eq!(
        fs::read(&path).unwrap(),
        legacy,
        "legacy loading never rewrites or auto-promotes"
    );
    for (field, value) in [
        ("setup_state", serde_json::json!("skipped")),
        ("setup_state", serde_json::json!("completed")),
        ("setup_on_new_profile", serde_json::json!(false)),
    ] {
        let mut preferences = serde_json::to_value(UserPreferences::default()).unwrap();
        preferences[field] = value;
        let mut current = serde_json::json!({"schema":CURRENT_SCHEMA,"preferences":preferences});
        let admitted = decode(&serde_json::to_vec(&current).unwrap()).unwrap();
        assert!(admitted.has_setup_metadata());
        current["schema"] = 1.into();
        let backdated = serde_json::to_vec(&current).unwrap();
        fs::write(&path, &backdated).unwrap();
        let error = load(&path).unwrap_err();
        assert_eq!(error.code(), "user_preferences_schema");
        assert_eq!(error.phase, Phase::BeforePublication);
        assert!(error.detail().contains("require user-preferences schema2"));
        assert_eq!(fs::read(&path).unwrap(), backdated);
        assert!(!storage::recovery_path(&path).exists());
    }
    let future = serde_json::to_vec(
        &serde_json::json!({"schema":CURRENT_SCHEMA+1,"preferences":UserPreferences::default()}),
    )
    .unwrap();
    fs::write(&path, &future).unwrap();
    assert_eq!(load(&path).unwrap_err().code(), "user_preferences_schema");
    assert_eq!(fs::read(&path).unwrap(), future);
    assert!(!storage::recovery_path(&path).exists());
    fs::remove_file(&path).unwrap();
    fs::remove_dir(&directory).unwrap();
}

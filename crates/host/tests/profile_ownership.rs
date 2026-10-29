//! Public host/core ownership and native migration boundary. No devices or UI.
use serde_json::{Value, json};
use skylive_core::{
    AudioBuffering, AudioSettings, Command, Id, ProfileCommand, ProfileId, Session, SpeakerLayout,
    VideoSettings,
};
use skylive_host::storage::{self, Phase, SaveJob};
use std::{
    fs,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

static SERIAL: AtomicU64 = AtomicU64::new(1);
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "skylive-profile-ownership-{}-{}",
            std::process::id(),
            SERIAL.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn profile(session: &mut Session, command: ProfileCommand) {
    session.apply(Command::Profile { command }).unwrap();
}
fn audio(session: &mut Session, rate: u32, speakers: SpeakerLayout, buffering: AudioBuffering) {
    session
        .apply(Command::SetAudioSettings {
            settings: AudioSettings::new(rate, speakers)
                .unwrap()
                .with_buffering(buffering)
                .unwrap(),
        })
        .unwrap();
}
fn scene(session: &mut Session, name: &str) -> Id {
    session
        .apply(Command::AddScene { name: name.into() })
        .unwrap()
        .created
        .unwrap()
}
fn encode(session: &Session) -> Value {
    serde_json::from_slice(&storage::encode(&session.snapshot()).unwrap()).unwrap()
}
fn legacy(session: &Session) -> Value {
    let snapshot = session.snapshot();
    let current = encode(session);
    let mut graph = current["collection"].clone();
    let fields = graph.as_object_mut().unwrap();
    fields.insert("revision".into(), current["revision"].clone());
    fields.insert(
        "video".into(),
        serde_json::to_value(snapshot.video()).unwrap(),
    );
    fields.insert(
        "audio".into(),
        serde_json::to_value(snapshot.audio()).unwrap(),
    );
    json!({"schema":1,"collection":graph,"selection":{"preview":session.preview(),"program":session.program()}})
}
fn decode(value: &Value, saved: bool) -> Session {
    storage::decode(&serde_json::to_vec(value).unwrap(), saved).unwrap()
}
fn rejected(value: &Value) {
    let error = storage::decode(&serde_json::to_vec(value).unwrap(), true)
        .err()
        .expect("malformed candidate must reject");
    assert!(matches!(error.phase, Phase::BeforePublication));
}

#[test]
fn profile_formats_and_user_buffering_have_one_owner_outside_the_scene_graph() {
    let mut session = Session::new();
    let live = session.preview();
    session
        .apply(Command::AddColorSource {
            scene: live,
            name: "Authored source".into(),
            rgba: [1, 2, 3, 255],
        })
        .unwrap();
    let graph = session.snapshot().collection().clone();
    let initial = session.snapshot().profiles().selected_id();
    let user = AudioBuffering::new(209, false).unwrap();
    audio(&mut session, 44100, SpeakerLayout::Mono, user);
    session
        .apply(Command::SetVideo {
            width: 1280,
            height: 720,
            fps_num: 60000,
            fps_den: 1001,
        })
        .unwrap();
    assert_eq!(session.snapshot().collection(), &graph);
    assert_eq!(session.snapshot().preferences().audio_buffering(), user);
    profile(
        &mut session,
        ProfileCommand::Duplicate {
            name: "Copy".into(),
        },
    );
    let copied = session.snapshot().profiles().selected_id();
    assert_ne!(copied, initial);
    assert_eq!(
        session.snapshot().profiles().selected().sample_rate(),
        44100
    );
    assert_eq!(session.snapshot().video().width(), 1280);
    profile(
        &mut session,
        ProfileCommand::New {
            name: "Fresh".into(),
        },
    );
    assert_eq!(
        session.snapshot().profiles().selected().sample_rate(),
        48000
    );
    assert_eq!(
        session.snapshot().profiles().selected().speakers(),
        SpeakerLayout::Stereo
    );
    assert_eq!(session.snapshot().video(), &VideoSettings::default());
    assert_eq!(
        session.snapshot().audio().buffering(),
        user,
        "new profile must not reset a user-global preference"
    );
    profile(&mut session, ProfileCommand::Select { id: initial });
    assert_eq!(session.snapshot().audio().sample_rate(), 44100);
    assert_eq!(session.snapshot().audio().speakers(), SpeakerLayout::Mono);
    assert_eq!(session.snapshot().video().fps_den(), 1001);
    assert_eq!(session.snapshot().collection(), &graph);
    let saved = encode(&session);
    assert_eq!(saved["schema"], storage::CURRENT_SCHEMA);
    assert!(saved["revision"].is_u64());
    for key in ["audio", "video", "revision", "profiles", "preferences"] {
        assert!(
            saved["collection"].get(key).is_none(),
            "{key} leaked into scene collection"
        );
    }
    assert_eq!(
        saved["preferences"]["audio_buffering"],
        json!({"max_ms":209,"fixed":false})
    );
    for entry in saved["profiles"]["profiles"].as_array().unwrap() {
        assert!(entry.get("buffering").is_none());
        assert!(entry.get("audio_buffering").is_none());
        assert!(entry.get("video").is_some());
    }
}

#[test]
fn scene_undo_redo_never_rewinds_profile_or_user_settings_and_profile_failure_is_atomic() {
    let mut session = Session::new();
    let added = scene(&mut session, "Scene history");
    audio(
        &mut session,
        44100,
        SpeakerLayout::FivePointOne,
        AudioBuffering::low_latency(),
    );
    profile(
        &mut session,
        ProfileCommand::Duplicate {
            name: "Production".into(),
        },
    );
    session
        .apply(Command::SetVideo {
            width: 1280,
            height: 720,
            fps_num: 60,
            fps_den: 1,
        })
        .unwrap();
    let settings = session.snapshot();
    let catalog = settings.profiles().clone();
    let preferences = settings.preferences().clone();
    let effective_audio = settings.audio();
    let video = settings.video().clone();
    session.apply(Command::Undo).unwrap();
    assert!(session.snapshot().scene(added).is_none());
    assert_eq!(session.snapshot().profiles(), &catalog);
    assert_eq!(session.snapshot().preferences(), &preferences);
    assert_eq!(session.snapshot().audio(), effective_audio);
    assert_eq!(session.snapshot().video(), &video);
    session.apply(Command::Redo).unwrap();
    assert!(session.snapshot().scene(added).is_some());
    assert_eq!(session.snapshot().profiles(), &catalog);
    assert_eq!(session.snapshot().preferences(), &preferences);
    let before = session.snapshot();
    let selected = before.profiles().selected_id();
    assert!(
        !session
            .apply(Command::Profile {
                command: ProfileCommand::Select { id: selected }
            })
            .unwrap()
            .changed
    );
    assert!(Arc::ptr_eq(&before, &session.snapshot()));
    assert!(
        session
            .apply(Command::Profile {
                command: ProfileCommand::Rename {
                    id: selected,
                    name: "Untitled".into()
                }
            })
            .is_err()
    );
    assert!(Arc::ptr_eq(&before, &session.snapshot()));
    let mut settings_only = Session::new();
    audio(
        &mut settings_only,
        44100,
        SpeakerLayout::Mono,
        AudioBuffering::low_latency(),
    );
    profile(
        &mut settings_only,
        ProfileCommand::New {
            name: "Only profile edits".into(),
        },
    );
    let before = settings_only.snapshot();
    assert!(matches!(
        settings_only.apply(Command::Undo),
        Err(skylive_core::Error::EmptyHistory)
    ));
    assert!(Arc::ptr_eq(&before, &settings_only.snapshot()));
}

#[test]
fn new_and_imported_collections_preserve_existing_profiles_and_preferences_under_new_identity() {
    let mut owner = Session::new();
    audio(
        &mut owner,
        44100,
        SpeakerLayout::Mono,
        AudioBuffering::new(209, false).unwrap(),
    );
    profile(
        &mut owner,
        ProfileCommand::Duplicate {
            name: "Existing settings".into(),
        },
    );
    let before = owner.snapshot();
    let fresh = owner.new_collection().unwrap();
    assert_ne!(fresh.identity(), owner.identity());
    assert_eq!(fresh.snapshot().profiles(), before.profiles());
    assert_eq!(fresh.snapshot().preferences(), before.preferences());
    assert!(fresh.snapshot().sources().is_empty());
    let mut imported = Session::new();
    let program = scene(&mut imported, "Imported Program");
    let preview = scene(&mut imported, "Imported Preview");
    imported.select_program(program).unwrap();
    imported.select_preview(preview).unwrap();
    imported
        .apply(Command::SetVideo {
            width: 640,
            height: 360,
            fps_num: 25,
            fps_den: 1,
        })
        .unwrap();
    let imported_identity = imported.identity();
    let imported_graph = imported.snapshot().collection().clone();
    let adopted = owner.adopt_collection(imported).unwrap();
    assert_ne!(adopted.identity(), owner.identity());
    assert_ne!(adopted.identity(), imported_identity);
    assert_eq!(adopted.snapshot().collection(), &imported_graph);
    assert_eq!(adopted.snapshot().profiles(), before.profiles());
    assert_eq!(adopted.snapshot().preferences(), before.preferences());
    assert_eq!(adopted.snapshot().audio(), before.audio());
    assert_eq!(adopted.snapshot().video(), before.video());
    assert_eq!(adopted.program(), program);
    assert_eq!(adopted.preview(), preview);
    assert!(adopted.is_dirty());
    assert!(Arc::ptr_eq(&before, &owner.snapshot()));
}

#[test]
fn save_reopen_preserves_all_scopes_and_stale_profile_receipt_cannot_mark_newer_state_saved() {
    let temp = Temp::new();
    let path = temp.0.join("studio.json");
    let mut session = Session::new();
    let preview = scene(&mut session, "Preview");
    session.select_preview(preview).unwrap();
    profile(
        &mut session,
        ProfileCommand::Duplicate {
            name: "Saved profile".into(),
        },
    );
    let captured = session.snapshot();
    let job = SaveJob::new(&session, path.clone());
    audio(
        &mut session,
        44100,
        SpeakerLayout::Mono,
        AudioBuffering::low_latency(),
    );
    let receipt = job.run().unwrap();
    assert!(!receipt.accept(&mut session));
    assert!(session.is_dirty());
    let reopened = storage::load(&path).unwrap();
    assert_ne!(reopened.identity(), session.identity());
    assert_eq!(*reopened.snapshot(), *captured);
    assert_eq!(reopened.preview(), preview);
    assert!(!reopened.is_dirty());
    assert_eq!(reopened.snapshot().audio().sample_rate(), 48000);
    assert_eq!(session.snapshot().audio().sample_rate(), 44100);
    let identity_path = temp.0.join("identity.json");
    let old = Session::new();
    let receipt = SaveJob::new(&old, identity_path).run().unwrap();
    let mut replacement = old.new_collection().unwrap();
    assert_eq!(replacement.snapshot().revision(), old.snapshot().revision());
    assert!(
        !receipt.accept(&mut replacement),
        "a renewed collection identity must reject an old save completion"
    );
}

#[test]
fn recovery_preserves_profiles_global_preferences_and_selection_without_reusing_runtime_identity() {
    let temp = Temp::new();
    let path = temp.0.join("recovery.json");
    let mut session = Session::new();
    let target = scene(&mut session, "Recovered Program");
    session.select_program(target).unwrap();
    profile(
        &mut session,
        ProfileCommand::Duplicate {
            name: "Recovery profile".into(),
        },
    );
    audio(
        &mut session,
        44100,
        SpeakerLayout::SevenPointOne,
        AudioBuffering::new(209, false).unwrap(),
    );
    SaveJob::new(&session, path.clone()).checkpoint().unwrap();
    assert!(storage::recovery_path(&path).exists());
    let mut first = storage::recover(&path).unwrap();
    let second = storage::recover(&path).unwrap();
    assert_ne!(first.identity(), session.identity());
    assert_ne!(first.identity(), second.identity());
    assert_eq!(*first.snapshot(), *session.snapshot());
    assert_eq!(first.program(), target);
    assert_eq!(first.preview(), session.preview());
    assert!(first.is_dirty());
    assert!(second.is_dirty());
    assert!(storage::recovery_path(&path).exists());
    assert!(
        SaveJob::new(&first, path.clone())
            .run()
            .unwrap()
            .accept(&mut first)
    );
    assert!(!first.is_dirty());
    assert!(!storage::recovery_path(&path).exists());
    assert_eq!(*storage::load(&path).unwrap().snapshot(), *first.snapshot());
}

#[test]
fn legacy_flat_schema_migrates_video_audio_and_buffering_to_their_owners() {
    for policy in [
        AudioBuffering::default(),
        AudioBuffering::low_latency(),
        AudioBuffering::new(209, false).unwrap(),
    ] {
        let mut old = Session::new();
        let selected = scene(&mut old, "Legacy Program");
        old.select_program(selected).unwrap();
        old.apply(Command::SetVideo {
            width: 1280,
            height: 720,
            fps_num: 60000,
            fps_den: 1001,
        })
        .unwrap();
        audio(&mut old, 44100, SpeakerLayout::FivePointOne, policy);
        let fixture = legacy(&old);
        let migrated = decode(&fixture, true);
        assert_eq!(migrated.snapshot().profiles().entries().len(), 1);
        assert_eq!(migrated.snapshot().audio(), old.snapshot().audio());
        assert_eq!(migrated.snapshot().video(), old.snapshot().video());
        assert_eq!(migrated.snapshot().preferences().audio_buffering(), policy);
        assert_eq!(
            migrated.snapshot().collection(),
            old.snapshot().collection()
        );
        assert_eq!(migrated.snapshot().revision(), old.snapshot().revision());
        assert_eq!(migrated.program(), selected);
        let v2 = encode(&migrated);
        assert_eq!(v2["schema"], storage::CURRENT_SCHEMA);
        assert_eq!(
            v2["preferences"]["audio_buffering"],
            serde_json::to_value(policy).unwrap()
        );
        assert!(v2["collection"].get("audio").is_none());
        assert!(v2["collection"].get("video").is_none());
    }
    let mut missing = legacy(&Session::new());
    missing["collection"]
        .as_object_mut()
        .unwrap()
        .remove("audio");
    let migrated = decode(&missing, true);
    assert_eq!(migrated.snapshot().audio(), AudioSettings::default());
    let mut missing_buffer = legacy(&Session::new());
    missing_buffer["collection"]["audio"]
        .as_object_mut()
        .unwrap()
        .remove("buffering");
    assert_eq!(
        decode(&missing_buffer, true)
            .snapshot()
            .preferences()
            .audio_buffering(),
        AudioBuffering::default()
    );
}

#[test]
fn schema_two_strictly_rejects_wrong_scope_unknown_fields_and_invalid_profile_candidates() {
    let owner = Session::new();
    let before = owner.snapshot();
    let valid = encode(&owner);
    let mut variants = Vec::new();
    let mut value = valid.clone();
    value["schema"] = json!(storage::CURRENT_SCHEMA + 1);
    variants.push(value);
    let mut value = valid.clone();
    value["unexpected"] = json!(true);
    variants.push(value);
    let mut value = valid.clone();
    value["collection"]["audio"] = json!({"sample_rate":48000});
    variants.push(value);
    let mut value = valid.clone();
    value["collection"]["video"] = json!({"width":640});
    variants.push(value);
    let mut value = valid.clone();
    value["profiles"]["profiles"][0]["buffering"] = json!({"max_ms":20,"fixed":true});
    variants.push(value);
    let mut value = valid.clone();
    value["profiles"]["profiles"][0]["sample_rate"] = json!(96000);
    variants.push(value);
    let mut value = valid.clone();
    value["profiles"]["selected"] = json!(999);
    variants.push(value);
    let mut value = valid.clone();
    value["preferences"]["audio_buffering"]["max_ms"] = json!(1001);
    variants.push(value);
    let mut value = valid.clone();
    value["preferences"]["audio_buffering"]["fixed"] = json!("false");
    variants.push(value);
    for candidate in variants {
        rejected(&candidate);
        assert!(Arc::ptr_eq(&before, &owner.snapshot()));
    }
    assert!(ProfileId::new(0).is_err());
}

#[test]
fn legacy_migration_stays_strict_instead_of_dropping_unknown_or_invalid_data() {
    let valid = legacy(&Session::new());
    let mut variants = Vec::new();
    let mut value = valid.clone();
    value["collection"]["profiles"] = json!([]);
    variants.push(value);
    let mut value = valid.clone();
    value["collection"]["audio"]["unexpected"] = json!(true);
    variants.push(value);
    let mut value = valid.clone();
    value["collection"]["audio"]["sample_rate"] = json!(96000);
    variants.push(value);
    let mut value = valid.clone();
    value["collection"]["audio"]["buffering"]["max_ms"] = json!(1001);
    variants.push(value);
    let mut value = valid.clone();
    value["collection"]["video"]["width"] = json!(0);
    variants.push(value);
    let mut value = valid.clone();
    value["selection"]["program"] = json!(999);
    variants.push(value);
    for candidate in variants {
        rejected(&candidate);
    }
}

#[test]
fn closed_application_owners_cannot_admit_collection_replacement() {
    let mut owner = Session::new();
    let before = owner.snapshot();
    let mut closed_import = Session::new();
    closed_import.close();
    assert!(matches!(
        owner.adopt_collection(closed_import),
        Err(skylive_core::Error::Closed)
    ));
    assert!(Arc::ptr_eq(&before, &owner.snapshot()));
    owner.close();
    assert!(matches!(
        owner.new_collection(),
        Err(skylive_core::Error::Closed)
    ));
    assert!(matches!(
        owner.adopt_collection(Session::new()),
        Err(skylive_core::Error::Closed)
    ));
    assert!(Arc::ptr_eq(&before, &owner.snapshot()));
}

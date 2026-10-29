use skylive_core::{AudioCaptureBinding, AudioFlow, AudioSlot, AudioTarget, Command, Session};
use skylive_host::{obs, playback::PlaybackSnapshot, preview, storage, transition::Controller};
use skylive_media::MediaTime;

fn microphone() -> AudioCaptureBinding {
    AudioCaptureBinding::new(AudioFlow::Input, AudioTarget::Default, false, false).unwrap()
}

#[test]
fn audio_items_have_no_pixels_or_missing_resource_diagnostics_in_live_and_frozen_program() {
    let mut session = Session::new();
    let scene = session.preview();
    session
        .apply(Command::AddColorSource {
            scene,
            name: "Background".into(),
            rgba: [80, 100, 120, 255],
        })
        .unwrap();
    let before = preview::render(&session.snapshot(), scene, 4, 4).unwrap();
    session
        .apply(Command::AddAudioCaptureSource {
            scene,
            name: "Microphone".into(),
            binding: microphone(),
        })
        .unwrap();
    let after = preview::render(&session.snapshot(), scene, 4, 4).unwrap();
    assert_eq!(before.pixels(), after.pixels());
    let mut controller = Controller::new();
    controller
        .set_studio(&mut session, true, MediaTime(0))
        .unwrap();
    let frozen = controller.sync(&mut session, MediaTime(0)).unwrap().output;
    let rendered = preview::render_output(
        &session.snapshot(),
        &frozen,
        4,
        4,
        &PlaybackSnapshot::at_time(&session, MediaTime(0)),
    )
    .unwrap();
    assert_eq!(before.pixels(), rendered.pixels());
}

#[test]
fn native_storage_preserves_audio_bindings_globals_and_gain_without_device_access() {
    let mut session = Session::new();
    session
        .apply(Command::AddGlobalAudioCaptureSource {
            slot: AudioSlot::Mic1,
            name: "Shared microphone".into(),
            binding: microphone(),
        })
        .unwrap();
    let source = session.snapshot().global_audio()[0].source();
    session
        .apply(Command::SetAudio {
            source,
            gain: 0.25,
            muted: true,
        })
        .unwrap();
    let original = session.snapshot();
    let bytes = storage::encode(&original).unwrap();
    let reopened = storage::decode(&bytes, false).unwrap();
    assert_eq!(*original, *reopened.snapshot());
    assert_eq!(reopened.snapshot().source(source).unwrap().gain(), 0.25);
    assert!(reopened.snapshot().source(source).unwrap().muted());
    let error = obs::export_collection(&reopened, None).unwrap_err();
    assert!(
        error
            .issues
            .iter()
            .any(|issue| issue.code == "unsupported" && issue.path == "$.global_audio")
    );
}

#[test]
fn audio_source_obs_export_fails_explicitly_before_any_bytes_are_published() {
    let mut session = Session::new();
    session
        .apply(Command::AddAudioCaptureSource {
            scene: session.preview(),
            name: "Mic".into(),
            binding: microphone(),
        })
        .unwrap();
    let error = obs::export_collection(&session, None).unwrap_err();
    assert!(
        error
            .issues
            .iter()
            .any(|issue| issue.code == "unsupported" && issue.message.contains("audio capture"))
    );
}

#[test]
fn native_audio_migration_defaults_empty_and_rejects_invalid_authored_data() {
    let empty = Session::new();
    let mut legacy: serde_json::Value =
        serde_json::from_slice(&storage::encode(&empty.snapshot()).unwrap()).unwrap();
    legacy["collection"]
        .as_object_mut()
        .unwrap()
        .remove("global_audio");
    let loaded = storage::decode(&serde_json::to_vec(&legacy).unwrap(), true).unwrap();
    assert!(loaded.snapshot().global_audio().is_empty());
    let mut session = Session::new();
    session
        .apply(Command::AddAudioCaptureSource {
            scene: session.preview(),
            name: "Mic".into(),
            binding: microphone(),
        })
        .unwrap();
    let valid: serde_json::Value =
        serde_json::from_slice(&storage::encode(&session.snapshot()).unwrap()).unwrap();
    let mut unknown = valid.clone();
    unknown["collection"]["sources"][0]["kind"]["binding"]["runtime_handle"] =
        serde_json::json!(99);
    assert!(storage::decode(&serde_json::to_vec(&unknown).unwrap(), true).is_err());
    let mut wrong_flow = valid.clone();
    wrong_flow["collection"]["global_audio"] = serde_json::json!([{"slot":"desktop1","source":2}]);
    assert!(storage::decode(&serde_json::to_vec(&wrong_flow).unwrap(), true).is_err());
    let mut invalid_binding = valid.clone();
    invalid_binding["collection"]["sources"][0]["kind"]["binding"]["target"] =
        serde_json::json!({"kind":"mac_system"});
    assert!(storage::decode(&serde_json::to_vec(&invalid_binding).unwrap(), true).is_err());
    let mut orphan = valid;
    orphan["collection"]["scenes"][0]["items"] = serde_json::json!([]);
    assert!(storage::decode(&serde_json::to_vec(&orphan).unwrap(), true).is_err());
}

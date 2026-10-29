//! Independent user-settings snapshot storage. Session remains the canonical
//! owner; loading never changes it or repairs/overwrites a malformed file.
//! Saving shares native storage's recovery and publication-phase contract.
use crate::storage::{self, Phase};
use serde::{Deserialize, Serialize};
use skylive_core::UserPreferences;
use std::{
    error::Error,
    ffi::OsString,
    fmt,
    fs::{self, File},
    io::{self, Read},
    path::{Path, PathBuf},
};

pub const MAX_USER_PREFERENCES_BYTES: usize = 128 * 1024;
pub const CURRENT_SCHEMA: u32 = 2;
const FILE_NAME: &str = "user-preferences.json";

#[cfg(test)]
mod hotkey_contract_tests {
    use super::*;
    use skylive_core::{HotkeyAction, HotkeyBinding, HotkeyBindings, HotkeyChord};
    #[test]
    fn strict_shortcut_wire_and_legacy_preference_envelope_roundtrip() {
        let mut legacy = serde_json::to_value(Envelope {
            schema: 1,
            preferences: UserPreferences::default(),
        })
        .unwrap();
        assert!(legacy["preferences"].get("hotkeys").is_none());
        assert_eq!(
            decode(&serde_json::to_vec(&legacy).unwrap())
                .unwrap()
                .hotkeys(),
            &HotkeyBindings::default()
        );
        let bindings = HotkeyBindings::new(vec![HotkeyBinding {
            action: HotkeyAction::StartRecording,
            chord: HotkeyChord::new("F9", false, false, false).unwrap(),
        }])
        .unwrap();
        legacy["preferences"]["hotkeys"] = serde_json::to_value(&bindings).unwrap();
        assert_eq!(
            decode(&serde_json::to_vec(&legacy).unwrap())
                .unwrap()
                .hotkeys(),
            &bindings
        );
        legacy["preferences"]["hotkeys"][0]["chord"]["native_handle"] = serde_json::json!(42);
        assert!(decode(&serde_json::to_vec(&legacy).unwrap()).is_err());
        legacy["preferences"]["hotkeys"] = serde_json::json!([]);
        assert!(
            decode(&serde_json::to_vec(&legacy).unwrap())
                .unwrap()
                .hotkeys()
                .bindings()
                .is_empty()
        );
    }
}

#[derive(Debug)]
pub struct UserPreferencesError {
    pub phase: Phase,
    code: &'static str,
    detail: String,
    cause: Option<Box<dyn Error + Send + Sync>>,
}
impl UserPreferencesError {
    pub fn code(&self) -> &'static str {
        self.code
    }
    pub fn detail(&self) -> &str {
        &self.detail
    }
}
impl fmt::Display for UserPreferencesError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({:?}): {}", self.code, self.phase, self.detail)
    }
}
impl Error for UserPreferencesError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.cause.as_deref().map(|cause| cause as &dyn Error)
    }
}
fn rejected(code: &'static str, detail: impl Into<String>) -> UserPreferencesError {
    UserPreferencesError {
        phase: Phase::BeforePublication,
        code,
        detail: detail.into(),
        cause: None,
    }
}
fn before(code: &'static str, cause: impl Error + Send + Sync + 'static) -> UserPreferencesError {
    UserPreferencesError {
        phase: Phase::BeforePublication,
        code,
        detail: cause.to_string(),
        cause: Some(Box::new(cause)),
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    schema: u32,
    preferences: UserPreferences,
}
#[derive(Deserialize)]
struct Version {
    schema: u32,
}

fn decode(bytes: &[u8]) -> Result<UserPreferences, UserPreferencesError> {
    if bytes.len() > MAX_USER_PREFERENCES_BYTES {
        return Err(rejected(
            "user_preferences_limit",
            "user preferences exceed 128 KiB",
        ));
    }
    let version: Version =
        serde_json::from_slice(bytes).map_err(|cause| before("user_preferences_decode", cause))?;
    if !(1..=CURRENT_SCHEMA).contains(&version.schema) {
        return Err(rejected(
            "user_preferences_schema",
            format!("unsupported user-preferences schema {}", version.schema),
        ));
    }
    let envelope: Envelope =
        serde_json::from_slice(bytes).map_err(|cause| before("user_preferences_decode", cause))?;
    if version.schema<2&&!envelope.preferences.preview_snapping().is_default(){return Err(rejected("user_preferences_version","Preview snapping settings require preferences schema2"));}
    if version.schema<2&&!envelope.preferences.projectors().is_default(){return Err(rejected("user_preferences_version","projector defaults require preferences schema2"));}
    if version.schema<2&&!envelope.preferences.process_priority().is_default(){return Err(rejected("user_preferences_version","process priority requires preferences schema2"));}
    if version.schema<2&&!envelope.preferences.ndi_output().is_default(){return Err(rejected("user_preferences_version","NDI output settings require preferences schema2"));}
    if version.schema<2 && !envelope.preferences.accessibility().is_default() {
        return Err(rejected("user_preferences_version","accessibility colors require preferences schema2"));
    }
    if version.schema < 2 && envelope.preferences.appearance().theme().requires_extended_theme_format() {
        return Err(rejected("user_preferences_version", "extended OBS theme variants require preferences schema2"));
    }
    if version.schema < 2 && !envelope.preferences.mixer_display().is_default() {
        return Err(rejected(
            "user_preferences_schema",
            "mixer display preferences require user-preferences schema2",
        ));
    }
    if version.schema < 2 && envelope.preferences.has_setup_metadata() {
        return Err(rejected(
            "user_preferences_schema",
            "setup preferences require user-preferences schema2",
        ));
    }
    if version.schema<2 && !envelope.preferences.audio_monitoring_device().is_default() {
        return Err(rejected("user_preferences_schema","audio monitoring device requires user-preferences schema2"));
    }
    if version.schema<2&&envelope.preferences.hotkeys().bindings().iter().any(|binding|binding.action.requires_extended_format()) {
        return Err(rejected("user_preferences_schema","extended output shortcuts require user-preferences schema2"));
    }
    if version.schema<2&&envelope.preferences.global_hotkeys().requires_extended_format(){return Err(rejected("user_preferences_schema","global output shortcuts require user-preferences schema2"));}
    envelope
        .preferences
        .validate()
        .map_err(|cause| before("user_preferences_invalid", cause))?;
    Ok(envelope.preferences)
}

/// Missing alone means no saved preference snapshot. Malformed, unreadable and
/// future-version files are errors and remain untouched. Recovery is explicit:
/// callers may load storage::recovery_path(path) without deleting either file.
pub fn load(path: &Path) -> Result<Option<UserPreferences>, UserPreferencesError> {
    validate_path(path)?;
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(cause) if cause.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(cause) => return Err(before("user_preferences_io", cause)),
    };
    if file
        .metadata()
        .map_err(|cause| before("user_preferences_io", cause))?
        .len()
        > MAX_USER_PREFERENCES_BYTES as u64
    {
        return Err(rejected(
            "user_preferences_limit",
            "user-preferences file exceeds 128 KiB",
        ));
    }
    // Fixed staging admits at most the cap plus one growth sentinel. read_to_end
    // cannot expand an input-derived Vec capacity on a concurrently grown file.
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(MAX_USER_PREFERENCES_BYTES + 1)
        .map_err(|cause| before("user_preferences_limit", cause))?;
    bytes.resize(MAX_USER_PREFERENCES_BYTES + 1, 0);
    let mut length = 0;
    while length < bytes.len() {
        match file.read(&mut bytes[length..]) {
            Ok(0) => break,
            Ok(count) => length += count,
            Err(cause) if cause.kind() == io::ErrorKind::Interrupted => continue,
            Err(cause) => return Err(before("user_preferences_io", cause)),
        }
    }
    decode(&bytes[..length]).map(Some)
}

/// Explicitly save a validated snapshot. Preparing recovery precedes main-file
/// publication, and an after-publication failure is preserved as such. This is
/// one recoverable file workflow, not a transaction with Session mutation.
pub fn save(path: &Path, preferences: &UserPreferences) -> Result<(), UserPreferencesError> {
    validate_path(path)?;
    preferences
        .validate()
        .map_err(|cause| before("user_preferences_invalid", cause))?;
    let bytes = serde_json::to_vec_pretty(&Envelope {
        schema: CURRENT_SCHEMA,
        preferences: preferences.clone(),
    })
    .map_err(|cause| before("user_preferences_encode", cause))?;
    if bytes.len() > MAX_USER_PREFERENCES_BYTES {
        return Err(rejected(
            "user_preferences_limit",
            "encoded user preferences exceed 128 KiB",
        ));
    }
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).map_err(|cause| before("user_preferences_io", cause))?;
    }
    storage::publish_bytes(path, &bytes).map_err(|cause| UserPreferencesError {
        phase: cause.phase,
        code: "user_preferences_publish",
        detail: cause.to_string(),
        cause: Some(Box::new(cause)),
    })
}
fn validate_path(path: &Path) -> Result<(), UserPreferencesError> {
    if path.file_name().is_none() {
        return Err(rejected(
            "user_preferences_path",
            "user-preferences path must name a file",
        ));
    }
    Ok(())
}

/// Resolve only: no directory creation, file access, environment mutation or
/// default preference publication. Relative explicit overrides become absolute
/// once here so background jobs cannot reinterpret a later working directory.
pub fn default_path() -> Result<PathBuf, UserPreferencesError> {
    #[cfg(target_os = "windows")]
    let platform = Platform::Windows;
    #[cfg(target_os = "macos")]
    let platform = Platform::Macos;
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let platform = Platform::Xdg;
    let path = resolve_path(platform, |name| std::env::var_os(name))?;
    let path = std::path::absolute(path).map_err(|cause| before("user_preferences_path", cause))?;
    validate_path(&path)?;
    Ok(path)
}

// All variants are used by pure path-policy tests on every host.
#[allow(dead_code)]
#[derive(Clone, Copy)]
enum Platform {
    Windows,
    Macos,
    Xdg,
}
fn resolve_path(
    platform: Platform,
    mut environment: impl FnMut(&str) -> Option<OsString>,
) -> Result<PathBuf, UserPreferencesError> {
    if let Some(path) = environment("SKYLIVE_USER_CONFIG") {
        if path.is_empty() {
            return Err(rejected(
                "user_preferences_path",
                "SKYLIVE_USER_CONFIG must not be empty",
            ));
        }
        return Ok(path.into());
    }
    let mut value = |name: &str| {
        environment(name)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    };
    match platform {
        Platform::Windows => value("APPDATA")
            .filter(|root| root.is_absolute())
            .or_else(|| value("LOCALAPPDATA").filter(|root| root.is_absolute()))
            .map(|root| root.join("SkyLive").join(FILE_NAME)),
        Platform::Macos => value("HOME").filter(|root| root.is_absolute()).map(|root| {
            root.join("Library")
                .join("Application Support")
                .join("SkyLive")
                .join(FILE_NAME)
        }),
        Platform::Xdg => value("XDG_CONFIG_HOME")
            .filter(|root| root.is_absolute())
            .or_else(|| {
                value("HOME")
                    .filter(|root| root.is_absolute())
                    .map(|root| root.join(".config"))
            })
            .map(|root| root.join("skylive").join(FILE_NAME)),
    }
    .ok_or_else(|| {
        rejected(
            "user_preferences_path",
            "per-user configuration directory is unavailable",
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use skylive_core::{AUDIO_PAN_PLUGIN, AudioBuffering, Command, Session};
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Directory(PathBuf);
    impl Directory {
        fn new() -> Self {
            static SERIAL: AtomicU64 = AtomicU64::new(1);
            let path = std::env::temp_dir().join(format!(
                "skylive-user-preferences-{}-{}",
                std::process::id(),
                SERIAL.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn file(&self) -> PathBuf {
            self.0.join(FILE_NAME)
        }
    }
    impl Drop for Directory {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }
    fn custom() -> UserPreferences {
        let mut session = Session::new();
        session
            .apply(Command::SetLanguage {
                language: "uk-UA".into(),
            })
            .unwrap();
        session
            .apply(Command::SetPluginEnabled {
                plugin: AUDIO_PAN_PLUGIN.into(),
                enabled: true,
            })
            .unwrap();
        let settings = session
            .snapshot()
            .audio()
            .with_buffering(AudioBuffering::low_latency())
            .unwrap();
        session
            .apply(Command::SetAudioSettings { settings })
            .unwrap();
        session.snapshot().preferences().clone()
    }

    #[test]
    fn missing_roundtrip_nested_directory_and_explicit_replacement_preserve_all_user_scopes() {
        let directory = Directory::new();
        let path = directory.0.join("nested").join(FILE_NAME);
        assert_eq!(load(&path).unwrap(), None);
        assert!(
            !directory.0.join("nested").exists(),
            "load never creates state"
        );
        let preferences = custom();
        save(&path, &preferences).unwrap();
        assert_eq!(load(&path).unwrap(), Some(preferences));
        let wire: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(wire["schema"], CURRENT_SCHEMA);
        assert_eq!(wire["preferences"]["language"], "uk-UA");
        assert!(wire.get("collection").is_none());
        assert!(wire.get("profiles").is_none());
        assert!(!storage::recovery_path(&path).exists());
        save(&path, &UserPreferences::default()).unwrap();
        assert_eq!(load(&path).unwrap(), Some(UserPreferences::default()));
    }

    #[test]
    fn malformed_future_unknown_and_invalid_candidates_remain_exact_until_explicit_save() {
        let directory = Directory::new();
        let path = directory.file();
        let future = format!(r#"{{"schema":{},"future":true}}"#, CURRENT_SCHEMA + 1);
        for (bytes, code) in [
            (b"{broken".as_slice(), "user_preferences_decode"),
            (future.as_bytes(), "user_preferences_schema"),
            (b"{\"schema\":1,\"preferences\":{\"audio_buffering\":{\"max_ms\":1001,\"fixed\":false}}}".as_slice(), "user_preferences_decode"),
            (b"{\"schema\":1,\"preferences\":{\"audio_buffering\":{\"max_ms\":0,\"fixed\":false},\"language\":\"../bad\"}}".as_slice(), "user_preferences_decode"),
            (b"{\"schema\":1,\"preferences\":{\"audio_buffering\":{\"max_ms\":0,\"fixed\":false},\"enabled_plugins\":[\"unported/provider\"]}}".as_slice(), "user_preferences_decode"),
            (b"{\"schema\":1,\"preferences\":{\"audio_buffering\":{\"max_ms\":0,\"fixed\":false}},\"unknown\":1}".as_slice(), "user_preferences_decode"),
        ] {
            fs::write(&path, bytes).unwrap();
            let error = load(&path).unwrap_err();
            assert_eq!(error.code(), code);
            assert_eq!(error.phase, Phase::BeforePublication);
            assert_eq!(fs::read(&path).unwrap(), bytes);
            assert!(!storage::recovery_path(&path).exists());
        }
        save(&path, &custom()).unwrap();
        assert_eq!(load(&path).unwrap(), Some(custom()));
    }

    #[test]
    fn byte_limit_applies_to_file_and_owned_decode_before_publication() {
        let directory = Directory::new();
        let path = directory.file();
        let valid = br#"{"schema":1,"preferences":{"audio_buffering":{"max_ms":0,"fixed":false}}}"#;
        let mut exact = valid.to_vec();
        exact.resize(MAX_USER_PREFERENCES_BYTES, b' ');
        fs::write(&path, &exact).unwrap();
        assert_eq!(load(&path).unwrap(), Some(UserPreferences::default()));
        exact.push(b' ');
        fs::write(&path, &exact).unwrap();
        assert_eq!(load(&path).unwrap_err().code(), "user_preferences_limit");
        assert_eq!(decode(&exact).unwrap_err().code(), "user_preferences_limit");
        assert_eq!(fs::read(&path).unwrap(), exact);
    }

    #[test]
    fn failed_recovery_preparation_preserves_existing_preferences_and_reports_phase_and_cause() {
        let directory = Directory::new();
        let path = directory.file();
        save(&path, &UserPreferences::default()).unwrap();
        let original = fs::read(&path).unwrap();
        let recovery = storage::recovery_path(&path);
        fs::create_dir(&recovery).unwrap();
        fs::write(recovery.join("sentinel"), b"preserve").unwrap();
        let error = save(&path, &custom()).unwrap_err();
        assert_eq!(error.code(), "user_preferences_publish");
        assert_eq!(error.phase, Phase::BeforePublication);
        assert!(error.source().is_some());
        assert_eq!(fs::read(&path).unwrap(), original);
        assert_eq!(fs::read(recovery.join("sentinel")).unwrap(), b"preserve");
    }

    #[test]
    fn failed_main_publication_keeps_recoverable_new_snapshot_and_existing_target() {
        let directory = Directory::new();
        let path = directory.file();
        fs::create_dir(&path).unwrap();
        fs::write(path.join("sentinel"), b"preserve").unwrap();
        let preferences = custom();
        let error = save(&path, &preferences).unwrap_err();
        assert_eq!(error.phase, Phase::BeforePublication);
        assert_eq!(error.code(), "user_preferences_publish");
        assert_eq!(fs::read(path.join("sentinel")).unwrap(), b"preserve");
        assert_eq!(
            load(&storage::recovery_path(&path)).unwrap(),
            Some(preferences)
        );
        assert!(
            load(&path).is_err(),
            "a directory is not a missing preference file"
        );
    }

    #[test]
    fn platform_paths_and_exact_override_are_resolved_without_mutating_process_environment() {
        let root = std::env::temp_dir().join("preferences-path-policy");
        for (platform, variable, suffix) in [
            (
                Platform::Windows,
                "APPDATA",
                Path::new("SkyLive").join(FILE_NAME),
            ),
            (
                Platform::Macos,
                "HOME",
                Path::new("Library")
                    .join("Application Support")
                    .join("SkyLive")
                    .join(FILE_NAME),
            ),
            (
                Platform::Xdg,
                "XDG_CONFIG_HOME",
                Path::new("skylive").join(FILE_NAME),
            ),
        ] {
            let path = resolve_path(platform, |name| {
                (name == variable).then(|| root.as_os_str().to_owned())
            })
            .unwrap();
            assert_eq!(path, root.join(suffix));
            let override_path = PathBuf::from("relative user settings.json");
            let path = resolve_path(platform, |name| match name {
                "SKYLIVE_USER_CONFIG" => Some(override_path.as_os_str().to_owned()),
                _ => Some(root.as_os_str().to_owned()),
            })
            .unwrap();
            assert_eq!(path, override_path);
            assert_eq!(
                resolve_path(platform, |_| None).unwrap_err().code(),
                "user_preferences_path"
            );
            assert_eq!(
                resolve_path(platform, |name| (name == "SKYLIVE_USER_CONFIG")
                    .then(OsString::new))
                .unwrap_err()
                .code(),
                "user_preferences_path"
            );
        }
        let fallback = resolve_path(Platform::Xdg, |name| match name {
            "HOME" => Some(root.as_os_str().to_owned()),
            "XDG_CONFIG_HOME" => Some("relative-invalid-xdg".into()),
            _ => None,
        })
        .unwrap();
        assert_eq!(
            fallback,
            root.join(".config").join("skylive").join(FILE_NAME)
        );
    }
    #[test]
    fn appearance_wire_legacy_defaults_strict_validation_and_installed_roundtrip() {
        use skylive_core::{Appearance, InterfaceTheme};
        let mut legacy = serde_json::to_value(Envelope {
            schema: 1,
            preferences: UserPreferences::default(),
        })
        .unwrap();
        assert!(legacy["preferences"].get("appearance").is_none());
        assert_eq!(
            decode(&serde_json::to_vec(&legacy).unwrap())
                .unwrap()
                .appearance(),
            Appearance::default()
        );
        for invalid in [
            serde_json::json!({"theme":"light","scale_percent":74}),
            serde_json::json!({"theme":"dark","scale_percent":201}),
            serde_json::json!({"theme":"foreign","scale_percent":100}),
            serde_json::json!({"theme":"yami","scale_percent":100,"os_handle":7}),
            serde_json::json!({"theme":"system","scale_percent":100.5}),
        ] {
            legacy["preferences"]["appearance"] = invalid;
            assert!(decode(&serde_json::to_vec(&legacy).unwrap()).is_err());
        }
        let directory = Directory::new();
        let path = directory.file();
        let mut session = Session::new();
        session
            .apply(Command::SetAppearance {
                appearance: Appearance::new(InterfaceTheme::System, 125).unwrap(),
            })
            .unwrap();
        let preferences = session.snapshot().preferences().clone();
        save(&path, &preferences).unwrap();
        assert_eq!(load(&path).unwrap(), Some(preferences));
        let bytes = fs::read(&path).unwrap();
        let mut corrupt: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        corrupt["preferences"]["appearance"]["scale_percent"] = serde_json::json!(0);
        let corrupt = serde_json::to_vec(&corrupt).unwrap();
        fs::write(&path, &corrupt).unwrap();
        assert!(load(&path).is_err());
        assert_eq!(
            fs::read(&path).unwrap(),
            corrupt,
            "load never repairs or overwrites invalid preferences"
        );
    }
}

#[cfg(test)]
#[path = "user_preferences_setup_tests.rs"]
mod setup_tests;

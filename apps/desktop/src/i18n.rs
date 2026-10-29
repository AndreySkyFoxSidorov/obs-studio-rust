//! Immutable selected-language presentation; file preparation belongs to host/I/O.
use eframe::egui;
use skylive_host::localization::Localization;
use std::{
    path::PathBuf,
    sync::{Arc, OnceLock},
};

const CATALOG_ID: &str = "skylive-localization";
const SOURCE_ID: &str = "skylive-localization-source";
static ENGLISH: OnceLock<Arc<Localization>> = OnceLock::new();

pub fn english() -> Result<Arc<Localization>, Box<dyn std::error::Error + Send + Sync>> {
    if let Some(value) = ENGLISH.get() {
        return Ok(value.clone());
    }
    let value = Arc::new(Localization::embedded("en-US")?);
    let _ = ENGLISH.set(value.clone());
    Ok(value)
}
pub fn workspace_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../localization/translations.csv")
}
pub fn external_path() -> PathBuf {
    if let Some(path) = std::env::var_os("SKYLIVE_LOCALIZATION_FILE") {
        return path.into();
    }
    let packaged = std::env::current_exe().ok().and_then(|path| {
        path.parent()
            .map(|parent| parent.join("localization/translations.csv"))
    });
    if let Some(path) = packaged.as_ref().filter(|path| path.is_file()) {
        return path.clone();
    }
    let workspace = workspace_path();
    if workspace.is_file() {
        return workspace;
    }
    packaged.unwrap_or(workspace)
}
pub fn install(context: &egui::Context, value: Arc<Localization>, source: &std::path::Path) {
    context.data_mut(|data| {
        data.insert_temp(egui::Id::new(CATALOG_ID), value);
        data.insert_temp(egui::Id::new(SOURCE_ID), source.display().to_string());
    });
    context.request_repaint();
}
pub fn catalog(context: &egui::Context) -> Option<Arc<Localization>> {
    context.data(|data| data.get_temp(egui::Id::new(CATALOG_ID)))
}
pub fn source(context: &egui::Context) -> String {
    context
        .data(|data| data.get_temp::<String>(egui::Id::new(SOURCE_ID)))
        .unwrap_or_default()
}
pub fn tr(ui: &egui::Ui, english: impl AsRef<str>) -> String {
    tr_context(ui.ctx(), english)
}
pub fn tr_key(ui: &egui::Ui, key: &str) -> String {
    key_text(ui.ctx(), key, key)
}
pub fn tr_key_or(ui: &egui::Ui, key: &str, fallback: &str) -> String {
    key_text(ui.ctx(), key, fallback)
}
fn key_text(context: &egui::Context, key: &str, fallback: &str) -> String {
    if let Some(value) = catalog(context).filter(|value| value.translate(key) != key) {
        return value.label(key).into_owned();
    }
    // Already prepared at startup: an old/edited external table may omit a
    // newly introduced semantic key. No parsing or filesystem access in a frame.
    if let Some(value) = ENGLISH.get().filter(|value| value.translate(key) != key) {
        return value.label(key).into_owned();
    }
    fallback.to_owned()
}
pub fn tf_catalog(catalog: &Localization, english: &str, args: &[(&str, String)]) -> String {
    let translated = catalog.text(english);
    render(&translated, args)
        .or_else(|| render(english, args))
        .unwrap_or_else(|| english.to_owned())
}
pub fn tf_context(context: &egui::Context, english: &str, args: &[(&str, String)]) -> String {
    let translated = tr_context(context, english);
    render(&translated, args)
        .or_else(|| render(english, args))
        .unwrap_or_else(|| english.to_owned())
}
pub fn tr_context(context: &egui::Context, english: impl AsRef<str>) -> String {
    let english = english.as_ref();
    catalog(context)
        .map(|value| value.text(english).into_owned())
        .unwrap_or_else(|| english.to_owned())
}

pub fn tf(ui: &egui::Ui, english: &str, args: &[(&str, String)]) -> String {
    let translated = tr(ui, english);
    render(&translated, args)
        .or_else(|| render(english, args))
        .unwrap_or_else(|| english.to_owned())
}
fn render(template: &str, args: &[(&str, String)]) -> Option<String> {
    if args.len() > 64 || template.len() > 32768 {
        return None;
    }
    let mut used = vec![false; args.len()];
    let mut output = String::new();
    let mut cursor = 0;
    while cursor < template.len() {
        let tail = &template[cursor..];
        if tail.starts_with("{{") {
            output.push('{');
            cursor += 2;
        } else if tail.starts_with("}}") {
            output.push('}');
            cursor += 2;
        } else if tail.starts_with('{') {
            let end = tail.find('}')?;
            let token = &tail[..=end];
            let (index, (_, value)) = args
                .iter()
                .enumerate()
                .find(|(index, (key, _))| !used[*index] && *key == token)?;
            output.push_str(value);
            used[index] = true;
            cursor += end + 1;
        } else if tail.starts_with('}') {
            return None;
        } else {
            let ch = tail.chars().next()?;
            output.push(ch);
            cursor += ch.len_utf8();
        }
        if output.len() > 32768 {
            return None;
        }
    }
    used.iter().all(|used| *used).then_some(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn missing_semantic_rows_use_cached_embedded_english_without_translating_authored_fallbacks() {
        let _english = english().unwrap();
        let context = egui::Context::default();
        let value = Arc::new(
            Localization::from_csv(
                "key,en-US,ru-RU\nSettings,Settings,Настройки\n".as_bytes(),
                "ru-RU",
            )
            .unwrap(),
        );
        install(&context, value, std::path::Path::new("owned.csv"));
        assert_eq!(
            key_text(
                &context,
                "Module.obs-transitions.CutTransition",
                "Русский исходник"
            ),
            "Cut"
        );
        assert_eq!(
            key_text(&context, "unknown.key", "Authored Settings"),
            "Authored Settings"
        );
        assert_eq!(key_text(&context, "Settings", "Settings"), "Настройки");
    }

    #[test]
    fn formatted_localization_preserves_fields_and_never_reinterprets_user_values() {
        assert_eq!(
            render(
                "{name}: {}",
                &[("{}", "300".into()), ("{name}", "Save {unsafe}".into())]
            )
            .unwrap(),
            "Save {unsafe}: 300"
        );
        assert_eq!(
            render("{} / {}", &[("{}", "1".into()), ("{}", "2".into())]).unwrap(),
            "1 / 2"
        );
        assert!(render("{missing}", &[("{}", "value".into())]).is_none());
        assert!(
            render(
                "translated without required data",
                &[("{}", "value".into())]
            )
            .is_none()
        );
    }
    #[test]
    fn context_localization_changes_visible_chrome_without_changing_user_text_or_commands() {
        let context = egui::Context::default();
        for (language, expected) in [
            ("en-US", "Settings"),
            ("ru-RU", "Настройки"),
            ("uk-UA", "Налаштування"),
            ("de-DE", "Einstellungen"),
            ("es-ES", "Ajustes"),
        ] {
            let catalog = Arc::new(Localization::embedded(language).unwrap());
            install(&context, catalog, &workspace_path());
            assert_eq!(tr_context(&context, "Settings"), expected, "{language}");
        }
        assert_eq!(
            tr_context(&context, "User authored scene 123"),
            "User authored scene 123"
        );
    }
}

//! View-only file choice and compact document identity; no filesystem probing.
use super::{
    Intent,
    modal_footer::{self, Action},
};
use eframe::egui;
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DocumentAction {
    Open,
    Recover,
    ImportObs,
}
impl DocumentAction {
    pub fn title(self) -> &'static str {
        match self {
            Self::Open => "Open collection",
            Self::Recover => "Recover collection",
            Self::ImportObs => "Import OBS Scene Collection",
        }
    }
    pub fn action(self) -> &'static str {
        match self {
            Self::Open => "Open",
            Self::Recover => "Recover",
            Self::ImportObs => "Import",
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PathDraft {
    pub action: DocumentAction,
    pub path: String,
    pub session: u64,
    pub revision: u64,
}
pub fn body(ui: &mut egui::Ui, draft: &mut PathDraft, busy: bool) -> Option<Intent> {
    let mut browse=None;
    ui.heading(crate::i18n::tr(ui, draft.action.title()));
    let actions = [
        Action::new(
            draft.action.action(),
            !busy && !draft.path.trim().is_empty(),
            "Read and validate this file before replacing the current collection.",
        ),
        Action::new(
            "Cancel",
            !busy,
            "Close the file chooser and keep the current collection.",
        ),
    ];
    let height =
        (ui.ctx().screen_rect().height() - modal_footer::required_height(ui, &actions) - 110.0)
            .max(24.0);
    egui::ScrollArea::vertical().id_salt("document-picker-body").max_height(height).show(ui,|ui|{
        ui.style_mut().wrap_mode=Some(egui::TextWrapMode::Wrap);
        let label=ui.label(crate::i18n::tr(ui,"File path"));
        ui.add_enabled(!busy,egui::TextEdit::singleline(&mut draft.path).char_limit(4096)
            .id_salt("document-picker-path").desired_width(ui.available_width()))
            .labelled_by(label.id).on_hover_text(crate::i18n::tr(ui,"Enter the exact file to open, recover or import."));
        browse=crate::native_browse::button(ui,"Browse…",crate::native_browse::Target::DocumentOpen,!busy);
        ui.label(crate::i18n::tr(ui,"Opening a file does not change the destination used by Save."));
        if draft.action==DocumentAction::ImportObs {
            ui.label(crate::i18n::tr(ui,"Only supported OBS collection features can be imported; unsupported features are reported."));
        }
    });
    ui.separator();
    match modal_footer::show(ui, &actions) {
        Some(0) => Some(Intent::ConfirmDocumentPath(draft.clone())),
        Some(1) => Some(Intent::CancelDocumentPath),
        _ => browse,
    }
}

/// Display only. The caller retains its exact original PathBuf for every operation.
pub fn display_path(path: &Path) -> String {
    let raw = path.to_string_lossy();
    if let Some(unc) = raw.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{unc}");
    }
    if let Some(drive) = raw.strip_prefix(r"\\?\") {
        let b = drive.as_bytes();
        if b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'\\' {
            return drive.to_owned();
        }
    }
    raw.into_owned()
}
pub fn filename(path: Option<&Path>, untitled: &str) -> String {
    path.and_then(Path::file_name).map_or_else(
        || untitled.to_owned(),
        |name| name.to_string_lossy().into_owned(),
    )
}
pub fn window_title(path: Option<&Path>, dirty: bool, untitled: &str) -> String {
    format!(
        "{}{} — SkyLive",
        filename(path, untitled),
        if dirty { " *" } else { "" }
    )
}
pub fn sync_title(ctx: &egui::Context, path: Option<&Path>, dirty: bool) {
    let title = window_title(path, dirty, &crate::i18n::tr_context(ctx, "Untitled"));
    let id = egui::Id::new("document-window-title");
    let changed = ctx.data_mut(|data| {
        if data.get_temp::<String>(id).as_ref() == Some(&title) {
            false
        } else {
            data.insert_temp(id, title.clone());
            true
        }
    });
    if changed {
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(title));
    }
}
/// Managed filenames are implementation identity; the authored collection name
/// is the user's visible title. Legacy filename APIs remain available.
pub fn sync_collection_title(ctx:&egui::Context,name:&str,dirty:bool){
    if name.is_empty(){sync_title(ctx,None,dirty);return;}
    let title=format!("{}{} — SkyLive",name,if dirty{" *"}else{""});let id=egui::Id::new("document-window-title");
    let changed=ctx.data_mut(|data|{if data.get_temp::<String>(id).as_ref()==Some(&title){false}else{data.insert_temp(id,title.clone());true}});
    if changed{ctx.send_viewport_cmd(egui::ViewportCommand::Title(title));}
}
pub fn current_collection(ui:&mut egui::Ui,name:&str,path:Option<&Path>,dirty:bool){
    if name.is_empty(){current(ui,path,dirty);return;}
    let caption=format!("{}{}",name,if dirty{" *"}else{""});let location=path.map_or_else(||crate::i18n::tr(ui,"Not yet saved"),display_path);
    ui.add_sized(egui::vec2(ui.available_width().min(180.0),ui.spacing().interact_size.y),egui::Label::new(caption).truncate()).on_hover_text(format!("{}\n{}",name,location));
}
pub fn current(ui: &mut egui::Ui, path: Option<&Path>, dirty: bool) {
    let caption = format!(
        "{}{}",
        filename(path, &crate::i18n::tr(ui, "Untitled")),
        if dirty { " *" } else { "" }
    );
    let hint = path.map_or_else(|| crate::i18n::tr(ui, "Not yet saved"), display_path);
    ui.add_sized(
        egui::vec2(
            ui.available_width().min(180.0),
            ui.spacing().interact_size.y,
        ),
        egui::Label::new(caption).truncate(),
    )
    .on_hover_text(hint);
}

#[cfg(test)]
#[path = "document_picker_tests.rs"]
mod tests;

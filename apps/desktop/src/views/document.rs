//! View-only document replacement/save confirmation; no filesystem probing.
use super::{
    Intent,
    modal_footer::{self, Action},
};
use eframe::egui;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    Save,
    Discard,
    Cancel,
}
pub enum Prompt {
    Choose(super::document_picker::PathDraft),
    Replace { target: String },
    SaveAs { path: String },
}

/// Camera discovery is asynchronous: cancelling abandons this import, while a
/// native query already in progress is allowed to retire on its owner.
pub fn show_camera_import_progress(context: &egui::Context, completed: usize, total: usize) -> bool {
    let mut cancel = false;
    let response = egui::Modal::new(egui::Id::new("obs-camera-import-progress")).show(context, |ui| {
        ui.set_width(480.0_f32.min((context.screen_rect().width() - 48.0).max(120.0)));
        ui.heading(crate::i18n::tr(ui, "Finding OBS cameras"));
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(crate::i18n::tf(ui, "Cameras checked: {completed} of {total}", &[
                ("{completed}", completed.min(total).to_string()), ("{total}", total.to_string()),
            ]));
        });
        if total > 0 {
            ui.add(egui::ProgressBar::new(completed.min(total) as f32 / total as f32).show_percentage());
        }
        ui.label(crate::i18n::tr(ui, "Cancelling keeps the current document. A camera query already in progress may finish in the background."));
        ui.separator();
        let actions = [Action::new("Cancel", true, "Cancel camera discovery and keep the current document.")];
        cancel = modal_footer::show(ui, &actions) == Some(0);
    });
    cancel || response.should_close()
}

pub fn show(context: &egui::Context, prompt: &mut Prompt, busy: bool) -> Option<Intent> {
    let mut intent = None;
    let response=egui::Modal::new(egui::Id::new("document-replacement")).show(context,|ui|{
        ui.set_width(720.0_f32.min((context.screen_rect().width()-48.0).max(120.0)));
        match prompt {
            Prompt::Choose(draft)=>{ intent=super::document_picker::body(ui,draft,busy); }
            Prompt::Replace{target}=>{
                ui.heading(crate::i18n::tr(ui,"Unsaved document changes"));
                let actions=[Action::new("Save and continue",!busy,"Save the current native document before replacing it."),Action::new("Discard and continue",!busy,"Replace the current document without saving its changes."),Action::new("Cancel",!busy,"Keep the current document and cancel replacement.")];
                let height=(context.screen_rect().height()-modal_footer::required_height(ui,&actions)-110.0).max(40.0);
                egui::ScrollArea::vertical().id_salt("replace-document-body").max_height(height).show(ui,|ui|{
                    ui.label(crate::i18n::tr(ui,"The candidate document is validated. Your current Program remains active until you choose an action."));
                    ui.label(crate::i18n::tf(ui,"Replacement: {path}",&[("{path}",target.clone())]));
                });ui.separator();if let Some(index)=modal_footer::show(ui,&actions){intent=Some(Intent::ResolveDocument([Choice::Save,Choice::Discard,Choice::Cancel][index]));}
            }
            Prompt::SaveAs{path}=>{
                ui.heading(crate::i18n::tr(ui,"Save native document as"));
                let actions=[Action::new("Save to this path",!busy&&!path.trim().is_empty(),"Confirm this exact native destination, including replacement of an existing file."),Action::new("Cancel",!busy,"Cancel this save and keep the current document.")];
                let height=(context.screen_rect().height()-modal_footer::required_height(ui,&actions)-110.0).max(40.0);
                egui::ScrollArea::vertical().id_salt("save-document-body").max_height(height).show(ui,|ui|{
                    ui.label(crate::i18n::tr(ui,"Native save destination"));ui.add(egui::TextEdit::singleline(path).char_limit(4096).id_salt("native-save-path").desired_width(ui.available_width())).on_hover_text(crate::i18n::tr(ui,"Choose a native SkyLive filename. This path is separate from the Open target."));
                    if let Some(browse)=crate::native_browse::button(ui,"Browse…",crate::native_browse::Target::DocumentSave,!busy) {intent=Some(browse);}
                    ui.label(crate::i18n::tr(ui,"Confirming Save to this path may replace an existing file at this exact destination. The Open target and original OBS reference are never selected automatically."));
                });ui.separator();match modal_footer::show(ui,&actions){Some(0)=>intent=Some(Intent::SaveAsTo{path:path.clone()}),Some(1)=>intent=Some(Intent::CancelDocumentSave),_=>{}}
            }
        }
    });
    if response.should_close() && !busy {
        intent = Some(match prompt {
            Prompt::Choose(..) => Intent::CancelDocumentPath,
            Prompt::Replace { .. } => Intent::ResolveDocument(Choice::Cancel),
            Prompt::SaveAs { .. } => Intent::CancelDocumentSave,
        });
    }
    intent
}

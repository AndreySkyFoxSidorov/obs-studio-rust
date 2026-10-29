//! Settings presentation contracts; canonical publication belongs to the app owner.
use super::{dialogs::Dialog, modal_footer::Action};
use skylive_core::{Command, Session, StudioSnapshot};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SettingsToken {
    pub session: u64,
    pub revision: u64,
    pub generation: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsPage {
    General,
    Appearance,
    Accessibility,
    Advanced,
    Stream,
    Output,
    Audio,
    Video,
    Hotkeys,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsAction {
    Ok,
    Apply,
    ReloadLanguage,
}

pub fn page(dialog: &Dialog) -> Option<SettingsPage> {
    Some(match dialog {
        Dialog::General { .. } => SettingsPage::General,
        Dialog::Appearance(_) => SettingsPage::Appearance,
        Dialog::Accessibility(_) => SettingsPage::Accessibility,
        Dialog::AdvancedProcess(_) => SettingsPage::Advanced,
        Dialog::Streaming(_) => SettingsPage::Stream,
        Dialog::Recording(_) => SettingsPage::Output,
        Dialog::AudioSettings { .. } => SettingsPage::Audio,
        Dialog::Video { .. } => SettingsPage::Video,
        Dialog::Hotkeys(_) => SettingsPage::Hotkeys,
        _ => return None,
    })
}
pub fn fresh(page: SettingsPage, model: &StudioSnapshot) -> Dialog {
    match page {
        SettingsPage::General => Dialog::general(model),
        SettingsPage::Advanced=>Dialog::AdvancedProcess(super::advanced_process::Draft::new(model)),
        SettingsPage::Accessibility => Dialog::Accessibility(super::accessibility::Draft::new(model.preferences().accessibility())),
        SettingsPage::Appearance => Dialog::Appearance(super::appearance::AppearanceDraft::new(
            model.preferences().appearance(),
        )),
        SettingsPage::Stream => Dialog::streaming(model),
        SettingsPage::Output => Dialog::recording(model),
        SettingsPage::Audio => Dialog::audio_settings(&model.audio()),
        SettingsPage::Video => Dialog::video_profile(model),
        SettingsPage::Hotkeys => Dialog::Hotkeys(super::hotkeys::HotkeyDraft::from_preferences(model.preferences())),
    }
}
pub fn unchanged(dialog: &Dialog, model: &StudioSnapshot) -> bool {
    match dialog {
        Dialog::General { language,projectors,snapping } => language == model.preferences().language()&&projectors.unchanged(model.preferences().projectors())&&snapping.unchanged(model.preferences().preview_snapping()),
        Dialog::AdvancedProcess(draft)=>draft.unchanged(model),
        Dialog::Appearance(draft) => draft.unchanged(model.preferences().appearance()),
        Dialog::Accessibility(draft) => draft.unchanged(model.preferences().accessibility()),
        Dialog::Streaming(draft) => draft.unchanged(model),
        Dialog::Recording(draft) => draft.unchanged(model),
        Dialog::Hotkeys(draft) => draft.unchanged_preferences(model.preferences()),
        Dialog::Video {
            width,
            height,
            fps_num,
            fps_den,
            output,
        } => {
            output.unchanged(model.output_video_settings()) && (*width, *height, *fps_num, *fps_den)
                == (
                    model.video().width(),
                    model.video().height(),
                    model.video().fps_num(),
                    model.video().fps_den(),
                )
        }
        Dialog::AudioSettings {
            sample_rate,
            speakers,
            buffering,
        } => {
            (*sample_rate, *speakers, *buffering)
                == (
                    model.audio().sample_rate(),
                    model.audio().speakers(),
                    model.audio().buffering(),
                )
        }
        _ => false,
    }
}
pub fn actions(dialog: &Dialog, model: &StudioSnapshot) -> Option<Vec<Action<'static>>> {
    page(dialog)?;
    let listening = matches!(dialog,Dialog::Hotkeys(draft)if draft.is_listening());
    let priority=matches!(dialog,Dialog::AdvancedProcess(_));
    let busy=matches!(dialog,Dialog::AdvancedProcess(draft)if draft.busy);
    let supported=!priority||skylive_host::process_priority::available();
    let changed = !unchanged(dialog, model)||priority&&supported;
    Some(vec![
        Action::new(
            "OK",
            !listening&&!busy&&(supported||unchanged(dialog,model)),
            if busy {"Wait for the process priority result."}else if priority {"Apply Advanced settings and close after Windows confirms the process priority."}else if listening {
                "Finish or cancel the shortcut capture before applying."
            } else {
                "Save these settings and close Settings."
            },
        ),
        Action::new(
            "Cancel",
            true,
            if priority&&busy{"Discard the draft and close Settings. A completed native change is not rolled back."}else{"Discard unapplied changes and close Settings."},
        ),
        Action::new(
            "Apply",
            !listening&&!busy&&changed&&supported,
            if !supported {"Process priority control is available only on Windows."}else if busy {"Wait for the process priority result."}else if priority {"Apply the captured process priority and selected-profile reconnect policy."}else if listening {
                "Finish or cancel the shortcut capture before applying."
            } else if !changed {
                "There are no unapplied settings on this page."
            } else {
                "Save these settings and keep this Settings page open."
            },
        ),
    ])
}
pub(crate) fn prepare(dialog: &Dialog, session: &Session) -> Result<Command, String> {
    let model = session.snapshot();
    match dialog {
        Dialog::General{language,projectors,snapping}=>{projectors.command(model.preferences().projectors())?;Ok(Command::ConfigureGeneralSettings{language:language.clone(),projectors:projectors.prepared(),preview_snapping:Some(snapping.prepare_for(model.preferences().preview_snapping())?)})},
        Dialog::AdvancedProcess(draft)=>draft.command(),
        Dialog::Appearance(draft) => draft.command(model.preferences().appearance()),
        Dialog::Accessibility(draft) => draft.command(),
        Dialog::Streaming(draft) => draft.request()?.command_for(session),
        Dialog::Hotkeys(draft) => draft.command_preferences(model.preferences()),
        Dialog::Recording(_) | Dialog::Video { .. } | Dialog::AudioSettings { .. } => dialog
            .command()?
            .ok_or_else(|| "Settings page has no command.".to_owned()),
        _ => Err("Settings page has no command.".into()),
    }
}
pub fn preflight(dialog: &Dialog, model: &StudioSnapshot) -> Result<(), String> {
    match dialog {
        Dialog::General {projectors,snapping,..} => {projectors.command(model.preferences().projectors())?;snapping.prepare_for(model.preferences().preview_snapping()).map(|_|())},
        Dialog::AdvancedProcess(draft)=>draft.command().map(|_|()),
        Dialog::Appearance(draft) => draft.command(model.preferences().appearance()).map(|_| ()),
        Dialog::Accessibility(draft) => draft.command().map(|_|()),
        Dialog::Streaming(draft) => draft.request().map(|_| ()),
        Dialog::Hotkeys(draft) => draft.command_preferences(model.preferences()).map(|_| ()),
        Dialog::Recording(_) | Dialog::Video { .. } | Dialog::AudioSettings { .. } => {
            dialog.command().map(|_| ())
        }
        _ => Err("Settings page has no command.".into()),
    }
}

#[cfg(test)]
#[path = "settings_workflow_tests.rs"]
mod tests;

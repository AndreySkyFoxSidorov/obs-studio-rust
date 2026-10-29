//! One Settings-page transaction and optional owned localization request.
use super::*;
use crate::views::settings::{self, SettingsAction, SettingsPage, SettingsToken};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Binding {
    session: u64,
    revision: u64,
    page: SettingsPage,
    generation: u64,
}
struct Pending {
    request: u64,
    binding: Binding,
    action: SettingsAction,
    language: String,
    projectors:Option<skylive_core::ProjectorPreferences>,
    projector_draft:crate::views::projector_settings::Draft,
    snapping_draft:crate::views::preview_snapping::Draft,
}
#[derive(Default)]
pub(super) struct Flow {
    current: Option<Binding>,
    pending: Option<Pending>,
    sequence: u64,
}
impl Flow {
    pub(super) fn opened(&mut self, session: &Session, dialog: &Dialog) -> Result<(), String> {
        let Some(page) = settings::page(dialog) else {
            self.current = None;
            return Ok(());
        };
        let generation = self
            .sequence
            .checked_add(1)
            .ok_or("Settings draft identity exhausted. Restart SkyLive.")?;
        self.current = Some(Binding {
            session: session.identity(),
            revision: session.snapshot().revision(),
            page,
            generation,
        });
        self.sequence = generation;
        Ok(())
    }
    pub(super) fn token(&self) -> Option<SettingsToken> {
        self.current.map(|b| SettingsToken {
            session: b.session,
            revision: b.revision,
            generation: b.generation,
        })
    }
    pub(super) fn pending_request(&self) -> Option<u64> {
        self.pending.as_ref().map(|p| p.request)
    }
}
impl StudioApp {
    pub(super) fn observe_settings_draft(&mut self) {
        let page = self.dialog.as_ref().and_then(settings::page);
        if self.settings_flow.pending.is_some() {
            return;
        }
        match (self.settings_flow.current, page) {
            (_, None) => self.settings_flow.current = None,
            (None, Some(_)) => {
                if let Some(dialog) = &self.dialog {
                    if let Err(error) = self.settings_flow.opened(&self.session, dialog) {
                        self.message(true, error);
                    }
                }
            }
            (Some(binding), Some(page)) if binding.page != page => {
                if let Some(dialog) = &self.dialog {
                    if let Err(error) = self.settings_flow.opened(&self.session, dialog) {
                        self.message(true, error);
                    }
                }
            }
            _ => {} // Same-page external changes must remain stale, not silently rebase.
        }
    }
    fn settings_binding(&self, page: SettingsPage) -> Result<Binding, String> {
        let binding = self
            .settings_flow
            .current
            .ok_or("Settings have no active draft. Reopen Settings.")?;
        if binding.page != page
            || self.dialog.as_ref().and_then(settings::page) != Some(page)
            || self.session.is_closed()
            || binding.session != self.session.identity()
            || binding.revision != self.session.snapshot().revision()
        {
            return Err("Settings changed. Reopen this page before applying.".into());
        }
        Ok(binding)
    }
    fn refresh_settings(&mut self, binding: Binding, close: bool) {
        let page = binding.page;
        if close {
            self.dialog = None;
            self.settings_flow.current = None;
            return;
        }
        let mut dialog = settings::fresh(page, &self.session.snapshot());
        if let Dialog::Streaming(draft) = &mut dialog {
            if let Err(error) = draft.bind(&self.session) {
                self.message(true, error);
                return;
            }
        }
        self.settings_flow.current = Some(Binding {
            revision: self.session.snapshot().revision(),
            ..binding
        });
        self.dialog = Some(dialog);
    }
    pub(super) fn submit_settings(
        &mut self,
        page: SettingsPage,
        action: SettingsAction,
        token: Option<SettingsToken>,
        context: &egui::Context,
    ) {
        self.observe_settings_draft();
        let binding = match self.settings_binding(page) {
            Ok(value) => value,
            Err(error) => {
                self.message(true, error);
                return;
            }
        };
        if token != self.settings_flow.token() {
            self.message(
                true,
                "Settings action belongs to another draft. Reopen this page.",
            );
            return;
        }
        if self.settings_flow.pending.is_some() || self.io.busy() {
            self.message(true, "Wait for the current Settings operation to finish.");
            return;
        }
        if page!=SettingsPage::Advanced && action != SettingsAction::ReloadLanguage
            && self
                .dialog
                .as_ref()
                .is_some_and(|dialog| settings::unchanged(dialog, &self.session.snapshot()))
        {
            self.refresh_settings(binding, action == SettingsAction::Ok);
            return;
        }
        if page == SettingsPage::General {
            let Some(Dialog::General { language,projectors,snapping }) = &self.dialog else {
                return;
            };
            let language = language.clone();let projector_draft=*projectors;let snapping_draft=*snapping;
            if let Err(error)=projectors.command(self.session.snapshot().preferences().projectors()){self.message(true,error);return;}
            let projector_values=projectors.prepared();
            let snapping_values=match snapping.prepare_for(self.session.snapshot().preferences().preview_snapping()){Ok(value)=>value,Err(error)=>{self.message(true,error);return;}};
            if action!=SettingsAction::ReloadLanguage&&language==self.session.snapshot().preferences().language(){
                if self.publish_command_at(Some(binding.revision),Command::ConfigureGeneralSettings{language,projectors:projector_values,preview_snapping:Some(snapping_values)},false){self.refresh_settings(binding,action==SettingsAction::Ok);context.request_repaint();}return;
            }
            match self.io.submit(
                self.session.identity(),
                Job::LoadLocalization {
                    path: self.localization_path.clone(),
                    language: language.clone(),
                    revision: binding.revision,
                    commit: true,
                },
            ) {
                Ok(()) => {
                    // Zero is an invalid sentinel: even a broken frontend identity
                    // must drain/reject this owned result, never fall through to
                    // the generic localization publisher.
                    let request = self.io.request().unwrap_or(0);
                    self.settings_flow.pending = Some(Pending {
                        request,
                        binding,
                        action,
                        language,
                        projectors:(action!=SettingsAction::ReloadLanguage).then_some(projector_values),
                        projector_draft,
                        snapping_draft,
                    });
                    if request == 0 {
                        self.message(true, "Settings operation has no request identity.");
                    } else {
                        self.message(false, "Preparing localization…");
                    }
                }
                Err(error) => self.message(true, error),
            }
            context.request_repaint();
            return;
        }
        if action == SettingsAction::ReloadLanguage {
            self.message(true, "Reload is available only on the General page.");
            return;
        }
        if page==SettingsPage::Advanced&&!skylive_host::process_priority::available()&&self.dialog.as_ref().is_some_and(|dialog|settings::unchanged(dialog,&self.session.snapshot())){self.refresh_settings(binding,action==SettingsAction::Ok);return;}
        let prepared = match self.dialog.as_ref() {
            Some(dialog) => settings::prepare(dialog, &self.session),
            None => Err("Settings have no active draft. Reopen Settings.".to_owned()),
        };
        let command = match prepared {
            Ok(command) => command,
            Err(error) => {
                self.message(true, error);
                return;
            }
        };
        if matches!(&command,Command::ConfigureAdvancedSettings{..}){let token=SettingsToken{session:binding.session,revision:binding.revision,generation:binding.generation};self.request_advanced_process(command,token,action,context);return;}
        if let Command::SetProcessPriority{priority}=command {
            if !skylive_host::process_priority::available()&&self.dialog.as_ref().is_some_and(|dialog|settings::unchanged(dialog,&self.session.snapshot())){self.refresh_settings(binding,action==SettingsAction::Ok);return;}
            let token=SettingsToken{session:binding.session,revision:binding.revision,generation:binding.generation};self.request_process_priority(priority,Some((token,action)),false,context);return;
        }
        if self.publish_command_at(Some(binding.revision), command, false) {
            self.refresh_settings(binding, action == SettingsAction::Ok);
            context.request_repaint();
        }
    }
    pub(super) fn complete_priority_settings(&mut self,token:SettingsToken,action:SettingsAction,command:Command)->bool{
        let binding=match self.settings_binding(SettingsPage::Advanced){Ok(value)=>value,Err(error)=>{self.message(true,error);return false;}};
        if self.settings_flow.token()!=Some(token){self.message(true,"Settings action belongs to another draft. Reopen this page.");return false;}
        if self.publish_command_at(Some(token.revision),command,false){self.refresh_settings(binding,action==SettingsAction::Ok);true}else{false}
    }
    pub(super) fn complete_settings_io(
        &mut self,
        completion: crate::io::Completion,
        context: &egui::Context,
    ) {
        let Some(pending) = self.settings_flow.pending.take() else {
            return;
        };
        if completion.request != pending.request
            || completion.session != pending.binding.session
            || self.settings_flow.current != Some(pending.binding)
            || self.settings_binding(SettingsPage::General).is_err()
            || !matches!(&self.dialog,Some(Dialog::General{language,projectors,snapping})if language==&pending.language&&*projectors==pending.projector_draft&&*snapping==pending.snapping_draft)
        {
            self.message(
                true,
                "Settings language result is obsolete; the current draft was kept.",
            );
            return;
        }
        let localization = match completion.result {
            Ok(Reply::Localized {
                localization,
                revision,
                commit,
            }) if commit
                && revision == pending.binding.revision
                && localization.language() == pending.language =>
            {
                localization
            }
            Err(error) => {
                self.message(true, error);
                return;
            }
            _ => {
                self.message(
                    true,
                    "Settings language result is obsolete; the current draft was kept.",
                );
                return;
            }
        };
        let snapping=match pending.snapping_draft.prepare_for(self.session.snapshot().preferences().preview_snapping()){Ok(value)=>value,Err(error)=>{self.message(true,error);return;}};
        if self.publish_command_at(
            Some(pending.binding.revision),
            match pending.projectors{Some(projectors)=>Command::ConfigureGeneralSettings{language:pending.language,projectors,preview_snapping:Some(snapping)},None=>Command::SetLanguage{language:pending.language}},
            false,
        ) {
            self.localization = localization;
            crate::i18n::install(context, self.localization.clone(), &self.localization_path);
            self.refresh_settings(pending.binding, pending.action == SettingsAction::Ok);
            if pending.action==SettingsAction::ReloadLanguage{if let Some(Dialog::General{projectors,snapping,..})=&mut self.dialog{*projectors=pending.projector_draft;*snapping=pending.snapping_draft;}}
            context.request_repaint();
        }
    }
}

#[cfg(test)]
#[path = "app_settings_tests.rs"]
mod tests;

use super::*;
use crate::views::plugin_routes::{
    PluginEditorRequest, PluginEditorToken, PluginTarget, compatible,
};
use skylive_core::PluginOperation;

impl StudioApp {
    /// Bind only an unchanged canonical initial draft. Subsequent draft edits are
    /// presentation state; Apply must keep this exact owner/revision/instance.
    pub(super) fn bind_plugin_editor(&mut self, dialog: &mut Dialog) -> Result<(), String> {
        let model = self.session.snapshot();
        let binding = match dialog {
            Dialog::AudioFilters {
                binding,
                source,
                name,
                filters,
            } => {
                let canonical = model
                    .source(*source)
                    .ok_or("The selected source no longer exists.")?;
                if !canonical.kind().has_audio()
                    || canonical.name() != name.as_str()
                    || canonical.audio_filters() != filters.as_slice()
                {
                    return Err(
                        "Plugin settings changed. Reopen this editor before applying.".into(),
                    );
                }
                binding
            }
            Dialog::SceneNotes {
                binding,
                scene,
                name,
                text,
            } => {
                let canonical = model
                    .scene(*scene)
                    .ok_or("The selected scene no longer exists.")?;
                if canonical.name() != name.as_str() || canonical.notes() != text.as_str() {
                    return Err(
                        "Plugin settings changed. Reopen this editor before applying.".into(),
                    );
                }
                binding
            }
            _ => return Ok(()),
        };
        if binding.is_some() {
            return Err("Plugin settings changed. Reopen this editor before applying.".into());
        }
        let generation = self
            .plugin_dialog_sequence
            .checked_add(1)
            .ok_or("Plugin editor identity exhausted")?;
        self.plugin_dialog_sequence = generation;
        *binding = Some(PluginEditorToken {
            session: self.session.identity(),
            revision: model.revision(),
            generation,
        });
        Ok(())
    }
    pub(super) fn apply_plugin_editor(
        &mut self,
        token: Option<PluginEditorToken>,
        command: Command,
        context: &egui::Context,
    ) {
        let Some(token) = token else {
            self.message(
                true,
                "Plugin settings changed. Reopen this editor before applying.",
            );
            return;
        };
        let matches_draft = match (&self.dialog, &command) {
            (
                Some(Dialog::AudioFilters {
                    binding,
                    source,
                    filters,
                    ..
                }),
                Command::SetSourceAudioFilters {
                    source: target,
                    filters: submitted,
                },
            ) => *binding == Some(token) && source == target && filters == submitted,
            (
                Some(Dialog::SceneNotes {
                    binding,
                    scene,
                    text,
                    ..
                }),
                Command::SetSceneNotes {
                    scene: target,
                    text: submitted,
                },
            ) => *binding == Some(token) && scene == target && text == submitted,
            _ => false,
        };
        if !matches_draft
            || self.session.is_closed()
            || token.session != self.session.identity()
            || token.revision != self.session.snapshot().revision()
        {
            self.message(
                true,
                "Plugin settings changed. Reopen this editor before applying.",
            );
            return;
        }
        self.apply_at(Some(token.revision), command);
        context.request_repaint();
    }
    pub(super) fn open_plugin_editor(
        &mut self,
        request: PluginEditorRequest,
        context: &egui::Context,
    ) {
        if request.manager == 0
            || !matches!(&self.dialog,Some(Dialog::Plugins{token,..}) if *token==request.manager)
        {
            self.message(true, "Reopen Plugin Manager before choosing an editor.");
            return;
        }
        if request.session != self.session.identity()
            || request.revision != self.session.snapshot().revision()
        {
            self.message(
                true,
                "The document changed. Choose the plugin target again.",
            );
            return;
        }
        let model = self.session.snapshot();
        if !model.preferences().plugin_enabled(&request.plugin)
            || !skylive_core::plugin_entry_points(&request.plugin)
                .iter()
                .any(|entry| entry.operation == request.operation)
        {
            self.message(
                true,
                "This module has no enabled editor for this operation.",
            );
            return;
        }
        if !compatible(&model, request.operation, request.target) {
            self.message(
                true,
                "This source or scene cannot use the selected plugin operation.",
            );
            return;
        }
        if let Some(factory)=request.filter {
            if factory.operation()!=request.operation||factory.plugin_id()!=request.plugin {
                self.message(true,"This module has no enabled editor for this operation.");return;
            }
            if !views::plugin_routes::filter_capacity(&model,request.operation,request.target) {self.message(true,match factory {
                skylive_core::FilterFactory::Audio(_)=>"A source supports at most eight audio filters. Remove a filter before adding another.",
                skylive_core::FilterFactory::Video(_)=>"A source supports at most 32 video filters.",
            });return;}
        }
        let dialog = match (request.operation, request.target) {
            (PluginOperation::AudioFilters, PluginTarget::Source(id)) => model
                .source(id)
                .map(Dialog::audio_filters)
                .ok_or_else(|| "The selected source no longer exists.".to_owned()),
            (PluginOperation::VideoFilters, PluginTarget::Source(id)) => {
                views::video_filters::VideoFilterDraft::capture(request.session, &model, id)
                    .map(Dialog::VideoFilters)
            }
            (PluginOperation::VideoFilters, PluginTarget::Scene(id)) => {
                views::video_filters::VideoFilterDraft::capture_scene(request.session, &model, id)
                    .map(Dialog::VideoFilters)
            }
            (PluginOperation::SceneNotes, PluginTarget::Scene(id)) => model
                .scene(id)
                .map(Dialog::scene_notes)
                .ok_or_else(|| "The selected scene no longer exists.".to_owned()),
            _ => Err("This source or scene cannot use the selected plugin operation.".into()),
        };
        match dialog {
            Ok(dialog) => {
                // The ordinary path binds the unchanged canonical baseline.
                // Only after that succeeds may a factory change the local draft.
                self.intent(Intent::OpenDialog(dialog),context);
                if let Some(factory)=request.filter {
                    let result=match (&mut self.dialog,factory,request.target) {
                        (Some(Dialog::AudioFilters{binding,source,filters,..}),skylive_core::FilterFactory::Audio(factory),PluginTarget::Source(target))
                            if *source==target&&binding.is_some_and(|token|token.session==request.session&&token.revision==request.revision)=>{
                                if filters.len()>=skylive_core::MAX_AUDIO_FILTERS {Err("A source supports at most eight audio filters. Remove a filter before adding another.".to_owned())}
                                else {filters.push(factory.prototype());views::source_audio_filters::select_appended(context,*source,*binding,filters.len()-1);Ok(())}
                            },
                        (Some(Dialog::VideoFilters(draft)),skylive_core::FilterFactory::Video(factory),target)
                            if draft.session==request.session&&draft.revision==request.revision&&match (draft.target,target){
                                (views::video_filters::VideoFilterTarget::Source(source),PluginTarget::Source(id))=>source==id,
                                (views::video_filters::VideoFilterTarget::Scene(scene),PluginTarget::Scene(id))=>scene==id,
                                _=>false,
                            }=>draft.append_factory(factory),
                        _=>return,
                    };
                    if let Err(error)=result{self.message(true,error);}
                    context.request_repaint();
                }
            },
            Err(error) => self.message(true, error),
        }
    }
}

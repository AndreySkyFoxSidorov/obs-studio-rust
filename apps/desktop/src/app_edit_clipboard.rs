use super::*;
impl StudioApp {
    pub(super) fn source_clipboard_action(&mut self, context: &egui::Context, request: crate::source_clipboard::Request) {
        let selection = crate::source_clipboard::selected(context, &self.session, self.selected);
        match self.source_clipboard.execute(request, &self.session, selection) {
            Ok(crate::source_clipboard::Outcome::Copied) => self.message(false, "Sources copied"),
            Ok(crate::source_clipboard::Outcome::Paste { revision, command }) => {
                self.publish_command_at(Some(revision), command, false);
            }
            Err(error) => self.message(true, error),
        }
    }
    pub(super) fn mixer_filter_clipboard_action(&mut self,request:crate::edit_clipboard::FilterRequest){
        match self.edit_clipboard.execute_mixer(request,&self.session){
            Ok(crate::edit_clipboard::Outcome::Copied(message))=>self.message(false,message),
            Ok(crate::edit_clipboard::Outcome::Command{revision,command})=>{self.publish_command_at(Some(revision),command,false);}
            Err(error)=>{if let Some(message)=error.chrome(){self.message(true,message);}else{self.message(true,crate::status::Status::formatted("Clipboard operation failed: {error}",&[("{error}",error.to_string())]));}}
        }
    }
    pub(super) fn clipboard_action(&mut self, request: crate::edit_clipboard::Request) {
        self.clipboard_action_for_item(request, self.selected);
    }
    pub(super) fn context_transform_clipboard_action(&mut self, request: crate::edit_clipboard::Request) {
        if !matches!(request.action, crate::edit_clipboard::Action::CopyTransform | crate::edit_clipboard::Action::PasteTransform) {
            self.message(true, "This action requires a scene item.");
            return;
        }
        self.clipboard_action_for_item(request, Some(request.target.item));
    }
    fn clipboard_action_for_item(&mut self, request: crate::edit_clipboard::Request, selected: Option<Id>) {
        match self
            .edit_clipboard
            .execute(request, &self.session, selected)
        {
            Ok(crate::edit_clipboard::Outcome::Copied(message)) => self.message(false, message),
            Ok(crate::edit_clipboard::Outcome::Command { revision, command }) => {
                self.publish_command_at(Some(revision), command, false);
            }
            Err(error) => {
                if let Some(message) = error.chrome() {
                    self.message(true, message);
                } else {
                    self.message(
                        true,
                        crate::status::Status::formatted(
                            "Clipboard operation failed: {error}",
                            &[("{error}", error.to_string())],
                        ),
                    );
                }
            }
        }
    }
}

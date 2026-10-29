//! Source-row presentation command bridge. No renderer/capture/audio mutation.
use super::*;use crate::views::source_color_tags::{Action,Scope};
impl StudioApp{
 fn source_color_scope(&self,scope:Scope)->bool{!self.session.is_closed()&&scope.current(self.session.identity(),&self.session.snapshot(),self.session.preview(),self.selected)}
 pub(super) fn source_color_action(&mut self,action:Action,ctx:&egui::Context){
  let(scope,tag)=match action{
   Action::Set{scope,tag}=>{if !self.source_color_scope(scope)||self.dialog.is_some()||matches!(tag,skylive_core::SourceItemColorTag::Custom{..}){self.message(true,"The source row selection changed. Open Set Color again.");return;}(scope,tag)},
   Action::Apply{scope,token,rgba}=>{if !self.source_color_scope(scope)||!matches!(&self.dialog,Some(Dialog::SourceColorTag(draft))if draft.scope==scope&&draft.token==token&&draft.rgba==rgba){self.message(true,"The source row selection changed. Open Set Color again.");return;}(scope,skylive_core::SourceItemColorTag::Custom{rgba})}
  };
  if self.publish_command_at(Some(scope.revision),Command::SetSceneItemColorTag{scene:scope.container,item:scope.item,tag},true){crate::views::source_color_tags::clear_all(ctx);}ctx.request_repaint();
 }
}

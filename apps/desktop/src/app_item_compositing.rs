use super::*;
impl StudioApp {
    pub(super) fn apply_item_compositing(&mut self,request:views::item_compositing::Submission) {
        let model=self.session.snapshot();
        let current=request.focus.session==self.session.identity()&&request.focus.revision==model.revision()
            &&request.focus.root==self.session.preview()&&request.focus.selected==self.selected
            &&self.selected==Some(request.item)&&!self.session.is_closed()
            &&model.item_container(request.item).is_some_and(|(owner,item)|owner==request.container
                &&model.root_scene_for_container(owner)==Some(request.focus.root)
                &&item.source()==request.source&&item.compositing()==request.before);
        if !current {self.message(true,"The selected item changed. Open the compositing menu again.");return;}
        self.publish_command_at(Some(request.focus.revision),Command::SetItemCompositing {
            scene:request.container,item:request.item,settings:request.settings,
        },false);
    }
}

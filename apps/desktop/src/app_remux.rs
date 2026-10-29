//! Desktop adapter for the same headless file operation; no IO or joins here.
use super::*;
#[derive(Default)]pub(super) struct Runtime{pub owner:skylive_host::remux::Owner,pub view:views::remux::Panel}
impl StudioApp{
    pub(super) fn open_remux(&mut self){self.remux.view.open=true;}
    pub(super) fn remux_action(&mut self,action:views::remux::Action,ctx:&egui::Context){use views::remux::Action;match action{
        Action::Start=>{if self.remux.view.active(){return;}let jobs=self.remux.view.jobs();let context=ctx.clone();match self.remux.owner.start(jobs,Arc::new(move||context.request_repaint())){Ok(key)=>{self.remux.view.key=Some(key);self.remux.view.update(self.remux.owner.status());},Err(error)=>self.message(true,error)}},
        Action::Cancel(key)=>{if let Err(error)=self.remux.owner.cancel(key){self.message(true,error);}self.remux.view.status.cancel_requested=true;},
        Action::Close=>{if self.remux.view.active(){self.remux.owner.close();self.remux.view.closing=true;self.remux.view.status.cancel_requested=true;}else{self.remux.view.open=false;}}
    }}
    pub(super) fn poll_remux(&mut self,ctx:&egui::Context){self.remux.view.update(self.remux.owner.status());if self.remux.view.active(){ctx.request_repaint_after(std::time::Duration::from_millis(33));}}
    pub(super) fn wait_remux_close(&mut self,ctx:&egui::Context)->bool{self.remux.owner.close();if self.remux.owner.is_retired(){false}else{ctx.request_repaint_after(std::time::Duration::from_millis(8));true}}
}

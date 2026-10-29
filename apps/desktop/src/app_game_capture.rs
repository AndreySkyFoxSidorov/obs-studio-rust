//! Game source metadata, explicit capture grants and immutable frame union.
use super::*;
use skylive_host::game_capture::GameEvent;
impl StudioApp {
    pub(super) fn discover_games(&mut self,token:u64,session:u64,revision:u64,ctx:&egui::Context){
        if session!=self.session.identity()||revision!=self.session.snapshot().revision()||!matches!(&self.dialog,Some(Dialog::GameCapture(d))if d.token==token&&!d.loading){self.message(true,"The game source draft changed. Reopen its properties.");return;}
        match self.game.targets(&self.session){Ok(key)=>{self.pending_game=Some((token,key));if let Some(Dialog::GameCapture(d))=&mut self.dialog{d.loading=true;d.error=None;}},Err(error)=>self.message(true,error.to_string())}
        ctx.request_repaint();
    }
    pub(super) fn apply_game(&mut self,request:views::game_capture::Apply){
        if request.session!=self.session.identity()||request.revision!=self.session.snapshot().revision()||!matches!(&self.dialog,Some(Dialog::GameCapture(d))if d.matches(&request)&&!d.loading){self.message(true,"The game source draft changed. Reopen its properties.");return;}
        if self.publish_command_at(Some(request.revision),request.command(),true){
            if let Err(error)=self.game.authorize(&self.session,request.binding.target()){self.message(true,error.to_string());}
            else{self.message(false,"Game capture enabled for the selected process in this session.");}
        }
    }
    pub(super) fn game_control(&mut self,session:u64,revision:u64,source:Id,start:bool){
        if session!=self.session.identity()||revision!=self.session.snapshot().revision(){self.message(true,"The game source draft changed. Reopen its properties.");return;}
        let model=self.session.snapshot();let Some(skylive_core::SourceKind::GameCapture {binding})=model.source(source).map(|s|s.kind()) else{self.message(true,"Game source is unavailable.");return;};
        let result=if start{self.game.authorize(&self.session,binding.target())}else{self.game.revoke(&self.session,source)};
        match result{Ok(())=>self.message(false,if start {"Game capture enabled for the selected process in this session."}else{"Game capture permission revoked; native resources retire on their owner."}),Err(error)=>self.message(true,error.to_string())}
    }
    pub(super) fn update_game(&mut self,ctx:&egui::Context,active:&[FrameScene],now:MediaTime,base:Arc<CaptureSnapshot>)->Arc<CaptureSnapshot>{
        if let Err(error)=self.game.synchronize(&self.session,active,now){self.game_union.clear();if !matches!(error,skylive_host::game_capture::GameError::Busy){self.message(true,error.to_string());}return base;}
        for _ in 0..2 {match self.game.poll(&self.session){
            Ok(Some(GameEvent::Targets {key,result}))=>{if self.pending_game.take().is_some_and(|(token,expected)|expected==key&&matches!(&self.dialog,Some(Dialog::GameCapture(d))if d.token==token)){
                if let Some(Dialog::GameCapture(d))=&mut self.dialog{d.loading=false;match result{Ok(targets)=>d.targets=targets,Err(error)=>d.error=Some(error.to_string())}}
            }},Ok(None)=>break,Err(skylive_host::game_capture::GameError::Busy)=>break,Err(error)=>{self.pending_game=None;if let Some(Dialog::GameCapture(d))=&mut self.dialog{d.loading=false;d.error=Some(error.to_string());}self.message(true,error.to_string());break;}
        }}
        if let Some(Dialog::GameCapture(d))=&mut self.dialog {if let views::game_capture::Edit::Rebind {source}=d.edit {
            let snapshot=self.game.snapshot();d.observed=snapshot.frames().find(|(id,_)|*id==source).map(|(_,frame)|frame.clone());
            if let Some(problem)=snapshot.problems().iter().find(|p|p.source==source){d.error=Some(problem.error.to_string());}else if d.observed.is_some(){d.error=None;}
        }}
        if self.game.has_active_sources()||self.pending_game.is_some(){ctx.request_repaint_after(std::time::Duration::from_millis(10));}
        match self.game_union.merge(&self.session,base.clone(),&self.game){Ok(frame)=>frame,Err(error)=>{self.game_union.clear();self.message(true,error.to_string());base}}
    }
}

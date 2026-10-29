//! Metadata-only mask preparation on the existing single admitted I/O owner.
use super::*;
use views::video_filters::{MaskKey,VideoFilterTarget};
struct Pending {request:u64,key:MaskKey}
#[derive(Default)]
pub(super) struct Flow {pending:Option<Pending>}
impl StudioApp {
    pub(super) fn video_filter_current(&self,token:u64,session:u64,revision:u64,target:VideoFilterTarget,filters:&[skylive_core::VideoFilter])->bool {
        !self.session.is_closed()&&session==self.session.identity()&&revision==self.session.snapshot().revision()
            &&matches!(&self.dialog,Some(Dialog::VideoFilters(d)) if token!=0&&d.token==token&&d.session==session&&d.revision==revision&&d.target==target&&d.command().is_ok_and(|command| match command {
                Command::SetSourceVideoFilters {filters:current,..} | Command::SetSceneVideoFilters {filters:current,..} => current==filters,
                _ => false,
            }))
    }
    pub(super) fn prepare_mask(&mut self,key:MaskKey){
        if self.io.busy()||self.mask_flow.pending.is_some()||self.session.is_closed()||key.session!=self.session.identity()||key.revision!=self.session.snapshot().revision()
            ||key.path.is_empty()||key.path.len()>4096||key.path.chars().any(char::is_control)
            ||!matches!(&self.dialog,Some(Dialog::VideoFilters(d)) if !d.mask_busy()&&d.mask_key().as_ref()==Some(&key)) {
            self.message(true,"Mask image target changed; load the current draft again.");return;
        }
        match self.io.submit(key.session,Job::PrepareMask {path:PathBuf::from(&key.path)}){
            Ok(())=>{if let Some(request)=self.io.request(){if let Some(Dialog::VideoFilters(d))=&mut self.dialog {if d.mask_begin(&key){self.mask_flow.pending=Some(Pending {request,key});}}}},
            Err(error)=>self.message(true,error),
        }
    }
    pub(super) fn mask_completion(&mut self,completion:crate::io::Completion)->Option<crate::io::Completion>{
        if self.mask_flow.pending.as_ref().is_none_or(|p|p.request!=completion.request){return Some(completion);}
        let Some(pending)=self.mask_flow.pending.take()else{return Some(completion);};
        let current=!self.session.is_closed()&&completion.session==pending.key.session&&pending.key.session==self.session.identity()&&pending.key.revision==self.session.snapshot().revision();
        let result=if !current {Err("Mask image target changed; load the current draft again.".into())}else{match completion.result {Ok(Reply::MaskPrepared(asset))=>Ok(asset),Err(error)=>Err(error),_=>Err("Mask image returned an unexpected receipt.".into())}};
        if let Some(Dialog::VideoFilters(d))=&mut self.dialog {d.mask_finish(&pending.key,result);}
        None
    }
}

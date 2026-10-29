//! LUT proof preparation uses the existing bounded I/O actor, never a GUI parse.
use super::*;
use views::video_filters::LutKey;
struct Pending {request:u64,key:LutKey}
#[derive(Default)]
pub(super) struct Flow {pending:Option<Pending>}
impl StudioApp {
    pub(super) fn prepare_lut(&mut self,key:LutKey){
        if self.io.busy()||self.lut_flow.pending.is_some()||self.session.is_closed()||key.session!=self.session.identity()||key.revision!=self.session.snapshot().revision()||key.path.is_empty()||key.path.len()>4096||key.path.chars().any(char::is_control)
            ||!matches!(&self.dialog,Some(Dialog::VideoFilters(d)) if !d.lut_busy()&&d.lut_key().as_ref()==Some(&key)){self.message(true,"LUT target changed; load the current draft again.");return;}
        match self.io.submit(key.session,Job::PrepareLut {path:PathBuf::from(&key.path)}){
            Ok(())=>if let Some(request)=self.io.request(){if let Some(Dialog::VideoFilters(d))=&mut self.dialog{if d.lut_begin(&key){self.lut_flow.pending=Some(Pending {request,key});}}},
            Err(error)=>self.message(true,error),
        }
    }
    pub(super) fn lut_completion(&mut self,completion:crate::io::Completion)->Option<crate::io::Completion>{
        if self.lut_flow.pending.as_ref().is_none_or(|p|p.request!=completion.request){return Some(completion);}
        let Some(pending)=self.lut_flow.pending.take()else{return Some(completion);};
        let current=!self.session.is_closed()&&completion.session==pending.key.session&&pending.key.session==self.session.identity()&&pending.key.revision==self.session.snapshot().revision();
        let result=if !current{Err("LUT target changed; load the current draft again.".into())}else{match completion.result{Ok(Reply::LutPrepared(asset))=>Ok(asset),Err(error)=>Err(error),_=>Err("LUT preparation returned an unexpected receipt.".into())}};
        if let Some(Dialog::VideoFilters(d))=&mut self.dialog{d.lut_finish(&pending.key,result);}None
    }
}

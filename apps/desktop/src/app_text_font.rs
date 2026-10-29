//! Explicit font preparation reuses the bounded filesystem owner. No GUI file IO.
use super::*;
use views::text_source::FontKey;
struct Pending{request:u64,key:FontKey,system:bool}
#[derive(Default)]pub(super) struct Flow{next:u64,pending:Option<Pending>}
impl Flow{pub(super) fn open(&mut self,draft:&mut views::text_source::TextDraft,session:&Session)->Result<(),String>{self.next=self.next.checked_add(1).ok_or("Text dialog identity exhausted")?;draft.token=self.next;draft.session=session.identity();draft.revision=session.snapshot().revision();Ok(())}}
impl StudioApp{
    pub(super) fn prepare_text_font(&mut self,key:FontKey){
        if self.session.is_closed()||key.session!=self.session.identity()||key.revision!=self.session.snapshot().revision()||self.io.busy()||self.text_font.pending.is_some()
            ||key.path.is_empty()||key.path.len()>4096||key.path.chars().any(char::is_control)
            ||!matches!(&self.dialog,Some(Dialog::Text(d))if !d.font_loading&&d.font_key().as_ref()==Ok(&key)){
            self.message(true,"The text or font draft changed; prepare the current file again.");return;
        }
        match self.io.submit(key.session,Job::PrepareFont{path:PathBuf::from(&key.path)}){
            Ok(())=>if let Some(request)=self.io.request(){if let Some(Dialog::Text(d))=&mut self.dialog{d.font_loading=true;d.font_error=None;}self.text_font.pending=Some(Pending{request,key,system:false});},
            Err(error)=>self.message(true,error),
        }
    }
    pub(super) fn discover_text_system_fonts(&mut self,key:FontKey){
        if self.session.is_closed()||key.session!=self.session.identity()||key.revision!=self.session.snapshot().revision()||self.io.busy()||self.text_font.pending.is_some()
            ||!matches!(&self.dialog,Some(Dialog::Text(d))if !d.font_loading&&d.font_key().as_ref()==Ok(&key)){
            self.message(true,"The text or font draft changed; discover installed fonts again.");return;
        }
        match self.io.submit(key.session,Job::DiscoverSystemFonts){
            Ok(())=>if let Some(request)=self.io.request(){if let Some(Dialog::Text(d))=&mut self.dialog{d.font_loading=true;d.font_error=None;}self.text_font.pending=Some(Pending{request,key,system:true});},
            Err(error)=>self.message(true,error),
        }
    }
    pub(super) fn text_font_completion(&mut self,completion:crate::io::Completion)->Option<crate::io::Completion>{
        if self.text_font.pending.as_ref().is_none_or(|p|p.request!=completion.request){return Some(completion);}
        let Some(pending)=self.text_font.pending.take()else{return Some(completion);};
        let current=!self.session.is_closed()&&completion.session==pending.key.session&&pending.key.session==self.session.identity()&&pending.key.revision==self.session.snapshot().revision();
        if pending.system{
            let result=if !current{Err("The document changed; discover installed fonts from its current Text Properties.".into())}else{match completion.result{Ok(Reply::SystemFontsDiscovered(catalog))=>Ok(catalog),Err(error)=>Err(error),_=>Err("System font discovery returned an unexpected receipt.".into())}};
            if let Some(Dialog::Text(d))=&mut self.dialog{d.system_finish(&pending.key,result);}return None;
        }
        let result=if !current{Err("The document changed; prepare the font from its current Text Properties.".into())}else{match completion.result{Ok(Reply::FontPrepared(catalog))=>Ok(catalog),Err(error)=>Err(error),_=>Err("Font preparation returned an unexpected receipt.".into())}};
        if let Some(Dialog::Text(d))=&mut self.dialog{d.font_finish(&pending.key,result);}None
    }
    pub(super) fn apply_text_source(&mut self,key:FontKey){
        if self.session.is_closed()||key.session!=self.session.identity()||key.revision!=self.session.snapshot().revision(){self.message(true,"The document changed; reopen Text Properties.");return;}
        let command=match &self.dialog{Some(Dialog::Text(d))if d.font_key().as_ref()==Ok(&key)=>d.command(),_=>Err("The text or font draft changed; prepare the current file again.".into())};
        let command=match command{Ok(command)=>command,Err(error)=>{self.message(true,error);return;}};
        if self.publish_command_at(Some(key.revision),command,true){
            // Retry identical restored files without inventing a canonical edit.
            // Program resource generations propagate to Replay/NDI/projectors.
            if let Err(error)=self.recording.invalidate_resources(){self.stop_recording();self.message(true,error);}
            if let Err(error)=self.streaming.invalidate_resources(){self.stop_streaming();self.message(true,error);}
            if let Err(error)=self.virtual_camera.invalidate_resources(){self.stop_virtual_camera();self.message(true,error);}
            if let Err(error)=self.preview.retry_resources().and_then(|()|self.program.retry_resources()){self.message(true,error.to_string());}
        }
    }
}

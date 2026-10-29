//! App-side preparation receipt: I/O owns bytes and decoding, Session owns the
//! atomic metadata edit, and dialog identity owns only the draft presentation.
use super::*;
use crate::views::image_slideshow::Submission;
impl StudioApp {
    pub(super) fn prepare_slideshow(&mut self,submission:Submission){
        let current=matches!(&self.dialog,Some(Dialog::ImageSlideShow(d)) if d.token==submission.token && d.current(&self.session)&&submission.matches(d));
        if !current||submission.session!=self.session.identity()||submission.revision!=self.session.snapshot().revision(){self.message(true,"Slideshow properties are stale; reopen Properties");return;}
        if self.io.busy(){self.message(true,"A document operation is already active");return;}
        let job=Job::PrepareSlideShow {entries:submission.entries.clone(),token:submission.token};
        match self.io.submit(self.session.identity(),job){
            Ok(())=>{
                if let Some(request)=self.io.request(){self.pending_slideshow=Some((request,submission));}
                if let Some(Dialog::ImageSlideShow(d))=&mut self.dialog{d.loading=true;}
                self.message(false,"Preparing slideshow images…");
            },
            Err(error)=>self.message(true,error),
        }
    }
    pub(super) fn slideshow_completion(&mut self,completion:crate::io::Completion)->Option<crate::io::Completion>{
        let matches=self.pending_slideshow.as_ref().is_some_and(|(request,_)|*request==completion.request);
        if !matches {return Some(completion);}
        let Some((_,submission))=self.pending_slideshow.take() else{return Some(completion);};
        let current=matches!(&self.dialog,Some(Dialog::ImageSlideShow(d)) if d.token==submission.token && d.current(&self.session)&&submission.matches(d));
        if let Some(Dialog::ImageSlideShow(d))=&mut self.dialog{if d.token==submission.token{d.loading=false;}}
        if !current||completion.session!=submission.session{self.message(true,"Slideshow preparation is stale; current source was retained");return None;}
        let prepared=match completion.result {
            Ok(Reply::SlideShowPrepared {prepared,token}) if token==submission.token=>prepared,
            Ok(_)=>{self.message(true,"Slideshow preparation returned an unexpected receipt");return None;},
            Err(error)=>{self.message(true,crate::status::Status::formatted("Slideshow preparation failed; current source retained. {detail}",&[("{detail}",error)]));return None;},
        };
        let revision=submission.revision;
        match submission.command(&self.session,prepared){
            Ok((command,restart))=>{
                if self.publish_command_at(Some(revision),command,true){
                    self.restart_image=restart;
                    if let Err(error)=self.virtual_camera.invalidate_resources() {self.stop_virtual_camera();self.message(true,error);}
                    // Explicit re-prepare refreshes the shared image resource
                    // owners even when accepted metadata is a canonical no-op.
                    if let Err(error)=self.preview.retry_resources().and_then(|()|self.program.retry_resources()) {self.message(true,error.to_string());}
                }
            },
            Err(error)=>self.message(true,crate::status::Status::formatted("Slideshow preparation failed; current source retained. {detail}",&[("{detail}",error)])),
        }
        None
    }
}

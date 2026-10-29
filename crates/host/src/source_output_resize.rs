//! Current intrinsic metadata receipt. No acquisition, decode, pixels or IO.
use crate::capture::{CaptureBinding,CaptureSnapshot};
use skylive_core::{Command,Id,ProfileId,Session,SourceDefinitionEpoch,SourceOutputSize,SourceKind};
#[derive(Clone,Debug)]
pub struct Plan {session:u64,revision:u64,profile:ProfileId,scene:Id,item:Id,source:Id,definition:SourceDefinitionEpoch,binding:Option<CaptureBinding>,capture_epoch:Option<u64>,size:SourceOutputSize,name:String}
#[derive(Clone,Debug,PartialEq,Eq)]
pub struct Error(pub &'static str);
impl std::fmt::Display for Error{fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result{f.write_str(self.0)}}
impl std::error::Error for Error{}
impl Plan {
    pub fn prepare(session:&Session,capture:&CaptureSnapshot,scene:Id,item:Id)->Result<Self,Error>{
        if session.is_closed(){return Err(Error("The scene collection is closed."));}let model=session.snapshot();
        let row=model.scene_container(scene).and_then(|scene|scene.items().iter().find(|row|row.id()==item)).ok_or(Error("The selected source item no longer exists."))?;
        let source=model.source(row.source()).ok_or(Error("The selected source no longer exists."))?;
        let definition=session.source_definition_epoch(source.id()).ok_or(Error("Source definition epoch is absent."))?;
        let basis=model.source_video_dimensions(source.kind()).map_err(|_|Error("Source intrinsic dimensions could not be resolved."))?.ok_or(Error("Selected source has no video dimensions."))?;
        let dynamic=matches!(source.kind(),SourceKind::DisplayCapture{..}|SourceKind::WindowCapture{..}|SourceKind::VideoCapture{..}|SourceKind::MediaSource{..}|SourceKind::NdiSource{..}|SourceKind::GameCapture{..}|SourceKind::Browser{..});
        let (raw,binding,capture_epoch)=if dynamic {
            if capture.session()!=session.identity(){return Err(Error("Source capture belongs to another collection."));}
            if capture.problem(source.id()).is_some(){return Err(Error("The selected source has a current capture error; resize output is unavailable."));}
            let frame=capture.frame(source.id()).filter(|frame|frame.binding.matches_source(source.kind())).ok_or(Error("Wait for a real frame from the selected source before resizing output."))?;
            ((frame.image.width(),frame.image.height()),Some(frame.binding.clone()),Some(capture.epoch()))
        }else{(basis,None,None)};
        let size=model.source_output_size(scene,item,raw.0,raw.1).map_err(|error|match error{skylive_core::Error::Invalid(detail)|skylive_core::Error::Limit(detail)=>Error(detail),_=>Error("The selected source size, filters or rounded resolution exceed the supported output profile.")})?;
        // Core source_output_size also validates the selected fit under the new
        // canvas. No cloned model, new Session, rendering or resource IO here.
        Ok(Self{session:session.identity(),revision:model.revision(),profile:model.profiles().selected_id(),scene,item,source:source.id(),definition,binding,capture_epoch,size,name:source.name().into()})
    }
    pub fn session(&self)->u64{self.session}
    pub fn revision(&self)->u64{self.revision}
    pub fn scene(&self)->Id{self.scene}
    pub fn item(&self)->Id{self.item}
    pub fn name(&self)->&str{&self.name}
    pub fn size(&self)->SourceOutputSize{self.size}
    pub fn validate(&self,session:&Session,capture:&CaptureSnapshot)->Result<(),Error>{
        if session.is_closed()||session.identity()!=self.session||session.snapshot().revision()!=self.revision||session.snapshot().profiles().selected_id()!=self.profile||session.source_definition_epoch(self.source)!=Some(self.definition){return Err(Error("The source, profile or collection changed. Reopen Resize output."));}
        if let Some(binding)=&self.binding {
            if capture.session()!=self.session||Some(capture.epoch())!=self.capture_epoch||capture.problem(self.source).is_some(){return Err(Error("Source frame dimensions or capture identity changed. Reopen Resize output."));}
            let frame=capture.frame(self.source).filter(|frame|&frame.binding==binding).ok_or(Error("Wait for a real frame from the selected source before resizing output."))?;
            if (frame.image.width(),frame.image.height())!=(self.size.raw_width,self.size.raw_height){return Err(Error("Source frame dimensions or capture identity changed. Reopen Resize output."));}
        }
        Ok(())
    }
    pub fn command(&self,session:&Session,capture:&CaptureSnapshot)->Result<Command,Error>{
        self.validate(session,capture)?;
        Ok(Command::ResizeOutputToSource{scene:self.scene,item:self.item,raw_width:self.size.raw_width,raw_height:self.size.raw_height})
    }
}

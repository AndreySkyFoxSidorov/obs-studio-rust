//! File choice precedes an immutable current-frame request. No GUI pixel work.
use super::*;
use skylive_host::screenshot::{self,Target};
use skylive_host::native_path_dialog::{self,Kind};
#[derive(Clone,Copy)]
struct Key {token:u64,session:u64,revision:u64,target:Target}
enum Phase {Choosing,Saving {path:PathBuf,time_ns:u64}}
struct Pending {request:u64,key:Key,phase:Phase}
#[derive(Default)]
pub(super) struct Flow {sequence:u64,pending:Option<Pending>,chosen:Option<(Key,PathBuf)>}
impl StudioApp {
    pub(super) fn screenshot_hotkey(&mut self,selected:bool,ctx:&egui::Context){
        let model=self.session.snapshot();
        let target=if selected {
            let source=self.selected.and_then(|id|model.item_container(id)).filter(|(owner,_)|model.root_scene_for_container(*owner)==Some(self.session.preview())).map(|(_,item)|item.source());
            let Some(source)=source else{self.message(true,"Select a video source before saving its screenshot.");return;};Target::Source {source}
        }else{Target::Program};
        self.open_screenshot(self.session.identity(),model.revision(),target,ctx);
    }
    pub(super) fn screenshot_no_frame(&mut self){if self.screenshot.chosen.take().is_some(){self.message(true,"The current frame is unavailable. Try saving the screenshot again.");}}
    fn screenshot_current(&self,key:Key)->bool{
        if self.session.is_closed()||key.token!=self.screenshot.sequence||key.session!=self.session.identity()||key.revision!=self.session.snapshot().revision(){return false;}
        let model=self.session.snapshot();match key.target {Target::Source {source}=>model.source(source).is_some_and(|s|model.source_video_dimensions(s.kind()).is_ok_and(|v|v.is_some())),Target::Scene {scene}=>model.scene(scene).is_some(),_=>true}
    }
    pub(super) fn open_screenshot(&mut self,session:u64,revision:u64,target:Target,ctx:&egui::Context){
        if self.io.busy()||self.documents.modal_open()||self.screenshot.pending.is_some()||self.screenshot.chosen.is_some(){self.message(true,"Finish the active operation before saving a screenshot.");return;}
        let Some(token)=self.screenshot.sequence.checked_add(1)else{self.message(true,"Screenshot identity exhausted.");return;};self.screenshot.sequence=token;
        let key=Key {token,session,revision,target};if !self.screenshot_current(key){self.message(true,"The screenshot target changed. Select it again.");return;}
        let Some(owner)=self.native_browse.owner else{self.message(true,"The screenshot chooser requires an application window.");return;};
        let job=Job::BrowsePaths(native_path_dialog::Request {owner_hwnd:owner,kind:Kind::ScreenshotSave,title:crate::i18n::tr_context(ctx,target.label()),initial:format!("screenshot-{token}.png"),filter_label:crate::i18n::tr_context(ctx,"PNG image"),all_files_label:crate::i18n::tr_context(ctx,"All files")});
        match self.io.submit(session,job){Ok(())=>{if let Some(request)=self.io.request(){self.screenshot.pending=Some(Pending {request,key,phase:Phase::Choosing});}},Err(error)=>self.message(true,error)}
    }
    pub(super) fn screenshot_completion(&mut self,completion:crate::io::Completion,ctx:&egui::Context)->Option<crate::io::Completion>{
        if self.screenshot.pending.as_ref().is_none_or(|p|p.request!=completion.request){return Some(completion);}
        let pending=self.screenshot.pending.take()?;
        if let Phase::Saving {path,time_ns}=&pending.phase {
            match completion.result {
                Ok(Reply::ScreenshotSaved(receipt)) if completion.session==pending.key.session
                    &&receipt.session==pending.key.session&&receipt.revision==pending.key.revision
                    &&receipt.target==pending.key.target&&receipt.path==*path&&receipt.time_ns==*time_ns=>
                        self.message(false,crate::status::Status::formatted("Screenshot saved to {path}",&[("{path}",receipt.path.to_string_lossy().into_owned())])),
                Err(error)=>self.message(true,error),
                _=>self.message(true,"Screenshot receipt has no matching request."),
            }
            // Later edits or a closed document do not invalidate a completed
            // file publication of the exact frame already admitted to the worker.
            return None;
        }
        if matches!(&completion.result,Ok(Reply::PathsChosen(None))){return None;}
        if completion.session!=pending.key.session||!self.screenshot_current(pending.key){self.message(true,"The screenshot target changed. Select it again.");return None;}
        match completion.result {
            Ok(Reply::PathsChosen(Some(paths))) if paths.len()==1=>{
                let path=PathBuf::from(&paths[0]);match screenshot::validate_path(&path){Ok(())=>{self.screenshot.chosen=Some((pending.key,path));ctx.request_repaint();},Err(error)=>self.message(true,error)}
            },Err(error)=>self.message(true,error),_=>self.message(true,"Screenshot receipt has no matching request."),
        }None
    }
    pub(super) fn screenshot_frame(&mut self,ctx:&egui::Context,playback:&skylive_host::playback::PlaybackSnapshot,output:&skylive_host::transition::FrameComposition,capture:&Arc<CaptureSnapshot>){
        if self.io.busy(){return;}let Some((key,path))=self.screenshot.chosen.take()else{return;};
        if !self.screenshot_current(key)||playback.session()!=key.session||playback.revision()!=key.revision||playback.now()!=self.frame_now{self.message(true,"The screenshot target changed. Select it again.");return;}
        let frame=skylive_host::recording::RecordingFrame {session:key.session,time:self.frame_now,model:self.session.snapshot(),output:output.clone(),playback:playback.clone(),capture:capture.clone(),resource_generation:self.preview.resource_generation()};
        let phase=Phase::Saving {path:path.clone(),time_ns:self.frame_now.0};
        let request=screenshot::Request {path,target:key.target,frame,preview:self.session.preview(),studio:self.studio,gpu:self._render_gpu.clone()};
        match self.io.submit(key.session,Job::Screenshot(Box::new(request))){Ok(())=>{if let Some(request)=self.io.request(){self.screenshot.pending=Some(Pending {request,key,phase});}ctx.request_repaint();},Err(error)=>self.message(true,error)}
    }
}

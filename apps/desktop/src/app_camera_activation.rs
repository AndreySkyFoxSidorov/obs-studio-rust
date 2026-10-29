//! Frontend delegates source AV lifecycle to the same host owner as the CLI.
use super::*;
impl StudioApp {
    pub(super) fn request_camera_av(&mut self,source:Option<Id>,enabled:bool,context:&egui::Context){
        let result=match source {Some(source)=>self.camera_activation.request(&self.session,source,enabled,self.frame_now),None=>self.camera_activation.stop_all(&self.session,self.frame_now)};
        match result {
            Ok(_)=>{
                // No native calls. Install publication/acquisition eligibility
                // immediately; worker admission and acknowledgements follow.
                self.camera_start=None;
                if let Err(error)=self.camera_activation.synchronize(&self.session,&mut self.camera,&mut self.audio){self.camera_error=Some(error.to_string());}
                self.capture_union.set_camera_activation(self.camera_activation.snapshot());
                self.message(false,if enabled {"Camera AV activation requested"}else{"Camera AV deactivation requested; waiting for both legs"});
            },
            Err(error)=>self.message(true,error.to_string()),
        }
        context.request_repaint();
    }
    pub(super) fn drive_camera_av(&mut self,context:&egui::Context,output:Option<&skylive_host::transition::FrameComposition>,now:MediaTime){
        let before=self.camera_activation.status();
        match self.camera_activation.drive(&self.session,&mut self.camera,&mut self.audio,output,now){
            Ok(status)=>{
                self.capture_union.set_camera_activation(self.camera_activation.snapshot());
                if status.pending {context.request_repaint_after(std::time::Duration::from_millis(5));}
                if before.pending&&!status.pending {
                    if status.fully_acknowledged {self.camera_error=None;self.message(false,if status.desired_enabled {"Camera AV activation acknowledged"}else{"Camera AV deactivation acknowledged"});}
                    else {self.message(true,"Camera AV operation has a partial failure; inspect both leg statuses");}
                }
            },
            Err(error)=>self.camera_error=Some(error.to_string()),
        }
    }
}

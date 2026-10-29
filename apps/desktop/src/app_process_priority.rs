//! One native-priority transaction, current Settings token and startup hook.
use super::*;
use skylive_host::process_priority::{Owner,Key,Receipt};
use views::settings::{SettingsAction,SettingsToken};
struct Pending{key:Key,priority:skylive_core::ProcessPriority,command:Command,settings:Option<(SettingsToken,SettingsAction)>,startup:bool}
#[derive(Default)]pub(super) struct Runtime{owner:Option<Owner>,pending:Option<Pending>,startup:Option<skylive_core::ProcessPriority>}
impl Runtime{
    pub(super) fn advanced_pending(&self)->bool{self.pending.as_ref().is_some_and(|p|matches!(&p.command,Command::ConfigureAdvancedSettings{..}))}
    pub fn invalidate(&mut self){if self.pending.is_some(){if let Some(owner)=&mut self.owner{if let Err(error)=owner.invalidate(){eprintln!("process priority scope revocation: {error}");owner.close();}}}}
    pub fn close(&mut self){self.pending=None;if let Some(owner)=&mut self.owner{owner.close();}}}
impl StudioApp{
    /// Called only after successful installed-preferences decode/publication,
    /// not by page open, headless construction or an invalid/missing file.
    pub(super) fn arm_startup_process_priority(&mut self){let priority=self.session.snapshot().preferences().process_priority();if !priority.is_default(){self.process_priority.startup=Some(priority);}}
    pub(super) fn request_process_priority(&mut self,priority:skylive_core::ProcessPriority,settings:Option<(SettingsToken,SettingsAction)>,startup:bool,context:&egui::Context){
        if !skylive_host::process_priority::available(){self.message(true,"Process priority control is available only on Windows.");return;}
        if self.process_priority.owner.as_ref().is_some_and(Owner::is_closed){self.process_priority.owner=None;}
        if self.process_priority.owner.is_none(){let ctx=context.clone();match Owner::new(Arc::new(move||ctx.request_repaint())){Ok(owner)=>self.process_priority.owner=Some(owner),Err(error)=>{self.message(true,error.to_string());return;}}}
        let revision=settings.map_or_else(||self.session.snapshot().revision(),|(token,_)|token.revision);
        let result=self.process_priority.owner.as_mut().ok_or_else(||"process priority owner missing".to_owned()).and_then(|owner|owner.request(&self.session,revision,priority));
        match result{Ok(submitted)=>{self.process_priority.pending=Some(Pending{key:submitted.key,priority,command:Command::SetProcessPriority{priority},settings,startup});self.message(false,"Applying process priority…");},Err(error)=>self.message(true,error)}
    }
    pub(super) fn request_advanced_process(&mut self,command:Command,token:SettingsToken,action:SettingsAction,context:&egui::Context){self.request_advanced_command(command,Some((token,action)),context);}
    pub(super) fn request_advanced_command(&mut self,command:Command,settings:Option<(SettingsToken,SettingsAction)>,context:&egui::Context){
        let Command::ConfigureAdvancedSettings{profile,priority,reconnect}=&command else{return;};
        if *profile!=self.session.snapshot().profiles().selected_id(){self.message(true,"The profile changed. Reopen Advanced settings before applying.");return;}if let Err(error)=reconnect.validate(){self.message(true,error.to_string());return;}
        if self.streaming.active()&&self.streaming.blocks_command(&command){self.message(true,"Stop streaming before changing automatic reconnect settings.");return;}
        let previous=self.process_priority.pending.as_ref().map(|p|p.key);
        self.request_process_priority(*priority,settings,false,context);
        if let Some(pending)=&mut self.process_priority.pending{if Some(pending.key)!=previous&&pending.settings==settings{pending.command=command;}}
    }
    pub(super) fn poll_process_priority(&mut self,context:&egui::Context){
        if let Some(priority)=self.process_priority.startup.take(){self.request_process_priority(priority,None,true,context);}
        let stale_draft=self.process_priority.pending.as_ref().and_then(|p|p.settings).is_some_and(|(token,_)|self.settings_flow.token()!=Some(token)||!matches!(&self.dialog,Some(Dialog::AdvancedProcess(_))));
        if stale_draft{if let Some(owner)=&mut self.process_priority.owner{if let Err(error)=owner.invalidate(){self.message(true,error);}}}
        let mut receipts=Vec::with_capacity(2);
        if let Some(owner)=&mut self.process_priority.owner{if let Err(error)=owner.observe(&self.session){self.message(true,error);return;}for _ in 0..2{if let Some(receipt)=owner.poll(){receipts.push(receipt);}else{break;}}}
        for receipt in receipts{
            let matches=self.process_priority.pending.as_ref().is_some_and(|p|p.key==receipt.key);
            if !matches{if receipt.written{eprintln!("superseded process priority receipt: {:?}",receipt);}continue;}
            let pending=self.process_priority.pending.take();let Some(pending)=pending else{continue;};
            let fresh=self.process_priority.owner.as_ref().is_some_and(|owner|owner.accepts(receipt.key,&self.session))&&!stale_draft;
            if receipt.verified()&&fresh{
                let published=if pending.startup{true}else if let Some((token,action))=pending.settings{self.complete_priority_settings(token,action,pending.command)}else{self.publish_command_at(Some(receipt.key.revision),pending.command,false)};
                if published{self.message(false,crate::status::Status::formatted("Windows confirmed process priority class {class}.",&[("{class}",receipt.readback.map_or_else(||"unknown".into(),|v|v.to_string()))]));}
                else{let detail=self.status.clone();self.message(true,crate::status::Status::formatted("Windows confirmed priority, but settings could not be published: {detail}",&[("{detail}",detail)]));}
            }else{
                let detail=receipt.error.clone().unwrap_or_else(||"Settings changed. Reopen Advanced settings.".into());
                self.message(true,crate::status::Status::formatted(if receipt.written{"Process priority changed, but its result could not be published: {detail}"}else{"Process priority request failed: {detail}"},&[("{detail}",detail)]));
            }
        }
        if self.process_priority.owner.as_ref().is_some_and(Owner::is_closed)&&self.process_priority.pending.is_some(){self.process_priority.pending=None;let error=self.process_priority.owner.as_mut().and_then(|owner|owner.status().failure).unwrap_or_else(||"Process priority owner closed before publication.".into());self.message(true,error);}
        let (busy,last)=if let Some(owner)=&mut self.process_priority.owner{let status=owner.status();(self.process_priority.pending.is_some(),status.last.filter(|r|r.key.session==self.session.identity()))}else{(false,None)};
        if let Some(Dialog::AdvancedProcess(draft))=&mut self.dialog{draft.busy=busy;draft.last=last;}
    }
    pub(super) fn wait_process_priority_close(&mut self,context:&egui::Context)->bool{
        self.process_priority.close();let Some(owner)=&mut self.process_priority.owner else{return false;};
        if owner.is_closed(){let status=owner.status();if let Some(error)=status.restore.and_then(|r|r.error){self.message(true,crate::status::Status::formatted("Process priority restoration failed: {detail}",&[("{detail}",error)]));}return false;}
        context.request_repaint_after(std::time::Duration::from_millis(8));true
    }
}

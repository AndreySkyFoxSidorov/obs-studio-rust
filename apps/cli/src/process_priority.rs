//! Explicit CLI command uses the same host actor/readback contract as desktop.
//! CLI may await bounded receipts; no native call runs on the protocol thread.
use super::*;
use skylive_host::process_priority::{Owner,Key};
#[derive(Default)]pub(super) struct Runtime{owner:Option<Owner>,pending:Option<(Key,Command)>,publication_error:Option<String>}
impl Runtime{
    pub fn advanced_pending(&self)->bool{self.pending.as_ref().is_some_and(|(_,c)|matches!(c,Command::ConfigureAdvancedSettings{..}))}
    pub fn invalidate(&mut self){if self.pending.is_some(){if let Some(owner)=&mut self.owner{if let Err(error)=owner.invalidate(){eprintln!("process priority scope revocation: {error}");owner.close();}}}}
    pub fn poll(&mut self,session:&mut Session,camera:&mut CameraOwner,controller:&mut Controller)->Result<Option<Value>,CliError>{
        let Some(owner)=&mut self.owner else{return Ok(None);};owner.observe(session).map_err(CliError::from)?;
        for _ in 0..2{let Some(receipt)=owner.poll()else{break;};if !self.pending.as_ref().is_some_and(|(key,_)|*key==receipt.key){if receipt.written{eprintln!("superseded process priority receipt: {:?}",receipt);}continue;}
            let Some((key,command))=self.pending.take()else{continue;};
            if !receipt.verified()||!owner.accepts(key,session){let detail=receipt.error.clone().unwrap_or_else(||"Process priority result belongs to an obsolete session/revision.".into());self.publication_error=Some(detail.clone());return Err(CliError{code:"process_priority_failed",phase:Some(if receipt.written{"after_native_write"}else{"before_native_write"}),detail});}
            let result=(||->Result<_,CliError>{camera.before_session_change()?;controller.before_change(session)?;session.apply_at(Some(key.revision),command).map_err(CliError::from)})();
            match result{Ok(change)=>{self.publication_error=None;return Ok(Some(json!({"change":change,"process_priority_receipt":receipt})));},Err(error)=>{let detail=format!("Windows confirmed priority, but preference publication failed: {}",error.detail);self.publication_error=Some(detail.clone());return Err(CliError{code:"process_priority_publication",phase:Some("after_native_readback"),detail});}}
        }
        if owner.is_closed()&&self.pending.is_some(){self.pending=None;let detail=owner.status().failure.unwrap_or_else(||"Process priority owner closed before publication.".into());self.publication_error=Some(detail.clone());return Err(CliError{code:"process_priority_closed",phase:Some("native_retired"),detail});}
        Ok(None)
    }
    pub fn apply(&mut self,session:&mut Session,camera:&mut CameraOwner,controller:&mut Controller,expected:Option<u64>,priority:skylive_core::ProcessPriority)->Result<Value,CliError>{self.apply_command(session,camera,controller,expected,Command::SetProcessPriority{priority})}
    pub fn apply_command(&mut self,session:&mut Session,camera:&mut CameraOwner,controller:&mut Controller,expected:Option<u64>,command:Command)->Result<Value,CliError>{
        if let Command::ConfigureAdvancedSettings{profile,reconnect,..}=&command{if *profile!=session.snapshot().profiles().selected_id(){return Err("The profile changed. Reopen Advanced settings before applying.".into());}reconnect.validate()?;}
        let priority=match &command{Command::SetProcessPriority{priority}|Command::ConfigureAdvancedSettings{priority,..}=>*priority,_=>return Err("Expected process priority/Advanced command".into())};
        if !skylive_host::process_priority::available(){return Err(CliError{code:"process_priority_unavailable",phase:Some("before_native_write"),detail:"Windows process priority is unavailable on this platform.".into()});}
        let revision=session.snapshot().revision();if expected.is_some_and(|value|value!=revision){return Err("process priority request belongs to an obsolete revision".into());}
        if self.owner.as_ref().is_some_and(Owner::is_closed){self.owner=None;}
        if self.owner.is_none(){self.owner=Some(Owner::new(Arc::new(||{}))?);}
        let owner=self.owner.as_mut().ok_or("process priority owner missing")?;let submission=owner.request(session,revision,priority).map_err(CliError::from)?;self.pending=Some((submission.key,command));
        let until=std::time::Instant::now()+std::time::Duration::from_secs(5);
        loop{if let Some(value)=self.poll(session,camera,controller)?{return Ok(value);}if std::time::Instant::now()>=until{return Err(CliError{code:"process_priority_pending",phase:Some("native_pending"),detail:format!("process priority request {:?} has no receipt yet; its owner remains responsible through close/restore",submission.key)});}std::thread::sleep(std::time::Duration::from_millis(4));}
    }
    pub fn status(&mut self)->Value{json!({"pending":self.pending.is_some(),"publication_error":self.publication_error,"status":self.owner.as_mut().map(Owner::status),"capabilities":skylive_host::process_priority::capabilities()})}
    pub fn close_wait(&mut self)->Result<Value,CliError>{
        self.pending=None;let Some(owner)=&mut self.owner else{return Ok(json!({"closed":true,"native_owner_created":false}));};owner.close();
        let until=std::time::Instant::now()+std::time::Duration::from_secs(5);
        while !owner.is_closed(){let _=owner.poll();if std::time::Instant::now()>=until{return Err(CliError{code:"process_priority_retiring",phase:Some("restoring"),detail:"process priority owner is still restoring; no native retirement is claimed".into()});}std::thread::sleep(std::time::Duration::from_millis(4));}
        Ok(json!(owner.status()))
    }
}
impl Drop for Runtime{fn drop(&mut self){if let Err(error)=self.close_wait(){eprintln!("process priority close {}: {}",error.code,error.detail);}}}

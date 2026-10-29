//! Bounded physical hold receipts for the existing audio actor. No simulated
//! input and no authored mute mutation.
use crate::global_hotkeys::{Callback,CallbackDecision,Fired};
use skylive_core::{HotkeyAction,Id,SourceAudioAction,SourceDefinitionEpoch};
use std::{collections::VecDeque,sync::mpsc,time::Instant};

pub(super) const CAPACITY:usize=128;
pub(super) struct Message {pub event:Fired,pub epoch:SourceDefinitionEpoch,pub settings:skylive_core::PushAudioSettings}
struct Held {session:u64,source:Id,epoch:SourceDefinitionEpoch,action:SourceAudioAction,lease:Callback,observed:Instant,pressed:bool,deadline:Option<Instant>,settings:skylive_core::PushAudioSettings,revoked:bool}
pub(super) struct Holds {rows:Vec<Held>,pending:VecDeque<Message>}
impl Holds {
    pub(super) fn new()->Self{Self{rows:Vec::new(),pending:VecDeque::new()}}
    /// Take real observed edges in order. Pending callbacks are left at the
    /// front until their real publication gate accepts/rejects them.
    pub(super) fn receive(&mut self,rx:&mpsc::Receiver<Message>)->Result<(),&'static str>{
        while self.pending.len()<CAPACITY {match rx.try_recv(){Ok(value)=>{if self.pending.try_reserve(1).is_err(){value.event.callback().revoke_scope();return Err("Cannot allocate push audio queue.");}self.pending.push_back(value)},Err(mpsc::TryRecvError::Empty)=>break,Err(mpsc::TryRecvError::Disconnected)=>break}}
        Ok(())
    }
    pub(super) fn process(&mut self,mut edge:impl FnMut(u64,Id,SourceDefinitionEpoch,SourceAudioAction,bool,Instant)->Result<(),String>)->Result<(),String>{
        loop {
            let Some(message)=self.pending.front() else{break;};let callback=message.event.callback();
            match callback.decision(){CallbackDecision::Pending=>break,CallbackDecision::Accepted(_)=>{},_=>{self.pending.pop_front();continue;}}
            let message=self.pending.pop_front().ok_or("Push callback disappeared.")?;
            if !callback.is_current(){continue;}
            let HotkeyAction::SourceAudio{source,operation}=message.event.action else{return Err("Push callback has the wrong action.".into());};
            if !matches!(operation,SourceAudioAction::PushToMute|SourceAudioAction::PushToTalk){return Err("Push callback has the wrong operation.".into());}
            let session=message.event.scope.session;
            if message.event.pressed {
                let existing=self.rows.iter().position(|hold|hold.session==session&&hold.source==source&&hold.epoch==message.epoch&&hold.action==operation);
                if existing.is_none()&&self.rows.len()>=CAPACITY{callback.revoke_scope();return Err("Push hold limit exceeded.".into());}
                if existing.is_none()&&self.rows.try_reserve(1).is_err(){callback.revoke_scope();return Err("Cannot allocate push audio hold.".into());}
                if let Err(error)=edge(session,source,message.epoch,operation,true,message.event.observed){callback.revoke_scope();return Err(error);}
                let old_deadline=existing.and_then(|index|self.rows[index].deadline);
                let hold=Held{session,source,epoch:message.epoch,action:operation,lease:callback,observed:message.event.observed,pressed:true,deadline:old_deadline,settings:message.settings,revoked:false};
                if let Some(index)=existing{self.rows[index]=hold;}else{self.rows.push(hold);}
            }else{
                let Some(index)=self.rows.iter().position(|hold|hold.session==session&&hold.source==source&&hold.epoch==message.epoch&&hold.action==operation)else{continue;};
                let (enabled,delay)=match operation{SourceAudioAction::PushToMute=>(message.settings.mute_enabled(),message.settings.mute_delay_ms()),SourceAudioAction::PushToTalk=>(message.settings.talk_enabled(),message.settings.talk_delay_ms()),_=>unreachable!()};
                let deadline=if enabled{match message.event.observed.checked_add(std::time::Duration::from_millis(u64::from(delay))){Some(value)=>Some(value),None=>{callback.revoke_scope();return Err("Push audio deadline overflow.".into());}}}else{None};
                if let Err(error)=edge(session,source,message.epoch,operation,false,message.event.observed){callback.revoke_scope();return Err(error);}
                // Keep the press lease through the release tail. A later scope
                // revocation must close even a delayed Push-to-talk tail.
                self.rows[index].pressed=false;
                self.rows[index].deadline=match (self.rows[index].deadline,deadline){(Some(a),Some(b))=>Some(a.max(b)),(a,b)=>a.or(b)};
            }
        }
        Ok(())
    }
    /// A retired source/registration never carries a pressed key into the next
    /// owner. Callback scope liveness is observed, not a synthetic key event.
    pub(super) fn revoke(&mut self,now:Instant,mut edge:impl FnMut(u64,Id,SourceDefinitionEpoch,SourceAudioAction,Instant)->Result<(),String>)->Result<(),String>{
        let mut index=0;while index<self.rows.len(){
            if self.rows[index].revoked {if self.rows[index].deadline.is_none_or(|until|until<=now){self.rows.swap_remove(index);}else{index+=1;}continue;}
            if self.rows[index].lease.is_current(){index+=1;continue;}
            let hold=&mut self.rows[index];
            edge(hold.session,hold.source,hold.epoch,hold.action,now)?;
            if hold.action==SourceAudioAction::PushToMute {
                if hold.pressed&&hold.settings.mute_enabled(){
                    let until=now.checked_add(std::time::Duration::from_millis(u64::from(hold.settings.mute_delay_ms()))).ok_or("Push audio deadline overflow.")?;
                    hold.deadline=Some(hold.deadline.map_or(until,|old|old.max(until)));
                }
                hold.pressed=false;hold.revoked=true;
                if hold.deadline.is_some_and(|until|until>now){index+=1;continue;}
            }
            self.rows.swap_remove(index);
        }Ok(())
    }
    pub(super) fn held(&self,session:u64,source:Id,action:SourceAudioAction)->bool{
        self.rows.iter().any(|hold|hold.session==session&&hold.source==source&&hold.action==action&&hold.pressed&&hold.lease.is_current())
    }
    pub(super) fn replay(&self,mut edge:impl FnMut(u64,Id,SourceDefinitionEpoch,SourceAudioAction,Option<Instant>,Instant)->Result<(),String>)->Result<(),String>{
        let now=Instant::now();
        for hold in &self.rows {if (hold.lease.is_current()||hold.revoked&&hold.action==SourceAudioAction::PushToMute)&&(hold.pressed||hold.deadline.is_some_and(|deadline|deadline>now)){if let Err(error)=edge(hold.session,hold.source,hold.epoch,hold.action,if hold.pressed{None}else{hold.deadline},hold.observed){hold.lease.revoke_scope();return Err(error);}}}Ok(())
    }
}

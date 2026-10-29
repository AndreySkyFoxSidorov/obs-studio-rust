//! Owned bounded press timeline. No OS/input sampling or UI dependencies.
use super::*;
use std::{collections::VecDeque,sync::atomic::AtomicU8,time::{Duration,Instant}};
use skylive_core::{Id,SourcePlaybackAction as Source,SourceAudioAction as Audio};
const WAITING:u8=0;const FIRST:u8=1;const SECOND:u8=2;const REJECTED:u8=3;const RETIRED:u8=4;
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum CallbackDecision {Pending,Accepted(HotkeyAction),Rejected,Retired}
/// Semantic callback acceptance, independent of eventual encoder/decoder success.
/// Only the holder of the actual owner callback decides an allowed side.
#[derive(Clone,Debug)]
pub struct Callback {id:u64,allowed:[Option<HotkeyAction>;2],state:Arc<AtomicU8>,live:Arc<AtomicBool>}
impl Callback {
    fn new(id:u64,allowed:[Option<HotkeyAction>;2],live:Arc<AtomicBool>)->Self{Self{id,allowed,state:Arc::new(AtomicU8::new(WAITING)),live}}
    pub fn identity(&self)->u64{self.id}
    pub fn decision(&self)->CallbackDecision{
        // Retirement revokes future acceptance; it cannot roll back a callback
        // that was already accepted and may have begun backend work.
        match self.state.load(Ordering::Acquire){FIRST=>self.allowed[0].map_or(CallbackDecision::Retired,CallbackDecision::Accepted),SECOND=>self.allowed[1].map_or(CallbackDecision::Retired,CallbackDecision::Accepted),REJECTED=>CallbackDecision::Rejected,WAITING if self.live.load(Ordering::Acquire)=>CallbackDecision::Pending,_=>CallbackDecision::Retired}
    }
    pub fn is_current(&self)->bool{self.live.load(Ordering::Acquire)}
    pub(crate) fn revoke_scope(&self){self.live.store(false,Ordering::Release);}
    pub fn pending(&self)->bool{self.live.load(Ordering::Acquire)&&self.state.load(Ordering::Acquire)==WAITING}
    pub fn accept(&self)->bool{self.allowed[0].is_some_and(|action|self.accept_action(action))}
    pub fn accept_action(&self,action:HotkeyAction)->bool{
        let state=if self.allowed[0]==Some(action){FIRST}else if self.allowed[1]==Some(action){SECOND}else{return false;};
        if !self.live.load(Ordering::Acquire)||self.state.compare_exchange(WAITING,state,Ordering::AcqRel,Ordering::Acquire).is_err(){return false;}
        if !self.live.load(Ordering::Acquire){let _=self.state.compare_exchange(state,RETIRED,Ordering::AcqRel,Ordering::Acquire);return false;}true
    }
    pub fn reject(&self){let _=self.state.compare_exchange(WAITING,REJECTED,Ordering::AcqRel,Ordering::Acquire);}
    fn retire(&self){let _=self.state.compare_exchange(WAITING,RETIRED,Ordering::AcqRel,Ordering::Acquire);}
    fn accepted(&self)->Option<HotkeyAction>{if !self.live.load(Ordering::Acquire){return None;}match self.state.load(Ordering::Acquire){FIRST=>self.allowed[0],SECOND=>self.allowed[1],_=>None}}
}
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
enum Pair {Media(Id),Audio(Id),Recording,RecordingPause,Streaming,Replay,VirtualCamera}
fn pair(action:HotkeyAction)->Option<(Pair,bool)>{use HotkeyAction::*;match action{
    SourcePlayback{source,operation:Source::Play}=>Some((Pair::Media(source),false)),SourcePlayback{source,operation:Source::Pause}=>Some((Pair::Media(source),true)),
    SourceAudio{source,operation:Audio::Mute}=>Some((Pair::Audio(source),false)),SourceAudio{source,operation:Audio::Unmute}=>Some((Pair::Audio(source),true)),
    StartRecording=>Some((Pair::Recording,false)),StopRecording=>Some((Pair::Recording,true)),PauseRecording=>Some((Pair::RecordingPause,false)),ResumeRecording=>Some((Pair::RecordingPause,true)),
    StartStreaming=>Some((Pair::Streaming,false)),StopStreaming=>Some((Pair::Streaming,true)),StartReplayBuffer=>Some((Pair::Replay,false)),StopReplayBuffer=>Some((Pair::Replay,true)),StartVirtualCamera=>Some((Pair::VirtualCamera,false)),StopVirtualCamera=>Some((Pair::VirtualCamera,true)),_=>None}}
struct Action {action:HotkeyAction,held:u16,accepted:bool}
struct Route {canonical:HotkeyAction,members:[Option<usize>;2],physical:bool,source_epoch:Option<skylive_core::SourceDefinitionEpoch>,push:Option<skylive_core::PushAudioSettings>}
struct PairState {pair:Pair,latched:Option<bool>}
struct Edge {binding:usize,pressed:bool,id:u64,observed:Instant}
struct Pending {event:Fired,started:Instant}
pub(super) struct Timeline {scope:Scope,live:Arc<AtomicBool>,routes:Vec<Route>,actions:Vec<Action>,pairs:Vec<PairState>,edges:VecDeque<Edge>,pending:Option<Pending>,next:u64}
impl Timeline {
    pub(super) fn new(scope:Scope,bindings:&Bindings,live:Arc<AtomicBool>)->Result<Self,String>{
        let mut actions:Vec<Action>=Vec::new();let mut routes=Vec::new();let mut pairs=Vec::<PairState>::new();
        actions.try_reserve_exact(2*bindings::MAX_REGISTRATIONS).map_err(|_|"Cannot allocate bounded shortcut arbitration state.")?;
        routes.try_reserve_exact(bindings::MAX_REGISTRATIONS).map_err(|_|"Cannot allocate bounded shortcut arbitration state.")?;
        pairs.try_reserve_exact(2*bindings::MAX_REGISTRATIONS).map_err(|_|"Cannot allocate bounded shortcut arbitration state.")?;
        let mut edges=VecDeque::new();edges.try_reserve_exact(4*bindings::MAX_REGISTRATIONS).map_err(|_|"Cannot allocate bounded shortcut arbitration state.")?;
        for binding in bindings.bindings(){
            let members=match binding.action {HotkeyAction::SourcePlayback{source,operation:Source::PlayPause}if bindings.media_source(source)=>[Some(HotkeyAction::SourcePlayback{source,operation:Source::Play}),Some(HotkeyAction::SourcePlayback{source,operation:Source::Pause})],HotkeyAction::SourceAudio{source,operation:Audio::ToggleMute}=>[Some(HotkeyAction::SourceAudio{source,operation:Audio::Mute}),Some(HotkeyAction::SourceAudio{source,operation:Audio::Unmute})],action=>[Some(action),None]};
            let mut indices=[None,None];for(index,action)in members.into_iter().enumerate(){if let Some(action)=action{
                let slot=if let Some(index)=actions.iter().position(|value|value.action==action){index}else{actions.push(Action{action,held:0,accepted:false});actions.len()-1};indices[index]=Some(slot);
                if let Some((pair,_))=pair(action){if !pairs.iter().any(|state|state.pair==pair){pairs.push(PairState{pair,latched:None});}}
            }}let (source_epoch,push)=match binding.action{HotkeyAction::SourceAudio{source,..}=>(bindings.source_epoch(source),bindings.push_settings(source)),_=>(None,None)};routes.push(Route{canonical:binding.action,members:indices,physical:false,source_epoch,push});
        }
        if routes.len()>bindings::MAX_REGISTRATIONS||actions.len()>2*bindings::MAX_REGISTRATIONS{return Err("Shortcut arbitration route limit exceeded.".into());}
        Ok(Self{scope,live,routes,actions,pairs,edges,pending:None,next:0})
    }
    pub(super) fn waiting(&self)->bool{self.pending.is_some()}
    pub(super) fn sample(&mut self,binding:usize,pressed:bool)->Result<(),String>{
        let route=self.routes.get_mut(binding).ok_or("Shortcut sample has an invalid binding identity.")?;
        if route.physical==pressed{return Ok(());}
        if self.edges.len()>=4*bindings::MAX_REGISTRATIONS{return Err("Shortcut press/release buffer is full; input stopped. Release the configured keys and reopen shortcut settings.".into());}
        self.next=self.next.checked_add(1).ok_or("Shortcut press identity exhausted. Restart SkyLive.")?;
        route.physical=pressed;self.edges.push_back(Edge{binding,pressed,id:self.next,observed:Instant::now()});Ok(())
    }
    pub(super) fn advance(&mut self,events:&mpsc::SyncSender<Fired>,wake:&dyn Fn(),push:Option<&crate::audio_capture::PushAudioSink>)->Result<(),String>{
        if !self.live.load(Ordering::Acquire){self.retire();return Ok(());}
        if let Some(pending)=&self.pending {
            if pending.event.callback.pending(){if pending.started.elapsed()>Duration::from_secs(5){return Err("Shortcut callback did not acknowledge within five seconds; input stopped. Release the configured keys and reopen shortcut settings.".into());}return Ok(());}
            if let Some(action)=pending.event.callback.accepted(){if let Some((group,side))=pair(action){let state=self.pairs.iter_mut().find(|state|state.pair==group).ok_or("Shortcut pair identity is absent.")?;state.latched=Some(side);}if matches!(action,HotkeyAction::SourceAudio{operation:Audio::PushToMute|Audio::PushToTalk,..}){if let Some(state)=self.actions.iter_mut().find(|state|state.action==action){state.accepted=pending.event.pressed;}}}
            self.pending=None;
        }
        for _ in 0..64 {
            let Some(edge)=self.edges.pop_front()else{return Ok(());};let route=&self.routes[edge.binding];let canonical=route.canonical;let members=route.members;
            let mut allowed=[None,None];let mut count=0usize;
            for index in members.into_iter().flatten(){
                let state=&mut self.actions[index];
                if !edge.pressed {
                    state.held=state.held.checked_sub(1).ok_or("Shortcut action release count underflow.")?;
                    if state.held==0 {if let Some((group,side))=pair(state.action){if let Some(pair)=self.pairs.iter_mut().find(|state|state.pair==group){if pair.latched==Some(side){pair.latched=None;}}}if state.accepted&&matches!(state.action,HotkeyAction::SourceAudio{operation:Audio::PushToMute|Audio::PushToTalk,..}){allowed[count]=Some(state.action);count+=1;}}
                    continue;
                }
                let first=state.held==0;state.held=state.held.checked_add(1).ok_or("Shortcut action press count overflow.")?;
                if !first{continue;}
                let blocked=pair(state.action).is_some_and(|(group,side)|self.pairs.iter().any(|state|state.pair==group&&state.latched.is_some_and(|held|held!=side)));
                if !blocked{allowed[count]=Some(state.action);count+=1;}
            }
            if count==0{continue;}
            // A common paired chord is one compound callback. The real media
            // or canonical audio owner chooses its currently allowed side.
            let action=if count==2{canonical}else{allowed[0].expect("admitted callback")};
            let callback=Callback::new(edge.id,allowed,self.live.clone());let event=Fired{scope:self.scope,action,press:edge.id,pressed:edge.pressed,observed:edge.observed,binding:edge.binding,callback};
            if matches!(action,HotkeyAction::SourceAudio{operation:Audio::PushToMute|Audio::PushToTalk,..}){
                let epoch=route.source_epoch.ok_or("Push audio source epoch is absent.")?;
                let settings=route.push.ok_or("Push audio source settings are absent.")?;
                let sink=push.ok_or("Push audio transport is unavailable.")?;
                if let Err(error)=sink.submit(event.clone(),epoch,settings){self.live.store(false,Ordering::Release);return Err(error);}
                if !event.callback.accept(){self.live.store(false,Ordering::Release);return Err("Push audio callback was retired before acceptance.".into());}
            }else{
                events.try_send(event.clone()).map_err(|error|match error{mpsc::TrySendError::Full(_)=>"Shortcut callback queue is full; input stopped instead of dropping actions.",mpsc::TrySendError::Disconnected(_)=>"Shortcut callback receiver is closed."}.to_owned())?;
            }
            self.pending=Some(Pending{event,started:Instant::now()});wake();return Ok(());
        }Ok(())
    }
    pub(super) fn retire(&mut self){self.live.store(false,Ordering::Release);if let Some(pending)=self.pending.take(){pending.event.callback.retire();}self.edges.clear();for action in &mut self.actions{action.held=0;action.accepted=false;}for pair in &mut self.pairs{pair.latched=None;}}
}
impl Drop for Timeline {fn drop(&mut self){self.retire();}}

//! Source-local output-time held-action history. Never persists physical state.
use skylive_core::{Id,PushAudioSettings};

#[derive(Clone,Copy,PartialEq,Eq)]
struct State {
    settings:PushAudioSettings,
    mute_held:bool,
    talk_held:bool,
    mute_until:u64,
    talk_until:u64,
}
impl State {
    fn silent(self,frame:u64)->bool {
        let p=self.settings;
        (p.mute_enabled()&&(self.mute_held||frame<self.mute_until))
            ||(p.talk_enabled()&&!(self.talk_held||frame<self.talk_until))
    }
}
#[derive(Clone,Copy)]
struct Event {frame:u64,state:State}
#[derive(Clone,Copy)]
pub(super) enum PushAction {Mute,Talk}
pub(super) struct PushGate {rate:u32,events:Vec<Event>,retired:u64}
const MAX_EVENTS:usize=128;
impl PushGate {
    fn copy(&self)->Result<Self,&'static str>{let mut events=Vec::new();events.try_reserve_exact(MAX_EVENTS).map_err(|_|"Push audio history allocation failed.")?;events.extend_from_slice(&self.events);Ok(Self{rate:self.rate,events,retired:self.retired})}
    pub(super) fn new(rate:u32,settings:PushAudioSettings,frame:u64)->Result<Self,&'static str>{
        if rate==0{return Err("Push audio sample rate is zero.");}
        let mut events=Vec::new();events.try_reserve_exact(MAX_EVENTS).map_err(|_|"Push audio history allocation failed.")?;
        events.push(Event{frame,state:State{settings,mute_held:false,talk_held:false,mute_until:0,talk_until:0}});
        Ok(Self{rate,events,retired:frame})
    }
    fn transition(&mut self,frame:u64,mut change:impl FnMut(&mut State)->Result<(),&'static str>)->Result<bool,&'static str>{
        let last=*self.events.last().ok_or("Push audio history is empty.")?;
        let frame=frame.max(last.frame).max(self.retired);
        let mut state=last.state;change(&mut state)?;
        if state==last.state{return Ok(false);}
        if frame==last.frame{self.events.last_mut().ok_or("Push audio history is empty.")?.state=state;return Ok(true);}
        if self.events.len()==MAX_EVENTS{return Err("Push audio history is full.");}
        self.events.push(Event{frame,state});Ok(true)
    }
    pub(super) fn configure(&mut self,frame:u64,settings:PushAudioSettings)->Result<bool,&'static str>{
        self.transition(frame,|state|{state.settings=settings;Ok(())})
    }
    pub(super) fn edge(&mut self,frame:u64,action:PushAction,pressed:bool)->Result<bool,&'static str>{
        let rate=self.rate;
        let frame=frame.max(self.retired).max(self.events.last().map_or(frame,|event|event.frame));
        self.transition(frame,|state|{
            let (held,until,enabled,delay)=match action {
                PushAction::Mute=>(&mut state.mute_held,&mut state.mute_until,state.settings.mute_enabled(),state.settings.mute_delay_ms()),
                PushAction::Talk=>(&mut state.talk_held,&mut state.talk_until,state.settings.talk_enabled(),state.settings.talk_delay_ms()),
            };
            if *held==pressed{return Ok(());}
            if !pressed&&enabled {
                let frames=(u128::from(delay)*u128::from(rate)).div_ceil(1000);
                let frames=u64::try_from(frames).map_err(|_|"Push audio delay overflow.")?;
                *until=frame.checked_add(frames).ok_or("Push audio deadline overflow.")?;
            }
            *held=pressed;Ok(())
        })
    }
    pub(super) fn silent(&self,frame:u64)->bool {
        let end=self.events.partition_point(|event|event.frame<=frame);
        self.events.get(end.saturating_sub(1)).is_some_and(|event|event.state.silent(frame))
    }
    /// Keep the preceding state for reads at the retained frame. Its deadline
    /// remains absolute and therefore still applies after old events retire.
    pub(super) fn retire_before(&mut self,frame:u64)->Result<(),&'static str>{
        if frame<self.retired{return Err("Push audio retirement moved backwards.");}
        let end=self.events.partition_point(|event|event.frame<=frame);
        if end>1{self.events.drain(..end-1);}
        self.retired=frame;Ok(())
    }
    pub(super) fn revoke(&mut self,frame:u64,action:PushAction)->Result<bool,&'static str>{
        let frame=frame.max(self.retired).max(self.events.last().map_or(frame,|event|event.frame));
        self.transition(frame,|state|{match action{PushAction::Mute=>{state.mute_held=false;state.mute_until=frame;},PushAction::Talk=>{state.talk_held=false;state.talk_until=frame;}}Ok(())})
    }
    pub(super) fn restore_tail(&mut self,frame:u64,action:PushAction,until:u64)->Result<bool,&'static str>{
        self.transition(frame,|state|{match action{PushAction::Mute if !state.mute_held=>state.mute_until=until,PushAction::Talk if !state.talk_held=>state.talk_until=until,_=>{}}Ok(())})
    }
}

/// Fixed reservation covers old and candidate metadata simultaneously.
pub(super) const RESERVATION:usize=2*1024*1024;
#[derive(Default)]
pub(super) struct Bank {rows:Vec<(Id,PushGate)>}
impl Bank {
    pub(super) fn copy(&self)->Result<Self,&'static str>{let mut rows=Vec::new();rows.try_reserve_exact(self.rows.len()).map_err(|_|"Push audio source allocation failed.")?;for(id,gate)in &self.rows{rows.push((*id,gate.copy()?));}Ok(Self{rows})}
    fn index(&self,id:Id)->Result<usize,usize>{self.rows.binary_search_by_key(&id,|(source,_)|*source)}
    pub(super) fn settings(&self,id:Id)->Option<PushAudioSettings>{self.index(id).ok().and_then(|index|self.rows[index].1.events.last().map(|event|event.state.settings))}
    pub(super) fn configure(&mut self,id:Id,rate:u32,frame:u64,settings:PushAudioSettings)->Result<(),&'static str>{
        let index=match self.index(id){Ok(index)=>{self.rows[index].1.configure(frame,settings)?;return Ok(());},Err(index)=>index};
        if self.rows.len()>=crate::audio::MAX_AUDIO_BLOCK_SOURCES{return Err("Too many retained push audio sources.");}
        self.rows.try_reserve_exact(1).map_err(|_|"Push audio source allocation failed.")?;
        self.rows.insert(index,(id,PushGate::new(rate,settings,frame)?));Ok(())
    }
    pub(super) fn edge(&mut self,id:Id,frame:u64,action:PushAction,pressed:bool)->Result<bool,&'static str>{
        if let Ok(index)=self.index(id){return self.rows[index].1.edge(frame,action,pressed);}Ok(false)
    }
    pub(super) fn revoke(&mut self,id:Id,frame:u64,action:PushAction)->Result<bool,&'static str>{
        if let Ok(index)=self.index(id){return self.rows[index].1.revoke(frame,action);}Ok(false)
    }
    pub(super) fn restore_tail(&mut self,id:Id,frame:u64,action:PushAction,until:u64)->Result<bool,&'static str>{
        if let Ok(index)=self.index(id){return self.rows[index].1.restore_tail(frame,action,until);}Ok(false)
    }
    pub(super) fn silent(&self,id:Id,frame:u64)->bool{self.index(id).is_ok_and(|index|self.rows[index].1.silent(frame))}
    pub(super) fn retire(&mut self,frame:u64,retain:impl Fn(Id)->bool)->Result<(),&'static str>{
        for (_,gate) in &mut self.rows {gate.retire_before(frame)?;}
        self.rows.retain(|(source,_)|retain(*source));Ok(())
    }
}

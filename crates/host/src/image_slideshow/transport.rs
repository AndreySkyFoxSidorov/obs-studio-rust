//! Runtime-only stock slideshow media controls. No assets/IO/workers are owned here.
use super::{Sample,Order,random};
use skylive_core::{Id,ImageSlideShow,SlideShowMode};
use serde::{Deserialize,Serialize};
#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="snake_case")]
pub enum Control{Play,Pause,Restart,Stop,Next,Previous}
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub struct Target{pub session:u64,pub revision:u64,pub source:Id}
#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize)]
#[serde(rename_all="snake_case")]
pub enum State{Playing,Paused,Stopped,Ended}
impl State{pub fn label(self)->&'static str{match self{Self::Playing=>"Playing",Self::Paused=>"Paused",Self::Stopped=>"Stopped",Self::Ended=>"Ended"}}}
#[derive(Clone,Copy,Debug,Serialize)]
pub struct Status{pub state:State,pub current_index:u32,pub total:u32,pub manual:bool,pub transitioning:bool,pub random_seed:Option<u64>,pub random_generation:Option<u64>,pub random_draws:Option<u64>,pub random_compatibility:Option<skylive_core::SlideShowRandomCompatibility>}
#[derive(Clone,Copy,Debug,Serialize)]
pub struct Receipt{pub applied:bool,pub transition_busy:bool,pub status:Status,pub epoch:u64,pub discarded_elapsed:bool}
#[derive(Clone,Copy,Debug)]
struct Visual{index:usize,age:u64,at:u64}
impl Visual{fn age(self,now:u64)->u64{self.age.saturating_add(now.saturating_sub(self.at))}}
#[derive(Clone,Copy,Debug)]
pub(crate) struct Runtime{
    random:Option<random::Run>,
    cursor:usize,phase:u64,last_logical:u64,manual:bool,paused:bool,stopped:bool,state:State,
    current:Option<Visual>,outgoing:Option<Visual>,transition_at:u64,in_transition:bool,
}
impl Runtime{
    pub(crate) fn hotkey_paused(&self)->bool{self.paused}
    pub(crate) fn hotkey_manual(&self)->bool{self.manual}
    pub(crate) fn from_sample(settings:&ImageSlideShow,order:&Order,now:u64,logical:u64,s:Sample)->Self{
        let manual=settings.mode()==SlideShowMode::Manual;
        let visual=|index,age|Visual{index,age,at:now};
        let in_transition=s.next.is_some()||s.to_transparent||s.from_transparent;
        let current=if s.hidden||s.to_transparent{None}else{Some(visual(s.next.unwrap_or(s.current),if s.next.is_some(){s.next_image_elapsed_ns}else{s.image_elapsed_ns}))};
        let outgoing=if s.next.is_some()||s.to_transparent{Some(visual(s.current,s.image_elapsed_ns))}else{None};
        Self{random:None,cursor:if !settings.looping()&&logical/settings.period_ns()>=settings.images().len()as u64{settings.images().len()-1}else{((logical/settings.period_ns())%settings.images().len()as u64)as usize},phase:logical%settings.period_ns(),last_logical:logical,manual,paused:false,stopped:false,state:if manual{State::Paused}else if s.hidden||s.to_transparent{State::Ended}else{State::Playing},current,outgoing,transition_at:now.saturating_sub(s.fade_ns),in_transition}
    }
    fn busy(&self,settings:&ImageSlideShow,now:u64)->bool{self.in_transition&&now.saturating_sub(self.transition_at)<settings.transition_duration_ns()}
    fn start(&mut self,to:Option<usize>,at:u64){
        // obs_transition_start completes an interrupted transition to its old
        // destination first. That destination is always self.current here.
        if self.current.map(|v|v.index)==to{self.outgoing=None;self.in_transition=false;return;}
        self.outgoing=self.current;self.current=to.map(|index|Visual{index,age:0,at});
        self.transition_at=at;self.in_transition=true;
    }
    pub(crate) fn random_start(settings:&ImageSlideShow,order:&Order,seed:u64,now:u64,logical:u64)->Result<Self,&'static str>{
        let sample=super::sample_ordered(settings,0,0,order);let mut runtime=Self::from_sample(settings,order,now,logical,sample);let run=random::Run::new(seed,order.len(),settings.random_compatibility())?;
        runtime.cursor=run.current;runtime.current=Some(Visual{index:run.current,age:0,at:now});runtime.outgoing=None;runtime.random=Some(run);runtime.transition_at=now;runtime.in_transition=settings.behavior()!=skylive_core::SlideShowBehavior::StopRestart&&settings.transition_duration_ns()>0;runtime.phase=0;Ok(runtime)
    }
    pub(crate) fn discard_elapsed(&mut self,logical:u64){self.last_logical=logical;}
    pub(crate) fn random_work(&self,logical:u64)->u64{if self.random.is_none()||self.manual||self.paused||self.stopped{return 0;}let delta=logical.saturating_sub(self.last_logical);(u128::from(self.phase)+u128::from(delta)).min(u128::from(u64::MAX))as u64}
    pub(crate) fn due_random_advances(&self,settings:&ImageSlideShow,logical:u64)->u64{let total=self.random_work(logical);total/settings.period_ns()}
    pub(crate) fn visibility_restart(&mut self,settings:&ImageSlideShow,order:&Order,now:u64,logical:u64)->Result<(),&'static str>{
        self.cursor=if let Some(random)=&mut self.random{random.rebuild(true)?;random.current}else{0};self.phase=0;self.last_logical=logical;self.stopped=false;
        self.current=Some(Visual{index:if self.random.is_some(){self.cursor}else{order[0]},age:0,at:now});self.outgoing=None;self.in_transition=false;self.state=State::Playing;
        Ok(())
    }
    pub(crate) fn tick(&mut self,settings:&ImageSlideShow,order:&Order,logical:u64,now:u64)->Result<(),&'static str>{
        if self.due_random_advances(settings,logical)>random::MAX_SOURCE_ADVANCES{return Err(random::WORK_LIMIT);}
        let mut candidate=*self;candidate.tick_inner(settings,order,logical,now)?;*self=candidate;Ok(())
    }
    fn tick_inner(&mut self,settings:&ImageSlideShow,order:&Order,logical:u64,now:u64)->Result<(),&'static str>{
        let delta=logical.saturating_sub(self.last_logical);self.last_logical=logical;
        if self.manual||self.paused||self.stopped{return Ok(());}
        let period=settings.period_ns();let total=u128::from(self.phase)+u128::from(delta);
        let mut steps=(total/u128::from(period))as u64;let remainder=(total%u128::from(period))as u64;
        if steps==0{self.phase=remainder;return Ok(());}
        // Stock next ignores the first boundary if a manual transition is still
        // active there. Every later boundary is at least one full period apart.
        let first=now.saturating_sub(delta.saturating_sub(period-self.phase));
        if self.busy(settings,first){steps-=1;}
        self.phase=remainder;if steps==0{return Ok(());}
        let indices=order;let n=indices.len();
        if self.random.is_some(){
            for step in 0..steps{
                let delay=(u128::from(remainder)+u128::from(steps-1-step)*u128::from(period)).min(u128::from(u64::MAX))as u64;let at=now.saturating_sub(delay);
                let legacy=self.random.as_ref().is_some_and(|run|run.policy()==skylive_core::SlideShowRandomCompatibility::LegacyV1);
                if legacy&&!settings.looping()&&self.cursor==n-1{if settings.hide_when_done(){self.start(None,at);self.state=State::Ended;}break;}
                let next=self.random.as_mut().expect("random branch").advance(true)?;self.cursor=next;self.start(Some(next),at);
            }return Ok(());
        }
        if !settings.looping(){
            let until_end=(n-self.cursor)as u64;
            if steps>=until_end{
                if settings.hide_when_done(){
                    let delay=(u128::from(remainder)+u128::from(steps-until_end)*u128::from(period)).min(u128::from(u64::MAX))as u64;
                    let at=now.saturating_sub(delay);
                    if self.cursor!=n-1{self.current=Some(Visual{index:indices[n-1],age:period,at});}
                    self.cursor=n-1;self.start(None,at);self.state=State::Ended;
                }else if self.cursor!=n-1{
                    let advance=(n-1-self.cursor)as u64;
                    let delay=(u128::from(remainder)+u128::from(steps-advance)*u128::from(period)).min(u128::from(u64::MAX))as u64;
                    let at=now.saturating_sub(delay);self.outgoing=Some(Visual{index:indices[n-2],age:period,at});self.current=Some(Visual{index:indices[n-1],age:0,at});self.transition_at=at;self.in_transition=n>1;self.cursor=n-1;
                }
                return Ok(());
            }
        }
        let old=self.cursor;self.cursor=(self.cursor+(steps%n as u64)as usize)%n;
        let at=now.saturating_sub(remainder);
        if steps==1{self.start(Some(indices[self.cursor]),at);}else{
            self.outgoing=Some(Visual{index:indices[(self.cursor+n-1)%n],age:period,at});self.current=Some(Visual{index:indices[self.cursor],age:0,at});self.transition_at=at;self.in_transition=n>1;
        }
        if n==1||old==self.cursor&&steps==1{self.in_transition=false;}
        Ok(())
    }
    pub(crate) fn apply(&mut self,settings:&ImageSlideShow,order:&Order,op:Control,now:u64)->Result<bool,&'static str>{let mut candidate=*self;let applied=candidate.apply_inner(settings,order,op,now)?;*self=candidate;Ok(applied)}
    fn apply_inner(&mut self,settings:&ImageSlideShow,order:&Order,op:Control,now:u64)->Result<bool,&'static str>{
        if matches!(op,Control::Next|Control::Previous)&&self.busy(settings,now){return Ok(false);}
        let indices=order;
        match op{
            Control::Play|Control::Pause=>{
                if self.stopped{self.stopped=false;self.paused=false;self.start(Some(if self.random.is_some(){self.cursor}else{indices[self.cursor]}),now);}else{self.paused=op==Control::Pause;self.manual=self.paused;}
                self.state=if op==Control::Pause{State::Paused}else{State::Playing};
            },
            Control::Restart=>{self.cursor=if let Some(random)=&mut self.random{random.rebuild(false)?;random.current}else{0};self.phase=0;self.stopped=false;self.paused=false;self.start(Some(if self.random.is_some(){self.cursor}else{indices[0]}),now);self.state=State::Playing;},
            Control::Stop=>{self.cursor=if let Some(random)=&mut self.random{random.rebuild(false)?;random.current}else{0};self.phase=0;self.start(None,now);self.stopped=true;self.paused=false;self.state=State::Stopped;},
            Control::Next=>{self.cursor=if let Some(random)=&mut self.random{random.advance(true)?}else{(self.cursor+1)%indices.len()};self.start(Some(if self.random.is_some(){self.cursor}else{indices[self.cursor]}),now);},
            Control::Previous=>{self.cursor=if let Some(random)=&mut self.random{random.advance(false)?}else if self.cursor==0{indices.len()-1}else{self.cursor-1};self.start(Some(if self.random.is_some(){self.cursor}else{indices[self.cursor]}),now);},
        }Ok(true)
    }
    pub(crate) fn status(&self,settings:&ImageSlideShow,order:&Order,now:u64)->Status{Status{state:self.state,current_index:if self.random.is_some(){self.cursor as u32}else{order[self.cursor]as u32},total:settings.images().len()as u32,manual:self.manual,transitioning:self.busy(settings,now),random_seed:self.random.map(|run|run.seed),random_generation:self.random.map(|run|run.generation),random_draws:self.random.map(|run|run.draws),random_compatibility:self.random.map(|run|run.policy())}}
    pub(crate) fn sample(&self,settings:&ImageSlideShow,now:u64)->Sample{
        let duration=settings.transition_duration_ns();let changing=self.busy(settings,now);
        let (current,next,from,to,hidden,age,next_age)=if changing{match(self.outgoing,self.current){
            (Some(a),Some(b))=>(a.index,Some(b.index),false,false,false,a.age(now),b.age(now)),
            (Some(a),None)=>(a.index,None,false,true,false,a.age(now),0),
            (None,Some(b))=>(b.index,None,true,false,false,b.age(now),0),
            (None,None)=>(0,None,false,false,true,0,0),
        }}else{self.current.map_or((0,None,false,false,true,0,0),|v|(v.index,None,false,false,false,v.age(now),0))};
        let next_after=if changing{Some((duration-now.saturating_sub(self.transition_at)).min(16_666_667))}else if !self.manual&&!self.paused&&!self.stopped{Some(settings.period_ns()-self.phase)}else{None};
        Sample{current,next,image_elapsed_ns:age,next_image_elapsed_ns:next_age,fade_ns:if changing{now.saturating_sub(self.transition_at)}else{0},fade_duration_ns:duration,next_after_ns:next_after,hidden,to_transparent:to,from_transparent:from}
    }
}

//! Configured-chord-only Windows polling on the one owned input worker.
//! No hook, text input, window, OS chord reservation or key logging.
use super::*;
use skylive_core::HotkeyBinding;
use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
struct Configured {key:u32,modifiers:u32}
pub(super) struct Registry {configured:Vec<Configured>,scope:Option<Scope>,timeline:Option<press::Timeline>}
impl Registry {
    pub(super) fn new(_instance:u64)->Self{Self{configured:Vec::with_capacity(bindings::MAX_REGISTRATIONS),scope:None,timeline:None}}
    pub(super) fn count(&self)->usize{self.configured.len()}
    pub(super) fn waiting(&self)->bool{self.timeline.as_ref().is_some_and(press::Timeline::waiting)}
    pub(super) fn scope(&self)->Option<Scope>{self.scope}
    pub(super) fn keys_released(&self,bindings:&Bindings)->bool{
        bindings.bindings().iter().all(|binding|{let(key,mods)=chord(binding);!down(key)&&(!(mods&2!=0)||!down(17))&&(!(mods&1!=0)||!down(18))&&(!(mods&4!=0)||!down(16))})
    }
    pub(super) fn register(&mut self,scope:Scope,bindings:&Bindings,live:Arc<AtomicBool>)->Result<(),String>{
        if !self.configured.is_empty()||self.timeline.is_some(){return Err("Shortcut input is still retiring.".into());}
        let timeline=press::Timeline::new(scope,bindings,live)?;
        for binding in bindings.bindings(){let(key,modifiers)=chord(binding);if key==0{return Err("Configured shortcut has no supported Windows virtual key.".into());}self.configured.push(Configured{key,modifiers});}
        self.scope=Some(scope);self.timeline=Some(timeline);Ok(())
    }
    pub(super) fn retire(&mut self)->Result<(),String>{if let Some(mut timeline)=self.timeline.take(){timeline.retire();}self.configured.clear();self.scope=None;Ok(())}
    pub(super) fn close(&mut self)->Result<(),String>{self.retire()}
    pub(super) fn messages(&mut self,active:bool,events:&mpsc::SyncSender<Fired>,_dropped:&AtomicU64,_closed:&AtomicBool,wake:&dyn Fn(),push:Option<&crate::audio_capture::PushAudioSink>)->Result<(),String>{
        if !active{return Ok(());}let Some(timeline)=&mut self.timeline else{return Err("Shortcut input timeline is absent.".into());};
        timeline.advance(events,wake,push)?;
        // Only declared modifiers and configured non-text virtual key codes are
        // sampled. Extra modifiers do not trigger a different configured chord.
        let modifiers=(if down(17){2}else{0})|(if down(18){1}else{0})|(if down(16){4}else{0});
        let windows=down(91)||down(92);
        for(index,binding)in self.configured.iter().enumerate(){timeline.sample(index,!windows&&modifiers==binding.modifiers&&down(binding.key))?;}
        timeline.advance(events,wake,push)
    }
}
impl Drop for Registry{fn drop(&mut self){let _=self.close();}}
fn down(key:u32)->bool{(unsafe{GetAsyncKeyState(key as i32)}as u16&0x8000)!=0}
fn chord(binding:&HotkeyBinding)->(u32,u32){
    let key=binding.chord.key();let key=if key.len()==1{u32::from(key.as_bytes()[0])}else if let Some(n)=key.strip_prefix('F').and_then(|s|s.parse::<u32>().ok()){111+n}else if let Some(n)=key.strip_prefix("Numpad").filter(|n|n.len()==1).and_then(|s|s.parse::<u32>().ok()).filter(|n|*n<=9){96+n}else{match key{"NumLock"=>144,"NumpadMultiply"=>106,"NumpadAdd"=>107,"NumpadSubtract"=>109,"NumpadDecimal"=>110,"NumpadDivide"=>111,"ArrowLeft"=>37,"ArrowUp"=>38,"ArrowRight"=>39,"ArrowDown"=>40,"Tab"=>9,"Backspace"=>8,"Enter"=>13,"Insert"=>45,"Delete"=>46,"Home"=>36,"End"=>35,"PageUp"=>33,"PageDown"=>34,"Space"=>32,_=>0}};
    (key,(if binding.chord.primary(){2}else{0})|(if binding.chord.alt(){1}else{0})|(if binding.chord.shift(){4}else{0}))
}

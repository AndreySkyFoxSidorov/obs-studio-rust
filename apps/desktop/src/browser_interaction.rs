//! Modeless Browser Interaction adapter over real OSR paints and BrowserOwner.
//! OBS32 OBSBasicInteraction.cpp, copyright2023 Lain Bailey, GPL-2.0-or-later.
use eframe::egui;
use skylive_core::{BrowserSource,Id,Session,SourceKind,VideoFilterPreviewTarget};
use skylive_host::{browser::{BrowserOwner,BrowserError,BrowserInput,BrowserInputLease,BrowserFrame,BrowserSnapshot,interaction::{Preview,Key,MAX_PREVIEW_EDGE,MAX_PREVIEW_PIXELS}},transition::FrameScene};
use std::{collections::VecDeque,sync::Arc};
#[derive(Default)]pub struct Runtime {next:u64,window:Option<Window>,pending:Option<(u64,Id,Arc<BrowserSource>)>}
#[derive(Clone,Copy,PartialEq,Eq)]struct Lease {instance:u64,native_epoch:u64,native:u64,binding:u64}
struct Window {token:u64,view:u64,session:u64,source:Id,browser:Arc<BrowserSource>,lease:Option<Lease>,worker:Preview,
 texture:Option<egui::TextureHandle>,extent:(u32,u32),submitted:Option<Key>,presented:u64,
 focused:bool,closing:bool,finished:bool,hovered:bool,buttons:[bool;3],modifiers:egui::Modifiers,
 pointer:[i32;2],last_click:Option<(i32,f64,egui::Pos2,i32)>,queue:VecDeque<BrowserInput>,error:Option<String>}
impl Runtime {
 pub fn open(&mut self,session:&Session,source:Id,context:&egui::Context)->Result<(),String>{
  let model=session.snapshot();let browser=model.source(source).and_then(|s|match s.kind(){SourceKind::Browser {browser}=>Some(Arc::new(browser.clone())),_=>None}).ok_or("Select a Browser Source to interact with it.")?;
  if let Some(window)=&mut self.window {
   if window.session==session.identity()&&window.source==source&&window.browser==browser&&!window.closing {context.send_viewport_cmd_to(window.viewport(),egui::ViewportCommand::Focus);return Ok(());}
   window.begin_close();self.pending=Some((session.identity(),source,browser));return Ok(());
  }
  self.create(session.identity(),source,browser)
 }
 fn create(&mut self,session:u64,source:Id,browser:Arc<BrowserSource>)->Result<(),String>{
  self.next=self.next.checked_add(1).ok_or("Browser interaction identity exhausted.")?;
  self.window=Some(Window {token:self.next,view:1,session,source,browser,lease:None,worker:Preview::new().map_err(|e|e.to_string())?,texture:None,extent:(1,1),submitted:None,presented:0,focused:false,closing:false,finished:false,hovered:false,buttons:[false;3],modifiers:Default::default(),pointer:[0;2],last_click:None,queue:VecDeque::with_capacity(16),error:None});Ok(())
 }
 pub fn scope(&mut self,session:&Session,owner:&mut BrowserOwner)->Option<FrameScene>{
  self.observe(session,owner);
  let window=self.window.as_mut().filter(|w|!w.finished)?;
  if !window.valid(session){return None;}
  match FrameScene::preview_video_filters(window.session,&session.snapshot(),VideoFilterPreviewTarget::Source(window.source),(window.browser.width(),window.browser.height())) {
   Ok(scope)=>Some(scope),Err(error)=>{window.error=Some(error.to_string());None}
  }
 }
 fn observe(&mut self,session:&Session,owner:&mut BrowserOwner){
  let Some(window)=&mut self.window else{return;};
  if !window.valid(session){window.cancel_close();window.flush(session,owner);window.finished=window.queue.is_empty();return;}
  if let Some(pinned)=window.lease {
   let snapshot=owner.snapshot();
   if snapshot.session()!=window.session || snapshot.instance()!=pinned.instance || snapshot.native_epoch()!=pinned.native_epoch || snapshot.frame(window.source).is_none_or(|f|f.lease!=pinned.native||f.binding_lease!=pinned.binding||f.browser!=window.browser) {
    window.cancel_close();window.flush(session,owner);window.finished=window.queue.is_empty();
   }
  }
 }
 pub fn reserves_keyboard(&self)->bool{self.window.as_ref().is_some_and(|w|w.focused&&!w.finished)}
 pub fn show(&mut self,context:&egui::Context,session:&Session,owner:&mut BrowserOwner){
  self.observe(session,owner);
  if let Some(window)=&mut self.window {
   let name=session.snapshot().source(window.source).map(|s|s.name().to_owned()).unwrap_or_else(||crate::i18n::tr_context(context,"Browser Source"));
   let title=crate::i18n::tf_context(context,"Interact with {}",&[("{}",name)]);
   if !window.finished {
    context.show_viewport_immediate(window.viewport(),egui::ViewportBuilder::default().with_title(&title).with_inner_size([760.0,560.0]).with_min_inner_size([360.0,280.0]),|ctx,class|{
     if class==egui::ViewportClass::Embedded {
      let mut open=true;
      egui::Window::new(&title).id(egui::Id::new(("browser-interaction",window.token))).open(&mut open).default_size([760.0,560.0]).min_size([320.0,240.0]).show(ctx,|ui|window.body(ui,session,owner));
      if !open{window.begin_close();}
     }else{
      if ctx.input(|i|i.viewport().close_requested()){window.begin_close();ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);}
      egui::CentralPanel::default().show(ctx,|ui|window.body(ui,session,owner));
     }
     window.flush(session,owner);
     if window.closing&&window.queue.is_empty(){window.finished=true;if class!=egui::ViewportClass::Embedded{ctx.send_viewport_cmd(egui::ViewportCommand::Close);}}
     ctx.request_repaint_after(std::time::Duration::from_millis(16));
    });
   }
  }
  if self.window.as_ref().is_some_and(|w|w.finished){if let Some(window)=self.window.take(){context.send_viewport_cmd_to(window.viewport(),egui::ViewportCommand::Close);}}
  if self.window.is_none(){if let Some((id,source,browser))=self.pending.take(){if id==session.identity()&&session.snapshot().source(source).is_some_and(|s|matches!(s.kind(),SourceKind::Browser {browser:b}if b==browser.as_ref())){if let Err(error)=self.create(id,source,browser){eprintln!("browser interaction: {error}");}}}}
 }
 /// Called before the existing BrowserOwner shutdown; its CEF close releases
 /// every held key/button on the helper thread. No GUI join/CEF call is made.
 pub fn close(&mut self){self.pending=None;self.window=None;}
}
impl Window {
 fn viewport(&self)->egui::ViewportId{egui::ViewportId::from_hash_of(("browser-interaction",self.session,self.token))}
 fn valid(&self,session:&Session)->bool{!session.is_closed()&&session.identity()==self.session&&session.snapshot().source(self.source).is_some_and(|s|matches!(s.kind(),SourceKind::Browser {browser}if browser==self.browser.as_ref()))}
 fn release(&mut self){
  // Normal focus loss preserves already staged text/key ordering. Normal input
  // uses at most15 slots, leaving the16th for one exact-lease release.
  if self.lease.is_some()&&!matches!(self.queue.back(),Some(BrowserInput::Focus(false)))&&self.queue.len()<16 {self.queue.push_back(BrowserInput::Focus(false));}
  self.focused=false;self.buttons=[false;3];self.modifiers=Default::default();self.hovered=false;}
 fn cancel_close(&mut self){self.queue.clear();self.closing=true;self.release();}
 fn begin_close(&mut self){if !self.closing{self.closing=true;self.release();}}
 fn push(&mut self,event:BrowserInput)->bool{
  // Keep one slot for an exact-lease focus reset. Move coalescing never crosses
  // key/button events. A rejected burst is explicit, not partial text success.
  if matches!(event,BrowserInput::Move {..})&&self.queue.back().is_some_and(|old|matches!(old,BrowserInput::Move {..})){self.queue.pop_back();}
  if self.queue.len()>=15 {self.release();self.error=Some("Browser input is busy. Click the preview to resume.".into());return false;}
  self.queue.push_back(event);true
 }
 fn flush(&mut self,session:&Session,owner:&mut BrowserOwner){
  let Some(lease)=self.lease else{self.queue.clear();return;};
  for _ in 0..16 {let Some(event)=self.queue.front()else{break;};let result=if matches!(event,BrowserInput::Focus(false)) {
   owner.release_input(BrowserInputLease {instance:lease.instance,session:self.session,native_epoch:lease.native_epoch,source:self.source,lease:lease.native,binding_lease:lease.binding},&self.browser)
  }else{owner.input(session,self.source,lease.native,event.clone())};match result {
   Ok(())=>{self.queue.pop_front();},Err(BrowserError::Busy)=>break,
   Err(error)=>{self.queue.clear();self.focused=false;self.buttons=[false;3];self.error=Some(error.to_string());self.finished=true;break;}
  }}
 }
 fn frame<'a>(&self,snapshot:&'a BrowserSnapshot)->Option<&'a BrowserFrame>{snapshot.frame(self.source).filter(|f|snapshot.session()==self.session&&f.browser==self.browser&&self.lease.is_none_or(|lease|lease==Lease {instance:snapshot.instance(),native_epoch:snapshot.native_epoch(),native:f.lease,binding:f.binding_lease}))}
 fn body(&mut self,ui:&mut egui::Ui,session:&Session,owner:&mut BrowserOwner){
  let snapshot=owner.snapshot();let frame=self.frame(&snapshot);
  if self.lease.is_none(){if let Some(frame)=frame {self.lease=Some(Lease {instance:snapshot.instance(),native_epoch:snapshot.native_epoch(),native:frame.lease,binding:frame.binding_lease});}}
  let actions=[crate::views::modal_footer::Action::new("Refresh Browser",self.valid(session)&&!self.closing,"Reload the real browser page and reset interaction focus."),crate::views::modal_footer::Action::new("Close",true,"Close browser interaction and release its keyboard and mouse focus.")];
  let footer=crate::views::modal_footer::required_height(ui,&actions);
  let status_height=(ui.available_height()*0.28).clamp(64.0,140.0);
  egui::ScrollArea::vertical().id_salt(("browser-status",self.token)).max_height(status_height).auto_shrink([false,true]).show(ui,|ui|{
   ui.add(egui::Label::new(crate::i18n::tr(ui,"Click the preview to use the page. Escape releases keyboard focus.")).wrap());
   egui::CollapsingHeader::new(crate::i18n::tr(ui,"Input compatibility")).show(ui,|ui|{
    ui.add(egui::Label::new(crate::i18n::tr(ui,"Native IME composition and scan codes are unavailable; Unicode text events are forwarded as UTF-16. Clipboard integration is not provided.")).wrap());
   });
   if let Some(problem)=snapshot.problems().iter().find(|p|p.source==self.source){ui.add(egui::Label::new(egui::RichText::new(&problem.detail).color(ui.visuals().warn_fg_color)).wrap());}
   if let Some(error)=&self.error{ui.add(egui::Label::new(egui::RichText::new(crate::i18n::tr(ui,error)).color(ui.visuals().warn_fg_color)).wrap());}
  });
  let size=egui::vec2(ui.available_width().max(1.0),(ui.available_height()-footer-18.0).max(60.0));
  let (stage,_)=ui.allocate_exact_size(size,egui::Sense::hover());
  let ratio=(stage.width()/self.browser.width()as f32).min(stage.height()/self.browser.height()as f32);
  let rect=egui::Rect::from_center_size(stage.center(),egui::vec2(self.browser.width()as f32*ratio,self.browser.height()as f32*ratio));
  let response=ui.interact(rect,egui::Id::new(("browser-input",self.token)),egui::Sense::click_and_drag())
      .on_hover_text(crate::i18n::tr(ui,"Click the preview to use the page. Escape releases keyboard focus."))
      .on_disabled_hover_text(crate::i18n::tr(ui,"Waiting for browser frame"));
  let accessible_name=crate::i18n::tr(ui,"Browser page interaction");
  response.widget_info(||egui::WidgetInfo::labeled(egui::WidgetType::Image,frame.is_some(),accessible_name.clone()));
  checkerboard(ui,rect);
  let scale=(f64::from(rect.width()*ui.ctx().pixels_per_point())/f64::from(self.browser.width())).min(f64::from(rect.height()*ui.ctx().pixels_per_point())/f64::from(self.browser.height())).min(1.0).min(f64::from(MAX_PREVIEW_EDGE)/f64::from(self.browser.width())).min(f64::from(MAX_PREVIEW_EDGE)/f64::from(self.browser.height())).min((MAX_PREVIEW_PIXELS as f64/(f64::from(self.browser.width())*f64::from(self.browser.height()))).sqrt());
  let extent=((f64::from(self.browser.width())*scale).floor().max(1.0)as u32,(f64::from(self.browser.height())*scale).floor().max(1.0)as u32);
  if self.extent!=extent{let Some(view)=self.view.checked_add(1)else{self.error=Some("Browser interaction identity exhausted.".into());self.begin_close();return;};self.view=view;self.extent=extent;self.submitted=None;self.texture=None;self.presented=0;}
  if let (Some(frame),Some(lease))=(frame,self.lease){
   while let Some(reply)=self.worker.poll(){let key=reply.key;if key.window==self.token&&key.view==self.view&&key.session==self.session&&key.source==self.source&&key.instance==lease.instance&&key.native_epoch==lease.native_epoch&&key.lease==lease.native&&key.binding_lease==lease.binding&&key.width==extent.0&&key.height==extent.1&&key.sequence>self.presented&&key.sequence<=frame.sequence {
    match reply.image {Ok(image)=>{let image=egui::ColorImage::from_rgba_unmultiplied([image.width()as usize,image.height()as usize],image.pixels());if let Some(texture)=&mut self.texture{texture.set(image,egui::TextureOptions::LINEAR);}else{self.texture=Some(ui.ctx().load_texture(format!("browser-interaction-{}",self.token),image,egui::TextureOptions::LINEAR));}self.presented=key.sequence;},Err(error)=>self.error=Some(error.to_string())}
   }}
   let key=Key {window:self.token,view:self.view,session:self.session,source:self.source,instance:lease.instance,native_epoch:lease.native_epoch,lease:lease.native,binding_lease:lease.binding,sequence:frame.sequence,width:extent.0,height:extent.1};
   if self.submitted!=Some(key){match self.worker.submit(key,frame){Ok(())=>self.submitted=Some(key),Err(BrowserError::Busy)=>{},Err(error)=>self.error=Some(error.to_string())}}
  }
  if let Some(texture)=&self.texture{ui.painter().image(texture.id(),rect,egui::Rect::from_min_max(egui::Pos2::ZERO,egui::pos2(1.0,1.0)),egui::Color32::WHITE);}else{ui.painter().text(rect.center(),egui::Align2::CENTER_CENTER,crate::i18n::tr(ui,"Waiting for browser frame"),egui::TextStyle::Body.resolve(ui.style()),ui.visuals().text_color());}
  if self.focused {ui.painter().rect_stroke(rect,0.0,egui::Stroke::new(2.0,ui.visuals().selection.stroke.color),egui::StrokeKind::Inside);}
  if frame.is_some()&&self.texture.is_some()&&!self.closing {self.events(ui,&response,rect);}else if self.focused{self.release();}
  ui.separator();match crate::views::modal_footer::show(ui,&actions){Some(0)=>{self.release();self.flush(session,owner);match owner.refresh(session,self.source){Ok(())=>{self.queue.clear();self.lease=None;self.texture=None;self.submitted=None;self.presented=0;self.error=None;},Err(error)=>self.error=Some(error.to_string())}},Some(1)=>self.begin_close(),_=>{}}
 }
 fn position(&self,point:egui::Pos2,rect:egui::Rect)->[i32;2]{[((point.x-rect.left())/rect.width()*self.browser.width()as f32).floor()as i32,((point.y-rect.top())/rect.height()*self.browser.height()as f32).floor()as i32]}
 fn flags(&self,modifiers:egui::Modifiers)->u32{(if modifiers.shift{2}else{0})|(if modifiers.ctrl{4}else{0})|(if modifiers.alt{8}else{0})|(if modifiers.mac_cmd{128}else{0})|(if self.buttons[0]{16}else{0})|(if self.buttons[1]{32}else{0})|(if self.buttons[2]{64}else{0})}
 fn sync_modifiers(&mut self,next:egui::Modifiers){
  if !self.focused{return;}
  for (old,new,key) in [(self.modifiers.shift,next.shift,16),(self.modifiers.ctrl,next.ctrl,17),(self.modifiers.alt,next.alt,18),(self.modifiers.mac_cmd,next.mac_cmd,91)] {if old!=new&&!self.push(BrowserInput::Key {kind:if new{0}else{2},modifiers:self.flags(next),virtual_key:key,native_key:0,character:0}){return;}}
  self.modifiers=next;
 }
 fn text(&mut self,text:&str,modifiers:egui::Modifiers){
  let count=text.encode_utf16().take(16).count();if count>15usize.saturating_sub(self.queue.len()){self.release();self.error=Some("Browser text input exceeds the bounded input queue; no text from this event was sent.".into());return;}
  for unit in text.encode_utf16(){self.push(BrowserInput::Key {kind:3,modifiers:self.flags(modifiers),virtual_key:i32::from(unit),native_key:0,character:unit});}
 }
 fn events(&mut self,ui:&mut egui::Ui,response:&egui::Response,rect:egui::Rect){
  let ctx=ui.ctx();let native_focus=ctx.input(|i|i.focused);
  if self.focused&&(!native_focus||!response.has_focus()){self.release();}
  if self.focused {ui.memory_mut(|m|m.set_focus_lock_filter(response.id,egui::EventFilter {tab:true,horizontal_arrows:true,vertical_arrows:true,escape:true}));}
  let time=ctx.input(|i|i.time);
  // Move events instead of cloning clipboard/text payloads. Events belonging
  // to other widgets are put back, preserving main-window shortcuts/text.
  let events=ctx.input_mut(|i|std::mem::take(&mut i.events));let mut unused=Vec::new();
  for event in events {
   let interested=match &event {
    egui::Event::PointerButton {pos,pressed,button,..}=>rect.contains(*pos)||(*pressed&&self.focused)||match button{egui::PointerButton::Primary=>self.buttons[0],egui::PointerButton::Middle=>self.buttons[1],egui::PointerButton::Secondary=>self.buttons[2],_=>false},
    egui::Event::PointerMoved(pos)=>rect.contains(*pos)||self.hovered||self.buttons.iter().any(|b|*b),
    egui::Event::PointerGone=>self.hovered,
    egui::Event::MouseWheel {..}=>response.hovered(),
    egui::Event::Key {..}|egui::Event::Text(_)|egui::Event::Ime(_)|egui::Event::Copy|egui::Event::Cut|egui::Event::Paste(_)=>self.focused,
    egui::Event::WindowFocused(false)=>self.focused,
    _=>false,
   };
   if !interested{unused.push(event);continue;}
   match event {
   egui::Event::PointerButton {pos,button,pressed,modifiers}=>{
    let button=match button{egui::PointerButton::Primary=>0,egui::PointerButton::Middle=>1,egui::PointerButton::Secondary=>2,_=>continue};
    if pressed&&rect.contains(pos){if !self.focused{self.focused=true;response.request_focus();ui.memory_mut(|m|m.set_focus_lock_filter(response.id,egui::EventFilter {tab:true,horizontal_arrows:true,vertical_arrows:true,escape:true}));self.error=None;self.push(BrowserInput::Focus(true));}self.sync_modifiers(modifiers);if !self.focused{continue;}}
    if (pressed&&rect.contains(pos))||(!pressed&&self.buttons[button]){
     self.pointer=self.position(pos,rect);self.buttons[button]=pressed;
     let count=if pressed {let count=self.last_click.filter(|(b,t,p,_)|*b==button as i32&&time-*t<=0.4&&p.distance(pos)<=6.0).map_or(1,|(_,_,_,n)|(n+1).min(3));self.last_click=Some((button as i32,time,pos,count));count}else{1};
     self.push(BrowserInput::Click {x:self.pointer[0],y:self.pointer[1],modifiers:self.flags(modifiers),button:button as i32,up:!pressed,count});
    }else if pressed&&self.focused{self.release();response.surrender_focus();}
   },
   egui::Event::PointerMoved(pos)=>{self.pointer=self.position(pos,rect);let inside=rect.contains(pos);if inside||self.hovered||self.buttons.iter().any(|b|*b){self.push(BrowserInput::Move {x:self.pointer[0],y:self.pointer[1],modifiers:self.flags(ctx.input(|i|i.modifiers)),leave:!inside});}self.hovered=inside;},
   egui::Event::PointerGone=>{if self.hovered{self.push(BrowserInput::Move {x:self.pointer[0],y:self.pointer[1],modifiers:0,leave:true});self.hovered=false;}},
   egui::Event::MouseWheel {unit,delta,modifiers}=>if response.hovered(){if let Some(pos)=ctx.input(|i|i.pointer.hover_pos()){self.pointer=self.position(pos,rect);}let multiplier=match unit{egui::MouseWheelUnit::Point=>egui::vec2(1.0,1.0),egui::MouseWheelUnit::Line=>egui::vec2(120.0,120.0),egui::MouseWheelUnit::Page=>egui::vec2(self.browser.width()as f32,self.browser.height()as f32)};self.push(BrowserInput::Wheel {x:self.pointer[0],y:self.pointer[1],modifiers:self.flags(modifiers),dx:(delta.x*multiplier.x).round()as i32,dy:(delta.y*multiplier.y).round()as i32});},
   egui::Event::Key {key,pressed,repeat,modifiers,..}=>if self.focused{if ((modifiers.command||modifiers.ctrl)&&matches!(key,egui::Key::C|egui::Key::X|egui::Key::V|egui::Key::Insert))||(modifiers.shift&&matches!(key,egui::Key::Insert|egui::Key::Delete)){self.error=Some("Clipboard integration is not provided in Browser Interaction.".into());continue;}self.sync_modifiers(modifiers);if !self.focused{continue;}if let Some(vk)=virtual_key(key){self.push(BrowserInput::Key {kind:if pressed{0}else{2},modifiers:self.flags(modifiers)|if repeat{8192}else{0},virtual_key:vk,native_key:0,character:0});if pressed {if key==egui::Key::Escape{self.release();response.surrender_focus();}else if let Some(text)=match key{egui::Key::Enter=>Some("\r"),egui::Key::Tab=>Some("\t"),egui::Key::Backspace=>Some("\u{8}"),_=>None}{self.text(text,modifiers);}}}},
   egui::Event::Text(text)=>if self.focused{self.text(&text,ctx.input(|i|i.modifiers));},
   egui::Event::Ime(egui::ImeEvent::Commit(text))=>if self.focused{self.text(&text,ctx.input(|i|i.modifiers));},
   egui::Event::WindowFocused(false)=>self.release(),
   egui::Event::Paste(_)|egui::Event::Copy|egui::Event::Cut=>if self.focused{self.error=Some("Clipboard integration is not provided in Browser Interaction.".into());},
   _=>{},
  }}
  self.sync_modifiers(ctx.input(|i|i.modifiers));
  ctx.input_mut(|i|i.events.extend(unused));
 }
}
fn virtual_key(key:egui::Key)->Option<i32>{use egui::Key::*;Some(match key {
 ArrowLeft=>37,ArrowUp=>38,ArrowRight=>39,ArrowDown=>40,Escape=>27,Tab=>9,Backspace=>8,Enter=>13,Space=>32,Insert=>45,Delete=>46,Home=>36,End=>35,PageUp=>33,PageDown=>34,
 A=>65,B=>66,C=>67,D=>68,E=>69,F=>70,G=>71,H=>72,I=>73,J=>74,K=>75,L=>76,M=>77,N=>78,O=>79,P=>80,Q=>81,R=>82,S=>83,T=>84,U=>85,V=>86,W=>87,X=>88,Y=>89,Z=>90,
 Num0=>48,Num1=>49,Num2=>50,Num3=>51,Num4=>52,Num5=>53,Num6=>54,Num7=>55,Num8=>56,Num9=>57,
 F1=>112,F2=>113,F3=>114,F4=>115,F5=>116,F6=>117,F7=>118,F8=>119,F9=>120,F10=>121,F11=>122,F12=>123,F13=>124,F14=>125,F15=>126,F16=>127,F17=>128,F18=>129,F19=>130,F20=>131,F21=>132,F22=>133,F23=>134,F24=>135,
 Semicolon|Colon=>186,Equals|Plus=>187,Comma=>188,Minus=>189,Period=>190,Slash|Questionmark=>191,Backtick=>192,OpenBracket|OpenCurlyBracket=>219,Backslash|Pipe=>220,CloseBracket|CloseCurlyBracket=>221,Quote=>222,_=>return None,
})}
fn checkerboard(ui:&egui::Ui,rect:egui::Rect){let cell=(rect.width().max(rect.height())/80.0).max(12.0);let a=ui.visuals().extreme_bg_color;let b=ui.visuals().faint_bg_color;ui.painter().rect_filled(rect,0.0,a);let columns=(rect.width()/cell).ceil()as usize;let rows=(rect.height()/cell).ceil()as usize;for y in 0..rows{for x in 0..columns{if (x+y)%2==1{let tile=egui::Rect::from_min_size(rect.min+egui::vec2(x as f32*cell,y as f32*cell),egui::vec2(cell,cell)).intersect(rect);ui.painter().rect_filled(tile,0.0,b);}}}}

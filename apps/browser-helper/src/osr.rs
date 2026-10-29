//! Main-thread CEF lifecycle and pure-Rust C ABI callbacks. No native browser
//! window is created; every pixel reaches the parent through bounded OSR IPC.
use crate::cef::*;
use skylive_browser_protocol as wire;
use std::{ffi::c_void,collections::{HashMap,VecDeque},io::{self,Write},mem::size_of,ptr,sync::{Arc,Weak,Mutex,OnceLock,mpsc,atomic::{AtomicBool,AtomicUsize,AtomicU64,Ordering}},thread,time::{Duration,Instant}};
use windows::Win32::System::{LibraryLoader::GetModuleHandleW,Performance::{QueryPerformanceCounter,QueryPerformanceFrequency}};
// Context initialization is acknowledged by CEF itself before CreateBrowser.
// App and process-handler objects have independent native reference counts.
#[repr(C)]struct ContextObject {api:BrowserProcessHandler,refs:AtomicUsize,ready:Arc<AtomicBool>}
unsafe extern "system" fn context_add(p:*mut Base){unsafe {(*(p.cast::<ContextObject>())).refs.fetch_add(1,Ordering::Relaxed)};}
unsafe extern "system" fn context_drop(p:*mut Base)->i32{if unsafe {(*(p.cast::<ContextObject>())).refs.fetch_sub(1,Ordering::AcqRel)}==1{drop(unsafe {Box::from_raw(p.cast::<ContextObject>())});1}else{0}}
unsafe extern "system" fn context_one(p:*mut Base)->i32{(unsafe {(*(p.cast::<ContextObject>())).refs.load(Ordering::Acquire)}==1)as i32}
unsafe extern "system" fn context_any(p:*mut Base)->i32{(unsafe {(*(p.cast::<ContextObject>())).refs.load(Ordering::Acquire)}>0)as i32}
unsafe extern "system" fn context_ready(p:*mut BrowserProcessHandler){unsafe {(*(p.cast::<ContextObject>())).ready.store(true,Ordering::Release)};}
unsafe extern "system" fn no_relaunch_window(_:*mut BrowserProcessHandler,_:*mut c_void,_:*const CefString)->i32{1}
#[repr(C)]struct AppObject {api:AppApi,refs:AtomicUsize,handler:usize}
impl Drop for AppObject {fn drop(&mut self){unsafe {release(self.handler as *mut Base)};}}
unsafe extern "system" fn app_add(p:*mut Base){unsafe {(*(p.cast::<AppObject>())).refs.fetch_add(1,Ordering::Relaxed)};}
unsafe extern "system" fn app_drop(p:*mut Base)->i32{if unsafe {(*(p.cast::<AppObject>())).refs.fetch_sub(1,Ordering::AcqRel)}==1{drop(unsafe {Box::from_raw(p.cast::<AppObject>())});1}else{0}}
unsafe extern "system" fn app_one(p:*mut Base)->i32{(unsafe {(*(p.cast::<AppObject>())).refs.load(Ordering::Acquire)}==1)as i32}
unsafe extern "system" fn app_any(p:*mut Base)->i32{(unsafe {(*(p.cast::<AppObject>())).refs.load(Ordering::Acquire)}>0)as i32}
unsafe extern "system" fn get_process_handler(p:*mut AppApi)->*mut BrowserProcessHandler{let handler=unsafe {(*(p.cast::<AppObject>())).handler}as *mut Base;if let Some(add)=unsafe {(*handler).add_ref}{unsafe {add(handler)};}handler.cast()}
struct AppLease(*mut AppApi);impl Drop for AppLease {fn drop(&mut self){unsafe {release(self.0)};}}
fn application(ready:Arc<AtomicBool>)->AppLease{
 let mut process:BrowserProcessHandler=zero();process.base=Base{size:size_of::<BrowserProcessHandler>(),add_ref:Some(context_add),release:Some(context_drop),has_one_ref:Some(context_one),has_at_least_one_ref:Some(context_any)};process.on_context_initialized=Some(context_ready);process.on_already_running_app_relaunch=Some(no_relaunch_window);
 let process=Box::into_raw(Box::new(ContextObject{api:process,refs:AtomicUsize::new(1),ready}));
 let mut app:AppApi=zero();app.base=Base{size:size_of::<AppApi>(),add_ref:Some(app_add),release:Some(app_drop),has_one_ref:Some(app_one),has_at_least_one_ref:Some(app_any)};app.get_browser_process_handler=Some(get_process_handler);
 AppLease(Box::into_raw(Box::new(AppObject{api:app,refs:AtomicUsize::new(1),handler:process as usize})).cast())
}
struct Budget {bytes:AtomicUsize,count:AtomicUsize,max_bytes:usize,max_count:usize}
struct Credit {budget:Arc<Budget>,bytes:usize}
impl Budget {fn reserve(self:&Arc<Self>,bytes:usize)->Option<Credit>{self.count.fetch_update(Ordering::AcqRel,Ordering::Acquire,|n|(n<self.max_count).then_some(n+1)).ok()?;if self.bytes.fetch_update(Ordering::AcqRel,Ordering::Acquire,|n|n.checked_add(bytes).filter(|&n|n<=self.max_bytes)).is_err(){self.count.fetch_sub(1,Ordering::AcqRel);return None;}Some(Credit {budget:self.clone(),bytes})}}
impl Drop for Credit {fn drop(&mut self){self.budget.bytes.fetch_sub(self.bytes,Ordering::AcqRel);self.budget.count.fetch_sub(1,Ordering::AcqRel);}}
struct Paint {event:wire::Event,_credit:Credit}
#[derive(Default)]struct AudioState{grant:Option<wire::AudioGrant>,queue:VecDeque<Paint>}
struct SurfaceBytes {width:usize,height:usize,pixels:Vec<u8>,_credit:Credit}
#[derive(Default)]struct Surface {base:Option<SurfaceBytes>,popup:Option<SurfaceBytes>,rect:Rect,show:bool,dirty:bool,qpc_ns:u64}
struct State {audio:Mutex<AudioState>,audio_running:AtomicBool,audio_generation:AtomicU64,audio_sequence:AtomicU64,audio_dropped:AtomicU64,audio_budget:Arc<Budget>,source:wire::Source,session:u64,closed:AtomicBool,visible:AtomicBool,ready:AtomicBool,surface:Mutex<Surface>,budget_reported:AtomicBool,last_paint:Mutex<Option<Instant>>,paint:Mutex<Option<Paint>>,events:mpsc::SyncSender<wire::Event>,budget:Arc<Budget>,handlers:OnceLock<Weak<Handlers>>,frequency:u64,string_free:unsafe extern "C" fn(*mut CefString)}
impl State {fn event(&self,kind:u32,message:String){let message=if message.len()>4096{eprintln!("CEF diagnostic: {message}");"CEF diagnostic exceeds the bounded IPC message; complete text is in the helper log".into()}else{message};let _=self.events.try_send(wire::Event::State {session:self.session,source:self.source.id,lease:self.source.lease,kind,message});}}
struct Handlers {pointers:[usize;11]}
impl Drop for Handlers {fn drop(&mut self){for p in self.pointers {unsafe {release(p as *mut Base)};}}}
#[repr(C)]struct Object<T:Capi>{api:T,refs:AtomicUsize,state:Arc<State>,_handlers:Option<Arc<Handlers>>}
unsafe extern "system" fn add<T:Capi>(p:*mut Base){unsafe {(*(p.cast::<Object<T>>())).refs.fetch_add(1,Ordering::Relaxed)};}
unsafe extern "system" fn drop_ref<T:Capi>(p:*mut Base)->i32{if unsafe {(*(p.cast::<Object<T>>())).refs.fetch_sub(1,Ordering::AcqRel)}==1 {drop(unsafe {Box::from_raw(p.cast::<Object<T>>())});1}else{0}}
unsafe extern "system" fn one<T:Capi>(p:*mut Base)->i32{(unsafe {(*(p.cast::<Object<T>>())).refs.load(Ordering::Acquire)}==1) as i32}
unsafe extern "system" fn any<T:Capi>(p:*mut Base)->i32{(unsafe {(*(p.cast::<Object<T>>())).refs.load(Ordering::Acquire)}>0) as i32}
fn object<T:Capi>(mut api:T,state:Arc<State>,handlers:Option<Arc<Handlers>>)->*mut T{*api.base()=Base {size:size_of::<T>(),add_ref:Some(add::<T>),release:Some(drop_ref::<T>),has_one_ref:Some(one::<T>),has_at_least_one_ref:Some(any::<T>)};Box::into_raw(Box::new(Object {api,refs:AtomicUsize::new(1),state,_handlers:handlers})).cast()}
unsafe fn state<'a,T:Capi>(p:*mut T)->&'a State{unsafe {&(*(p.cast::<Object<T>>())).state}}
unsafe fn handler<T>(p:*mut Client,index:usize)->*mut T{let state=unsafe {state(p)};let Some(handlers)=state.handlers.get().and_then(Weak::upgrade)else{return ptr::null_mut();};let p=handlers.pointers[index] as *mut Base;if let Some(add)=unsafe {(*p).add_ref}{unsafe {add(p)};}p.cast()}
macro_rules! getter {($name:ident,$t:ty,$i:expr)=>{unsafe extern "system" fn $name(p:*mut Client)->*mut $t{unsafe {handler(p,$i)}}};}
getter!(render,RenderHandler,0);getter!(life,LifeSpanHandler,1);getter!(load,LoadHandler,2);getter!(js,JsdialogHandler,3);getter!(dialog,DialogHandler,4);getter!(context,ContextMenuHandler,5);getter!(download,DownloadHandler,6);getter!(request,RequestHandler,7);getter!(permission,PermissionHandler,9);
unsafe extern "system" fn audio_handler(p:*mut Client)->*mut AudioHandler{if unsafe {state(p)}.source.control_audio{unsafe {handler(p,10)}}else{ptr::null_mut()}}
unsafe extern "system" fn audio_parameters(p:*mut AudioHandler,_:*mut Browser,params:*mut AudioParameters)->i32{if params.is_null()||!unsafe {state(p)}.source.control_audio{return 0;}unsafe {*params=AudioParameters{channel_layout:3,sample_rate:48000,frames_per_buffer:1024}};1}
unsafe extern "system" fn audio_started(p:*mut AudioHandler,_:*mut Browser,params:*const AudioParameters,channels:i32){
 let s=unsafe {state(p)};s.audio_running.store(false,Ordering::Release);if params.is_null(){s.event(6,"CEF audio start has no actual format".into());return;}let format=unsafe {*params};
 if format.channel_layout!=3||format.sample_rate!=48000||channels!=2||!(1..=4096).contains(&format.frames_per_buffer){s.event(6,format!("CEF actual audio format unsupported: {}Hz/{channels}channels/layout{}/{}frames",format.sample_rate,format.channel_layout,format.frames_per_buffer));return;}
 let Ok(previous)=s.audio_generation.fetch_update(Ordering::AcqRel,Ordering::Acquire,|v|v.checked_add(1))else{s.event(6,"CEF audio stream identity exhausted".into());return;};
 let _=s.events.try_send(wire::Event::AudioFormat{session:s.session,source:s.source.id,lease:s.source.lease,stream:previous+1,rate:48000,channels:2,layout:3,frames:format.frames_per_buffer as u32});s.audio_running.store(true,Ordering::Release);
}
unsafe extern "system" fn audio_packet(p:*mut AudioHandler,_:*mut Browser,data:*const *const f32,frames:i32,pts:i64){
 let s=unsafe {state(p)};if !s.source.control_audio||!s.audio_running.load(Ordering::Acquire)||s.closed.load(Ordering::Acquire){return;}
 let Ok(previous)=s.audio_sequence.fetch_update(Ordering::AcqRel,Ordering::Acquire,|v|v.checked_add(1))else{s.audio_running.store(false,Ordering::Release);s.event(6,"CEF audio packet identity exhausted".into());return;};let sequence=previous+1;
 let Ok(mut audio)=s.audio.try_lock()else{s.audio_dropped.fetch_add(1,Ordering::Relaxed);return;};let Some(grant)=audio.grant else{return;};
 if frames<=0||frames>4096||data.is_null()||pts<0||audio.queue.len()>=8{s.audio_dropped.fetch_add(1,Ordering::Relaxed);return;}
 let frames=frames as usize;let planes=unsafe {std::slice::from_raw_parts(data,2)};if planes.iter().any(|p|p.is_null()){s.audio_dropped.fetch_add(1,Ordering::Relaxed);s.event(6,"CEF audio channel plane is null".into());return;}
 let Some(credit)=s.audio_budget.reserve(frames*8)else{s.audio_dropped.fetch_add(1,Ordering::Relaxed);return;};
 let Some(qpc_ns)=qpc(s.frequency).filter(|qpc|*qpc>=grant.start_qpc_ns)else{return;};let Some(unix_ns)=std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok().and_then(|v|u64::try_from(v.as_nanos()).ok())else{s.event(6,"CEF audio callback clock is unavailable".into());return;};
 let left=unsafe {std::slice::from_raw_parts(planes[0],frames)};let right=unsafe {std::slice::from_raw_parts(planes[1],frames)};let mut samples=Vec::new();if samples.try_reserve_exact(frames*2).is_err(){s.audio_dropped.fetch_add(1,Ordering::Relaxed);return;}
 for i in 0..frames{if !left[i].is_finite()||!right[i].is_finite(){s.audio_dropped.fetch_add(1,Ordering::Relaxed);s.event(6,"CEF audio contains a nonfinite sample".into());return;}samples.push(left[i]);samples.push(right[i]);}
 let stream=s.audio_generation.load(Ordering::Acquire);let dropped=s.audio_dropped.swap(0,Ordering::AcqRel);
 audio.queue.push_back(Paint{event:wire::Event::Audio{session:s.session,source:s.source.id,lease:s.source.lease,grant,stream,sequence,pts_ms:pts,qpc_ns,unix_ns,dropped,frames:frames as u32,samples},_credit:credit});
}
unsafe extern "system" fn audio_stopped(p:*mut AudioHandler,_:*mut Browser){unsafe {state(p)}.audio_running.store(false,Ordering::Release);}
unsafe extern "system" fn audio_error(p:*mut AudioHandler,_:*mut Browser,message:*const CefString){let s=unsafe {state(p)};s.audio_running.store(false,Ordering::Release);s.event(6,format!("CEF audio stream error: {}",unsafe {read_string(message,1800)}.unwrap_or_else(||"CEF error text exceeds its bound".into())));}
unsafe extern "system" fn rect(p:*mut RenderHandler,_:*mut Browser,rect:*mut Rect){if !rect.is_null(){let s=unsafe {state(p)};unsafe {*rect=Rect {x:0,y:0,width:s.source.width as i32,height:s.source.height as i32}};}}
fn qpc(frequency:u64)->Option<u64>{if frequency==0{return None;}let mut count=0i64;unsafe {QueryPerformanceCounter(&mut count)}.ok()?;u64::try_from(u128::try_from(count).ok()?*1_000_000_000/u128::from(frequency)).ok()}
unsafe extern "system" fn screen(p:*mut RenderHandler,_:*mut Browser,info:*mut ScreenInfo)->i32{if info.is_null(){return 0;}let s=unsafe {state(p)};let rect=Rect{x:0,y:0,width:s.source.width as i32,height:s.source.height as i32};unsafe {*info=ScreenInfo {device_scale_factor:1.0,depth:32,depth_per_component:8,is_monochrome:0,rect,available_rect:rect};}1}
unsafe extern "system" fn popup_show(p:*mut RenderHandler,_:*mut Browser,show:i32){let s=unsafe {state(p)};if let Ok(mut surface)=s.surface.try_lock(){surface.show=show!=0;if show==0 {surface.popup.take();}surface.dirty=true;surface.qpc_ns=qpc(s.frequency).unwrap_or(surface.qpc_ns);}}
unsafe extern "system" fn popup_size(p:*mut RenderHandler,_:*mut Browser,rect:*const Rect){if rect.is_null(){return;}let s=unsafe {state(p)};let rect=unsafe {*rect};if let Ok(mut surface)=s.surface.try_lock(){surface.popup.take();surface.dirty=true;if rect.width<=0||rect.height<=0||rect.width>4096||rect.height>4096{surface.show=false;}else{surface.rect=rect;}surface.qpc_ns=qpc(s.frequency).unwrap_or(surface.qpc_ns);}}
unsafe extern "system" fn paint(p:*mut RenderHandler,_:*mut Browser,kind:i32,_:usize,_:*const Rect,buffer:*const c_void,width:i32,height:i32){
 let s=unsafe {state(p)};if ![0,1].contains(&kind)||!s.ready.load(Ordering::Acquire)||!s.visible.load(Ordering::Acquire)||s.closed.load(Ordering::Acquire)||width<=0||height<=0||buffer.is_null(){return;}
 if kind==0&&(width!=s.source.width as i32||height!=s.source.height as i32){return;}
 let Ok(bytes)=wire::frame_bytes(width as u32,height as u32)else{return;};if kind==1&&bytes>16*1024*1024{return;}
 let Ok(mut surface)=s.surface.try_lock()else{return;};if kind==1&&(!surface.show||surface.rect.width!=width||surface.rect.height!=height){return;}
 if kind==0 {surface.base.take();}else{surface.popup.take();}
 let Some(credit)=s.budget.reserve(bytes)else{if !s.budget_reported.swap(true,Ordering::AcqRel){s.event(5,"Browser application paint budget exceeded".into());}return;};let Some(qpc_ns)=qpc(s.frequency)else{return;};
 let pixels=unsafe {std::slice::from_raw_parts(buffer.cast::<u8>(),bytes)}.to_vec();let image=SurfaceBytes {width:width as usize,height:height as usize,pixels,_credit:credit};if kind==0{surface.base=Some(image);}else{surface.popup=Some(image);}surface.qpc_ns=qpc_ns;surface.dirty=true;
}
impl State {
 fn flush_surface(&self){if !self.visible.load(Ordering::Acquire)||!self.ready.load(Ordering::Acquire){return;}let Ok(mut last)=self.last_paint.try_lock()else{return;};if last.is_some_and(|at|at.elapsed()<Duration::from_nanos(1_000_000_000/u64::from(self.source.fps))){return;}let Ok(mut surface)=self.surface.try_lock()else{return;};if !surface.dirty{return;}let Some(base)=surface.base.as_ref()else{return;};let Some(credit)=self.budget.reserve(base.pixels.len())else{if !self.budget_reported.swap(true,Ordering::AcqRel){self.event(5,"Browser application paint budget exceeded".into());}return;};let mut pixels=base.pixels.clone();
  // Compose every popup against the clean full-view base. PET_POPUP can never
  // become a small whole-source frame or accumulate into an earlier overlay.
  if surface.show {if let Some(popup)=&surface.popup {for y in 0..popup.height {let dy=i64::from(surface.rect.y)+y as i64;if !(0..base.height as i64).contains(&dy){continue;}for x in 0..popup.width {let dx=i64::from(surface.rect.x)+x as i64;if !(0..base.width as i64).contains(&dx){continue;}let a=(y*popup.width+x)*4;let b=(dy as usize*base.width+dx as usize)*4;let alpha=u32::from(popup.pixels[a+3]);for channel in 0..4 {pixels[b+channel]=(u32::from(popup.pixels[a+channel])+(u32::from(pixels[b+channel])*(255-alpha)+127)/255).min(255) as u8;}}}}}
  if let Ok(mut slot)=self.paint.try_lock(){*slot=Some(Paint {event:wire::Event::Paint {session:self.session,source:self.source.id,lease:self.source.lease,width:self.source.width,height:self.source.height,qpc_ns:surface.qpc_ns,pixels},_credit:credit});surface.dirty=false;self.budget_reported.store(false,Ordering::Release);*last=Some(Instant::now());}
 }
}
unsafe extern "system" fn popup(_:*mut LifeSpanHandler,_:*mut Browser,_:*mut Frame,_:*const CefString,_:*const CefString,_:i32,_:i32,_:*const c_void,_:*mut WindowInfo,_:*mut *mut Client,_:*mut BrowserSettings,_:*mut *mut c_void,_:*mut i32)->i32{1}
unsafe extern "system" fn before_close(p:*mut LifeSpanHandler,_:*mut Browser){let s=unsafe {state(p)};s.closed.store(true,Ordering::Release);s.event(2,String::new());}
fn quoted(s:&str)->String{let mut out=String::from("\"");for c in s.chars(){match c {'"'=>out.push_str("\\\""),'\\'=>out.push_str("\\\\"),'\n'=>out.push_str("\\n"),'\r'=>out.push_str("\\r"),'\t'=>out.push_str("\\t"),c if c<' '=>out.push_str(&format!("\\u{:04x}",c as u32)),c=>out.push(c)}}out.push('"');out}
unsafe fn read_string(p:*const CefString,max:usize)->Option<String>{if p.is_null(){return Some(String::new());}let v=unsafe {&*p};if v.length>max||v.length>0&&v.str_.is_null(){return None;}if v.length==0{return Some(String::new());}String::from_utf16(unsafe {std::slice::from_raw_parts(v.str_,v.length)}).ok()}
unsafe extern "system" fn loaded(p:*mut LoadHandler,browser:*mut Browser,frame:*mut Frame,_:i32){
 if frame.is_null()||browser.is_null(){return;}if unsafe {(*frame).is_main}.is_none_or(|f|unsafe {f(frame)}==0){return;}let s=unsafe {state(p)};
 // Source-controlled audio is muted per browser and captured by AudioHandler.
 // Native playback also requires an original audio-view eligibility receipt. An engine-generated blank
 // completion is not a loaded source page and never publishes readiness.
 let url=unsafe {(*frame).get_url}.map(|f|unsafe {f(frame)}).unwrap_or(ptr::null_mut());let current=unsafe {read_string(url,16384)};if !url.is_null(){unsafe {(s.string_free)(url)};}if current.as_deref()==Some("about:blank"){return;}
 let script=format!("(()=>{{let s=document.getElementById('__skylive_css');if(!s){{s=document.createElement('style');s.id='__skylive_css';(document.head||document.documentElement).appendChild(s);}}s.textContent={};}})();",quoted(&s.source.css));
 let mut code=Wide::new(&script);let mut origin=Wide::new("skylive-browser-css");if let Some(run)=unsafe {(*frame).execute_java_script}{unsafe {run(frame,&code.value(),&origin.value(),1)};}
 s.ready.store(true,Ordering::Release);s.event(4,String::new());let host=unsafe {(*browser).get_host}.map(|f|unsafe {f(browser)}).unwrap_or(ptr::null_mut());if !host.is_null(){if let Some(invalidate)=unsafe {(*host).invalidate}{unsafe {invalidate(host,0)};}unsafe {release(host)};}
}
unsafe extern "system" fn load_error(p:*mut LoadHandler,_:*mut Browser,frame:*mut Frame,code:i32,text:*const CefString,_:*const CefString){if frame.is_null()||unsafe {(*frame).is_main}.is_none_or(|f|unsafe {f(frame)}==0)||code==-3{return;}let s=unsafe {state(p)};s.ready.store(false,Ordering::Release);let detail=unsafe {read_string(text,1800)}.unwrap_or_else(||"CEF error text exceeds its bound".into());s.event(3,format!("CEF load error {code}: {detail}"));}
unsafe extern "system" fn jsdialog(_:*mut JsdialogHandler,_:*mut Browser,_:*const CefString,_:i32,_:*const CefString,_:*const CefString,_:*mut JsdialogCallback,suppress:*mut i32)->i32{if !suppress.is_null(){unsafe {*suppress=1};}0}
unsafe extern "system" fn unload(_:*mut JsdialogHandler,_:*mut Browser,_:*const CefString,_:i32,callback:*mut JsdialogCallback)->i32{if !callback.is_null(){if let Some(cont)=unsafe {(*callback).cont}{unsafe {cont(callback,1,&CefString::default())};}}1}
unsafe extern "system" fn file_dialog(_:*mut DialogHandler,_:*mut Browser,_:i32,_:*const CefString,_:*const CefString,_:*mut c_void,_:*mut c_void,_:*mut c_void,callback:*mut FileDialogCallback)->i32{if !callback.is_null(){if let Some(cancel)=unsafe {(*callback).cancel}{unsafe {cancel(callback)};}}1}
unsafe extern "system" fn context_before(_:*mut ContextMenuHandler,_:*mut Browser,_:*mut Frame,_:*mut c_void,model:*mut MenuModel){if !model.is_null(){if let Some(clear)=unsafe {(*model).clear}{unsafe {clear(model)};}}}
unsafe extern "system" fn context_run(_:*mut ContextMenuHandler,_:*mut Browser,_:*mut Frame,_:*mut c_void,_:*mut MenuModel,callback:*mut RunContextMenuCallback)->i32{if !callback.is_null(){if let Some(cancel)=unsafe {(*callback).cancel}{unsafe {cancel(callback)};}}1}
unsafe extern "system" fn can_download(_:*mut DownloadHandler,_:*mut Browser,_:*const CefString,_:*const CefString)->i32{0}
unsafe extern "system" fn before_download(_:*mut DownloadHandler,_:*mut Browser,_:*mut c_void,_:*const CefString,_:*mut c_void)->i32{0}
unsafe extern "system" fn download_update(_:*mut DownloadHandler,_:*mut Browser,_:*mut c_void,callback:*mut DownloadItemCallback){if !callback.is_null(){if let Some(cancel)=unsafe {(*callback).cancel}{unsafe {cancel(callback)};}}}
unsafe extern "system" fn resource_handler(p:*mut RequestHandler,_:*mut Browser,_:*mut Frame,_:*mut c_void,_:i32,_:i32,_:*const CefString,_:*mut i32)->*mut ResourceRequestHandler{let s=unsafe {state(p)};let Some(handlers)=s.handlers.get().and_then(Weak::upgrade)else{return ptr::null_mut();};let p=handlers.pointers[8] as *mut Base;if let Some(add)=unsafe {(*p).add_ref}{unsafe {add(p)};}p.cast()}
unsafe extern "system" fn external_tab(_:*mut RequestHandler,_:*mut Browser,_:*mut Frame,_:*const CefString,_:i32,_:i32)->i32{1}
unsafe extern "system" fn protocol(_:*mut ResourceRequestHandler,_:*mut Browser,_:*mut Frame,_:*mut c_void,allow:*mut i32){if !allow.is_null(){unsafe {*allow=0};}}
unsafe extern "system" fn media_permission(_:*mut PermissionHandler,_:*mut Browser,_:*mut Frame,_:*const CefString,_:u32,callback:*mut MediaAccessCallback)->i32{if !callback.is_null(){if let Some(cancel)=unsafe {(*callback).cancel}{unsafe {cancel(callback)};}}1}
unsafe extern "system" fn permission_prompt(_:*mut PermissionHandler,_:*mut Browser,_:u64,_:*const CefString,_:u32,callback:*mut PermissionPromptCallback)->i32{if !callback.is_null(){if let Some(cont)=unsafe {(*callback).cont}{unsafe {cont(callback,1)};}}1}
fn client(s:Arc<State>)->*mut Client {
 let mut render_api:RenderHandler=zero();render_api.get_view_rect=Some(rect);render_api.get_screen_info=Some(screen);render_api.on_popup_show=Some(popup_show);render_api.on_popup_size=Some(popup_size);render_api.on_paint=Some(paint);
 let mut life_api:LifeSpanHandler=zero();life_api.on_before_popup=Some(popup);life_api.on_before_close=Some(before_close);
 let mut load_api:LoadHandler=zero();load_api.on_load_end=Some(loaded);load_api.on_load_error=Some(load_error);
 let mut js_api:JsdialogHandler=zero();js_api.on_jsdialog=Some(jsdialog);js_api.on_before_unload_dialog=Some(unload);
 let mut dialog_api:DialogHandler=zero();dialog_api.on_file_dialog=Some(file_dialog);
 let mut ctx_api:ContextMenuHandler=zero();ctx_api.on_before_context_menu=Some(context_before);ctx_api.run_context_menu=Some(context_run);
 let mut download_api:DownloadHandler=zero();download_api.can_download=Some(can_download);download_api.on_before_download=Some(before_download);download_api.on_download_updated=Some(download_update);
 let mut request_api:RequestHandler=zero();request_api.get_resource_request_handler=Some(resource_handler);request_api.on_open_urlfrom_tab=Some(external_tab);
 let mut resource_api:ResourceRequestHandler=zero();resource_api.on_protocol_execution=Some(protocol);
 let mut permission_api:PermissionHandler=zero();permission_api.on_request_media_access_permission=Some(media_permission);permission_api.on_show_permission_prompt=Some(permission_prompt);
 let mut audio_api:AudioHandler=zero();audio_api.get_audio_parameters=Some(audio_parameters);audio_api.on_audio_stream_started=Some(audio_started);audio_api.on_audio_stream_packet=Some(audio_packet);audio_api.on_audio_stream_stopped=Some(audio_stopped);audio_api.on_audio_stream_error=Some(audio_error);
 let handlers=Arc::new(Handlers {pointers:[object(render_api,s.clone(),None) as usize,object(life_api,s.clone(),None) as usize,object(load_api,s.clone(),None) as usize,object(js_api,s.clone(),None) as usize,object(dialog_api,s.clone(),None) as usize,object(ctx_api,s.clone(),None) as usize,object(download_api,s.clone(),None) as usize,object(request_api,s.clone(),None) as usize,object(resource_api,s.clone(),None) as usize,object(permission_api,s.clone(),None) as usize,object(audio_api,s.clone(),None) as usize]});let _=s.handlers.set(Arc::downgrade(&handlers));
 let mut api:Client=zero();api.get_audio_handler=Some(audio_handler);api.get_render_handler=Some(render);api.get_life_span_handler=Some(life);api.get_load_handler=Some(load);api.get_jsdialog_handler=Some(js);api.get_dialog_handler=Some(dialog);api.get_context_menu_handler=Some(context);api.get_download_handler=Some(download);api.get_request_handler=Some(request);api.get_permission_handler=Some(permission);object(api,s,Some(handlers))
}
struct Node {browser:*mut Browser,state:Arc<State>,closing:bool,visible:bool,native_serial:u64,native_receipt:Option<(u64,bool)>,
 keys:[Option<KeyEvent>;256],buttons:[bool;3],mouse:MouseEvent}
impl Node {
 fn host(&self)->*mut BrowserHost{unsafe {(*self.browser).get_host}.map(|f|unsafe {f(self.browser)}).unwrap_or(ptr::null_mut())}
 fn hide(&mut self,visible:bool){let host=self.host();if !visible&&!host.is_null(){self.release_input(host);}if !host.is_null(){if let Some(hidden)=unsafe {(*host).was_hidden}{unsafe {hidden(host,!visible as i32)};}unsafe {release(host)};}self.state.visible.store(visible,Ordering::Release);self.visible=visible;}
 fn flush_native_receipt(&mut self){if let Some((serial,muted))=self.native_receipt{if self.state.events.try_send(wire::Event::NativeAudio{session:self.state.session,source:self.state.source.id,lease:self.state.source.lease,serial,muted}).is_ok(){self.native_receipt=None;}}}
 fn native_audio(&mut self,serial:u64,allowed:bool)->Result<(),String>{if serial<=self.native_serial{self.flush_native_receipt();return Ok(());}let host=self.host();if host.is_null(){return Err("CEF native playback has no host".into());}let result=(||{let mute=unsafe{(*host).set_audio_muted}.ok_or("CEF native mute API missing")?;let read=unsafe{(*host).is_audio_muted}.ok_or("CEF native mute readback API missing")?;let desired=self.state.source.control_audio||!allowed;unsafe{mute(host,desired as i32)};let muted=unsafe{read(host)}!=0;if muted!=desired{return Err("CEF native playback mute readback differs".into());}self.native_serial=serial;self.native_receipt=Some((serial,muted));self.flush_native_receipt();Ok(())})();unsafe{release(host)};result}
 fn grant(&self,grant:Option<wire::AudioGrant>){if let Ok(mut audio)=self.state.audio.try_lock(){if audio.grant!=grant{audio.queue.clear();audio.grant=grant;self.state.audio_dropped.store(0,Ordering::Release);}}}
 fn close(&mut self){if self.closing{return;}let host=self.host();if !host.is_null(){self.release_input(host);}self.closing=true;if !host.is_null(){if let Some(mute)=unsafe{(*host).set_audio_muted}{unsafe{mute(host,1)}}if let Some(close)=unsafe {(*host).close_browser}{unsafe {close(host,1)};}unsafe {release(host)};}}
 // All calls remain on the CEF owner thread. A single Focus(false) resets
 // every admitted key/button, avoiding an unbounded burst on the16-slot IPC.
 fn release_input(&mut self,host:*mut BrowserHost){
  if host.is_null(){return;}
  if let Some(send)=unsafe {(*host).send_key_event}{
   for old in &mut self.keys {if let Some(mut key)=old.take(){key.kind=2;key.modifiers=0;key.character=0;key.unmodified_character=0;unsafe {send(host,&key)};}}
  }else{self.keys.fill(None);}
  if let Some(send)=unsafe {(*host).send_mouse_click_event}{for (button,down) in self.buttons.iter_mut().enumerate(){if *down{unsafe {send(host,&MouseEvent {modifiers:0,..self.mouse},button as i32,1,1)};*down=false;}}}else{self.buttons=[false;3];}
  if let Some(focus)=unsafe {(*host).set_focus}{unsafe {focus(host,0)};}
 }
 fn input(&mut self,event:&wire::Input){
  if self.closing{return;}let host=self.host();if host.is_null(){return;}
  unsafe {match event {
   wire::Input::Move{x,y,modifiers,leave}=>{self.mouse=MouseEvent{x:*x,y:*y,modifiers:*modifiers};if let Some(send)=(*host).send_mouse_move_event {send(host,&self.mouse,*leave as i32);}},
   wire::Input::Click{x,y,modifiers,button,up,count}=>if (0..=2).contains(button)&&(1..=3).contains(count){self.mouse=MouseEvent{x:*x,y:*y,modifiers:*modifiers};if let Some(send)=(*host).send_mouse_click_event {send(host,&self.mouse,*button,*up as i32,*count);self.buttons[*button as usize]=!*up;}},
   wire::Input::Wheel{x,y,modifiers,dx,dy}=>{self.mouse=MouseEvent{x:*x,y:*y,modifiers:*modifiers};if let Some(send)=(*host).send_mouse_wheel_event {send(host,&self.mouse,*dx,*dy);}},
   wire::Input::Key{kind,modifiers,virtual_key,native_key,character}=>if (0..=3).contains(kind){if let Some(send)=(*host).send_key_event {
    let key=KeyEvent {kind:*kind,modifiers:*modifiers,windows_key_code:*virtual_key,native_key_code:*native_key,character:*character,unmodified_character:*character,is_system_key:i32::from(modifiers&8!=0),..Default::default()};send(host,&key);
    if (0..=255).contains(virtual_key){if *kind==0||*kind==1{self.keys[*virtual_key as usize]=Some(key);}else if *kind==2{self.keys[*virtual_key as usize]=None;}}
   }},
   wire::Input::Focus(true)=>if let Some(focus)=(*host).set_focus{focus(host,1);},
   wire::Input::Focus(false)=>self.release_input(host),
  }};
  unsafe {release(host)};
 }

}
impl Drop for Node {fn drop(&mut self){unsafe {release(self.browser)};}}
fn create(api:&Api,source:wire::Source,session:u64,events:mpsc::SyncSender<wire::Event>,budget:Arc<Budget>,audio_budget:Arc<Budget>,frequency:u64)->Result<Node,String>{
 source.validate().map_err(|e|e.to_string())?;let state=Arc::new(State {audio:Mutex::new(AudioState{grant:source.audio_grant,queue:VecDeque::new()}),audio_running:AtomicBool::new(false),audio_generation:AtomicU64::new(0),audio_sequence:AtomicU64::new(0),audio_dropped:AtomicU64::new(0),audio_budget,visible:AtomicBool::new(source.visible),source,session,closed:AtomicBool::new(false),ready:AtomicBool::new(false),surface:Mutex::new(Surface::default()),budget_reported:AtomicBool::new(false),last_paint:Mutex::new(None),paint:Mutex::new(None),events,budget,handlers:OnceLock::new(),frequency,string_free:api.string_free});
 let c=client(state.clone());let mut window:WindowInfo=zero();window.windowless_rendering_enabled=1;let mut settings:BrowserSettings=zero();settings.size=size_of::<BrowserSettings>();settings.windowless_frame_rate=state.source.fps as i32;settings.background_color=0;let mut initial_url=Wide::new("about:blank");
 let browser=unsafe {(api.create)(&window,c,&initial_url.value(),&settings,ptr::null_mut(),ptr::null_mut())};unsafe {release(c)};if browser.is_null(){return Err("CEF browser creation failed".into());}
 let mut node=Node {browser,state,closing:false,visible:true,native_serial:0,native_receipt:None,keys:[None;256],buttons:[false;3],mouse:Default::default()};let host=node.host();if host.is_null(){eprintln!("CEF created a browser without a host; terminating the owned helper");std::process::exit(3);}let Some(mute)= (unsafe{(*host).set_audio_muted})else{eprintln!("CEF native mute API missing before navigation");std::process::exit(3);};unsafe{mute(host,1);release(host)};
 if let Err(error)=node.native_audio(node.state.source.native_serial,node.state.source.native_audio){eprintln!("CEF initial native playback: {error}");std::process::exit(3);}
 let frame=unsafe{(*browser).get_main_frame}.map(|get|unsafe{get(browser)}).unwrap_or(ptr::null_mut());if frame.is_null(){eprintln!("CEF source has no main frame before navigation");std::process::exit(3);}let Some(load)= (unsafe{(*frame).load_url})else{eprintln!("CEF source main-frame LoadURL unavailable");std::process::exit(3);};let mut url=Wide::new(&node.state.source.url);unsafe{load(frame,&url.value());release(frame)};
 node.state.event(1,String::new());Ok(node)
}
pub fn run()->Result<(),String>{
 let exe=std::env::current_exe().map_err(|e|e.to_string())?;let root=exe.parent().ok_or("browser helper directory")?;let api=Api::load(root)?;let args=MainArgs {instance:unsafe {GetModuleHandleW(None)}.map_err(|e|e.to_string())?.0};let cli:Vec<String>=std::env::args().collect();
 if cli.iter().any(|arg|arg.starts_with("--type=")){let child=unsafe {(api.execute)(&args,ptr::null_mut(),ptr::null_mut())};std::process::exit(child.max(0));}
if !cli.iter().any(|v|v=="--skylive-browser-owner"){return Err("This executable is the private SkyLive CEF OSR helper".into());}
 if !cli.iter().any(|v|v=="--skylive-audio-routing=v2"){return Err("Browser helper requires explicit source audio routing protocolv2".into());}
 // Parent assigns the kill-on-close process Job before admitting Boot. No
 // CEF initialization/subprocess creation is allowed before that barrier.
 if !matches!(wire::read_command(&mut io::stdin().lock()).map_err(|e|e.to_string())?,wire::Command::Boot){return Err("missing browser owner boot barrier".into());}
 let child=unsafe {(api.execute)(&args,ptr::null_mut(),ptr::null_mut())};if child>=0{std::process::exit(child);}
 let cache=cli.iter().find_map(|v|v.strip_prefix("--skylive-cache=")).ok_or("missing unique browser cache directory")?;
 let mut settings:Settings=zero();settings.size=size_of::<Settings>();settings.no_sandbox=1;settings.windowless_rendering_enabled=1;settings.log_severity=99;
 let mut helper_path=Wide::new(&exe.to_string_lossy());let mut resource_path=Wide::new(&root.to_string_lossy());let mut locales_path=Wide::new(&root.join("locales").to_string_lossy());let mut cache_path=Wide::new(cache);
 settings.browser_subprocess_path=helper_path.value();settings.resources_dir_path=resource_path.value();settings.locales_dir_path=locales_path.value();settings.root_cache_path=cache_path.value();
 let context_initialized=Arc::new(AtomicBool::new(false));let app=application(context_initialized.clone());
 if unsafe {(api.initialize)(&args,&settings,app.0.cast(),ptr::null_mut())}==0{return Err("CEF initialization failed".into());}
 let mut frequency=0i64;unsafe {QueryPerformanceFrequency(&mut frequency)}.map_err(|e|e.to_string())?;let frequency=u64::try_from(frequency).ok().filter(|v|*v>0).ok_or("invalid QPC frequency")?;
 let (commands,input)=mpsc::sync_channel(8);let reader=thread::Builder::new().name("skylive-cef-command-reader".into()).spawn(move||{let mut stdin=io::stdin().lock();while let Ok(command)=wire::read_command(&mut stdin){let end=matches!(&command,wire::Command::Shutdown);if commands.send(command).is_err()||end{return;}}let _=commands.send(wire::Command::Shutdown);}).map_err(|e|e.to_string())?;
 let (events,output)=mpsc::sync_channel(8);let states:Arc<Mutex<Vec<Weak<State>>>>=Arc::new(Mutex::new(Vec::new()));let registry=states.clone();let writer=thread::Builder::new().name("skylive-cef-paint-writer".into()).spawn(move||{let mut out=io::stdout().lock();if wire::write_event(&mut out,&wire::Event::Hello).is_err(){return;}loop {for _ in 0..8{let Ok(event)=output.try_recv()else{break;};let stop=matches!(&event,wire::Event::Stopped);if wire::write_event(&mut out,&event).is_err()||stop{return;}}let list=registry.lock().map(|s|s.iter().filter_map(Weak::upgrade).collect::<Vec<_>>()).unwrap_or_default();for state in list {for _ in 0..4{let pcm=state.audio.try_lock().ok().and_then(|mut a|a.queue.pop_front());if let Some(pcm)=pcm{if wire::write_event(&mut out,&pcm.event).is_err(){return;}}else{break;}}let frame=state.paint.lock().ok().and_then(|mut p|p.take());if let Some(frame)=frame {if wire::write_event(&mut out,&frame.event).is_err(){return;}}}thread::sleep(Duration::from_millis(2));}}).map_err(|e|e.to_string())?;
 let budget=Arc::new(Budget {bytes:AtomicUsize::new(0),count:AtomicUsize::new(0),max_bytes:wire::PIXEL_BUDGET,max_count:wire::MAX_FRAMES});let audio_budget=Arc::new(Budget{bytes:AtomicUsize::new(0),count:AtomicUsize::new(0),max_bytes:wire::PCM_BUDGET,max_count:wire::MAX_PCM_PACKETS});let mut desired=HashMap::<u64,wire::Source>::new();let mut session=0u64;let mut sync_epoch=0u64;let mut nodes=Vec::<Node>::new();let mut retry=HashMap::<u64,Instant>::new();let mut stopping=false;let initialized_at=Instant::now();
 loop {
  for _ in 0..8 {match input.try_recv(){Ok(wire::Command::Boot)=>{},Ok(wire::Command::Sync {session:next,epoch,sources})=>{if stopping||next==session&&epoch<=sync_epoch{continue;}sync_epoch=epoch;session=next;desired=sources.into_iter().map(|s|(s.id,s)).collect();retry.retain(|id,_|desired.contains_key(id));},Ok(wire::Command::Input {session:input_session,source,lease,event})=>{if input_session==session {if let Some(node)=nodes.iter_mut().find(|n|n.state.source.id==source&&n.state.source.lease==lease&&n.state.session==session){node.input(&event);}}},Ok(wire::Command::Shutdown)=>{stopping=true;desired.clear();},Err(mpsc::TryRecvError::Empty)=>break,Err(_)=>{stopping=true;desired.clear();break;}}}
  for node in &mut nodes {let wanted=desired.get(&node.state.source.id).filter(|s|node.state.session==session&&s.lease==node.state.source.lease);if let Some(wanted)=wanted {node.grant(wanted.audio_grant);if let Err(error)=node.native_audio(wanted.native_serial,wanted.native_audio){node.state.event(3,error);node.close();}if node.visible!=wanted.visible {node.hide(wanted.visible);}}else{node.close();}}
  unsafe {(api.pump)()};for node in &nodes {node.state.flush_surface();}nodes.retain(|node|!node.state.closed.load(Ordering::Acquire));
  if stopping&&nodes.is_empty(){break;}
  if !context_initialized.load(Ordering::Acquire){if initialized_at.elapsed()>Duration::from_secs(15){eprintln!("CEF context initialization did not acknowledge readiness");stopping=true;desired.clear();}thread::sleep(Duration::from_millis(5));continue;}
  let mut wanted:Vec<_>=desired.values().cloned().collect();wanted.sort_by_key(|s|s.id);for source in wanted {if nodes.len()>=wire::MAX_SOURCES{break;}if nodes.iter().any(|n|n.state.source.id==source.id)||retry.get(&source.id).is_some_and(|at|*at>Instant::now()){continue;}let id=source.id;let lease=source.lease;match create(&api,source,session,events.clone(),budget.clone(),audio_budget.clone(),frequency){Ok(mut node)=>{node.hide(node.state.source.visible);nodes.push(node);},Err(error)=>{retry.insert(id,Instant::now()+Duration::from_secs(1));let _=events.try_send(wire::Event::State {session,source:id,lease,kind:3,message:error});}}}
  if let Ok(mut registry)=states.lock(){registry.clear();registry.extend(nodes.iter().map(|node|Arc::downgrade(&node.state)));}
  thread::sleep(Duration::from_millis(5));
 }
 unsafe {(api.shutdown)()};let _=events.send(wire::Event::Stopped);drop(events);let _=writer.join();let _=reader.join();Ok(())
}

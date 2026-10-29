//! One coalescing CPU thumbnail owner for the real Browser Interaction window.
//! No GUI/CEF/device calls. Raw inputs retain their Browser owner's pixel credit.
use super::{BrowserError,BrowserFrame};
use skylive_core::Id;
use skylive_media::RgbaFrame;
use std::sync::{Arc,Mutex,mpsc,atomic::{AtomicBool,Ordering}};
pub const MAX_PREVIEW_EDGE:u32=1024;
pub const MAX_PREVIEW_PIXELS:u64=786432;
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub struct Key {pub window:u64,pub view:u64,pub session:u64,pub source:Id,pub instance:u64,pub native_epoch:u64,pub lease:u64,pub binding_lease:u64,pub sequence:u64,pub width:u32,pub height:u32}
struct Request {key:Key,image:Arc<RgbaFrame>}
pub struct Reply {pub key:Key,pub image:Result<RgbaFrame,BrowserError>}
pub struct Preview {pending:Arc<Mutex<Option<Request>>>,closed:Arc<AtomicBool>,replies:mpsc::Receiver<Reply>}
impl Preview {
 pub fn new()->std::io::Result<Self>{
  let pending=Arc::new(Mutex::new(None::<Request>));let closed=Arc::new(AtomicBool::new(false));
  let (send,replies)=mpsc::sync_channel(1);let (wanted,stop)=(pending.clone(),closed.clone());
  std::thread::Builder::new().name("browser-interaction-preview".into()).spawn(move||{
   let mut reply=None;
   while !stop.load(Ordering::Acquire){
    if let Some(request)=wanted.lock().ok().and_then(|mut slot|slot.take()) {
     let image=resize(&request.image,request.key.width,request.key.height,&stop);
     if stop.load(Ordering::Acquire){break;}reply=Some(Reply {key:request.key,image});
    }
    if let Some(value)=reply.take(){match send.try_send(value){Ok(())=>{},Err(mpsc::TrySendError::Full(value))=>reply=Some(value),Err(mpsc::TrySendError::Disconnected(_))=>break}}
    std::thread::sleep(std::time::Duration::from_millis(4));
   }
  })?;
  Ok(Self {pending,closed,replies})
 }
 pub fn submit(&self,key:Key,frame:&BrowserFrame)->Result<(),BrowserError>{
  if self.closed.load(Ordering::Acquire){return Err(BrowserError::Closed);}
  if key.window==0||key.view==0||key.session==0||key.instance==0||key.native_epoch==0||key.lease==0||key.binding_lease==0||key.sequence==0||frame.image.width()!=frame.browser.width()||frame.image.height()!=frame.browser.height()||key.lease!=frame.lease||key.binding_lease!=frame.binding_lease||key.sequence!=frame.sequence||key.width==0||key.height==0||key.width>MAX_PREVIEW_EDGE||key.height>MAX_PREVIEW_EDGE||u64::from(key.width)*u64::from(key.height)>MAX_PREVIEW_PIXELS{return Err(BrowserError::Invalid("browser interaction preview dimensions/lease"));}
  *self.pending.try_lock().map_err(|_|BrowserError::Busy)?=Some(Request {key,image:frame.image.clone()});Ok(())
 }
 pub fn poll(&self)->Option<Reply>{self.replies.try_recv().ok()}
 pub fn close(&self){self.closed.store(true,Ordering::Release);if let Ok(mut slot)=self.pending.try_lock(){*slot=None;}}
}
impl Drop for Preview {fn drop(&mut self){self.close();}}
fn resize(input:&RgbaFrame,width:u32,height:u32,closed:&AtomicBool)->Result<RgbaFrame,BrowserError>{
 if width==0||height==0||width>MAX_PREVIEW_EDGE||height>MAX_PREVIEW_EDGE||u64::from(width)*u64::from(height)>MAX_PREVIEW_PIXELS{return Err(BrowserError::Invalid("browser thumbnail output limit"));}
 let bytes=(width as usize).checked_mul(height as usize).and_then(|n|n.checked_mul(4)).ok_or(BrowserError::Invalid("browser thumbnail byte overflow"))?;
 let mut output=Vec::new();output.try_reserve_exact(bytes).map_err(|_|BrowserError::Unavailable("browser thumbnail allocation failed"))?;output.resize(bytes,0);
 for y in 0..height {
  if closed.load(Ordering::Acquire){return Err(BrowserError::Closed);}
  let py=((f64::from(y)+0.5)*f64::from(input.height())/f64::from(height)-0.5).clamp(0.0,f64::from(input.height()-1));
  let y0=py.floor()as u32;let y1=(y0+1).min(input.height()-1);let fy=py-f64::from(y0);
  for x in 0..width {
   let px=((f64::from(x)+0.5)*f64::from(input.width())/f64::from(width)-0.5).clamp(0.0,f64::from(input.width()-1));
   let x0=px.floor()as u32;let x1=(x0+1).min(input.width()-1);let fx=px-f64::from(x0);
   let mut alpha=0.0;let mut color=[0.0;3];
   for (sx,sy,weight) in [(x0,y0,(1.0-fx)*(1.0-fy)),(x1,y0,fx*(1.0-fy)),(x0,y1,(1.0-fx)*fy),(x1,y1,fx*fy)] {
    let i=(sy as usize*input.width()as usize+sx as usize)*4;let a=f64::from(input.pixels()[i+3]);alpha+=a*weight;
    for c in 0..3 {color[c]+=f64::from(input.pixels()[i+c])*a*weight;}
   }
   let i=(y as usize*width as usize+x as usize)*4;output[i+3]=alpha.round().clamp(0.0,255.0)as u8;
   for c in 0..3 {output[i+c]=if alpha>0.0 {(color[c]/alpha).round().clamp(0.0,255.0)as u8}else{0};}
  }
 }
 RgbaFrame::new(width,height,output).map_err(|_|BrowserError::Invalid("browser thumbnail RGBA layout"))
}

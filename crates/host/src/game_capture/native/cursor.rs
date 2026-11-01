//! Foreground system cursor composited on a separate bounded CPU game frame.
//! The shared hook frame is never modified; sources with cursor off keep it.
use super::{GameCaptureTarget,GameError,RgbaFrame};
use std::{ffi::c_void,mem::size_of,ptr,sync::OnceLock,time::{Duration,Instant}};
use windows::Win32::{
 Foundation::{HANDLE,HWND,POINT,RECT},
 Graphics::Gdi::{BITMAP,BITMAPINFO,BITMAPINFOHEADER,BI_RGB,ClientToScreen,CreateCompatibleDC,CreateDIBSection,DIB_RGB_COLORS,DeleteDC,DeleteObject,GdiFlush,GetObjectW,HBITMAP,HBRUSH,HDC,HGDIOBJ,SelectObject},
 UI::{HiDpi::{DPI_AWARENESS_CONTEXT,DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE,SetThreadDpiAwarenessContext},WindowsAndMessaging::{CURSORINFO,CURSOR_SHOWING,CopyIcon,DestroyIcon,DrawIconEx,DI_NORMAL,GetClientRect,GetCursorInfo,GetForegroundWindow,GetIconInfo,GetWindowThreadProcessId,HICON,ICONINFO}},
};

struct Dpi(DPI_AWARENESS_CONTEXT);
impl Drop for Dpi{fn drop(&mut self){unsafe{let _=SetThreadDpiAwarenessContext(self.0);}}}
struct Icon(HICON);
impl Drop for Icon{fn drop(&mut self){unsafe{let _=DestroyIcon(self.0);}}}
struct IconBits(ICONINFO);
impl Drop for IconBits{fn drop(&mut self){unsafe{if !self.0.hbmColor.is_invalid(){let _=DeleteObject(self.0.hbmColor);}if !self.0.hbmMask.is_invalid(){let _=DeleteObject(self.0.hbmMask);}}}}
struct Dib{dc:HDC,bitmap:HBITMAP,old:HGDIOBJ,bits:*mut c_void}
impl Drop for Dib{fn drop(&mut self){unsafe{if !self.old.is_invalid()&&self.old.0 as isize != -1{let _=SelectObject(self.dc,self.old);}if !self.bitmap.is_invalid(){let _=DeleteObject(self.bitmap);}if !self.dc.is_invalid(){let _=DeleteDC(self.dc);}}}}
fn err(label:&str)->GameError{GameError::Native(format!("{label}: {}",windows::core::Error::from_win32()))}

/// Cursor state is independent of the game's presentation sequence. Include
/// window geometry because moving/resizing a window changes overlay mapping.
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub(super) struct Stamp { foreground:usize, icon:usize, flags:u32, x:i32, y:i32, origin_x:i32, origin_y:i32, width:i32, height:i32, step:u32 }
pub(super) fn stamp(target:&GameCaptureTarget)->Result<Stamp,GameError>{
 let _dpi=Dpi(unsafe{SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE)});
 let foreground=unsafe{GetForegroundWindow()};let hwnd=HWND(target.hwnd as usize as *mut c_void);
 let mut cursor=CURSORINFO{cbSize:size_of::<CURSORINFO>() as u32,..Default::default()};
 unsafe{GetCursorInfo(&mut cursor).map_err(|_|err("GetCursorInfo"))?;}
 let mut rect=RECT::default();unsafe{GetClientRect(hwnd,&mut rect).map_err(|_|err("GetClientRect"))?;}
 let mut origin=POINT{x:0,y:0};if !unsafe{ClientToScreen(hwnd,&mut origin)}.as_bool(){return Err(err("ClientToScreen"));}
 Ok(Stamp{foreground:foreground.0 as usize,icon:cursor.hCursor.0 as usize,flags:cursor.flags.0,x:cursor.ptScreenPos.x,y:cursor.ptScreenPos.y,origin_x:origin.x,origin_y:origin.y,width:rect.right-rect.left,height:rect.bottom-rect.top,step:0})
}

// Windows does not document a way to query the displayed animation phase.
// Resolve this optional user32 export dynamically: if absent or invalid, keep
// the normal static-cursor behavior. The returned handle is borrowed.
type FrameInfo=unsafe extern "system" fn(*mut c_void,*mut c_void,u32,*mut u32,*mut u32)->*mut c_void;
fn frame_info()->Option<FrameInfo>{
 static SYMBOL:OnceLock<Option<FrameInfo>>=OnceLock::new();
 *SYMBOL.get_or_init(||{
  let module:Vec<u16>="user32.dll\0".encode_utf16().collect();
  let handle=unsafe{super::target::GetModuleHandleW(module.as_ptr())};
  if handle.is_null(){return None;}
  let address=unsafe{super::target::GetProcAddress(handle,b"GetCursorFrameInfo\0".as_ptr())};
  if address.is_null(){None}else{Some(unsafe{std::mem::transmute::<*mut c_void,FrameInfo>(address)})}
 })
}
fn frame(icon:usize,step:u32)->Option<(*mut c_void,u32,u32)>{
 let function=frame_info()?;let(mut rate,mut count)=(0u32,0u32);
 let handle=unsafe{function(icon as *mut c_void,ptr::null_mut(),step,&mut rate,&mut count)};
 if handle.is_null()||count==0||count>256||step>=count{return None;}
 Some((handle,rate,count))
}

/// Independent per-stream animation clock. Windows exposes frame delays in
/// 1/60-second jiffies, not the currently displayed system phase. Advancing at
/// most once per worker tick avoids unbounded catch-up on a suspended host.
#[derive(Default)]
pub(super) struct Animation { icon:usize,step:u32,count:u32,probed:bool,deadline:Option<Instant> }
impl Animation {
 pub(super) fn update(&mut self,stamp:&mut Stamp,now:Instant){
  if self.icon!=stamp.icon {self.icon=stamp.icon;self.step=0;self.count=0;self.probed=false;self.deadline=None;}
  if stamp.icon==0 || stamp.flags&CURSOR_SHOWING.0==0 {return;}
  if !self.probed {
   self.probed=true;
   if let Some((_,rate,count))=frame(stamp.icon,0){
    self.count=count;
    if count>1 {self.deadline=Some(now+delay(rate));}
   }
  } else if self.deadline.is_some_and(|at|now>=at) {
   let next=(self.step+1)%self.count;
   if let Some((_,rate,count))=frame(stamp.icon,next){self.step=next;self.count=count;self.deadline=Some(now+delay(rate));}
   else {self.step=0;self.count=0;self.deadline=None;}
  }
  stamp.step=self.step;
 }
}
fn delay(jiffies:u32)->Duration {Duration::from_millis((u64::from(jiffies.clamp(1,600))*1000/60).max(16))}

/// Return None when there is no visible cursor in this game. Scratch DIB and
/// output allocation are both admitted by the caller before this function.
pub(super) fn overlay(target:&GameCaptureTarget,image:&RgbaFrame,free:usize,stamp:Stamp,transparent:bool)->Result<Option<RgbaFrame>,GameError>{
 let _dpi=Dpi(unsafe{SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE)});
 let foreground=unsafe{GetForegroundWindow()};let mut pid=0u32;unsafe{GetWindowThreadProcessId(foreground,Some(&mut pid));}
 if pid!=target.pid{return Ok(None);}
 let hwnd=HWND(target.hwnd as usize as *mut c_void);let mut rect=RECT::default();
 unsafe{GetClientRect(hwnd,&mut rect).map_err(|_|err("GetClientRect"))?;}
 let (w,h)=(rect.right-rect.left,rect.bottom-rect.top);if w<=0||h<=0{return Ok(None);}
 let mut origin=POINT{x:0,y:0};if !unsafe{ClientToScreen(hwnd,&mut origin)}.as_bool(){return Err(err("ClientToScreen"));}
 let mut cursor=CURSORINFO{cbSize:size_of::<CURSORINFO>() as u32,..Default::default()};
 unsafe{GetCursorInfo(&mut cursor).map_err(|_|err("GetCursorInfo"))?;}
 if cursor.flags!=CURSOR_SHOWING{return Ok(None);}
 // Obtain the selected frame's own dimensions and hotspot, not only the
 // animation's first frame. A changed cursor between stamp and draw falls
 // back to the newly observed icon, without applying a stale step to it.
 let actual=cursor.hCursor.0 as usize;
 let borrowed=if actual==stamp.icon {frame(actual,stamp.step).map(|v|v.0).unwrap_or(cursor.hCursor.0)} else {cursor.hCursor.0};
 let icon=Icon(unsafe{CopyIcon(HICON(borrowed))}.map_err(|_|err("CopyIcon"))?);
 let mut bits=IconBits(ICONINFO::default());unsafe{GetIconInfo(icon.0,&mut bits.0).map_err(|_|err("GetIconInfo"))?;}
 let handle=if bits.0.hbmColor.is_invalid(){bits.0.hbmMask}else{bits.0.hbmColor};
 let mut info=BITMAP::default();if unsafe{GetObjectW(HGDIOBJ(handle.0),size_of::<BITMAP>() as i32,Some((&mut info as *mut BITMAP).cast()))}==0{return Err(err("GetObjectW cursor bitmap"));}
 let iw=info.bmWidth;let ih=if bits.0.hbmColor.is_invalid(){info.bmHeight/2}else{info.bmHeight};
 if iw<=0||ih<=0||iw>1024||ih>1024{return Err(GameError::Limit("game cursor bitmap dimensions"));}
 let sx=f64::from(image.width())/f64::from(w);let sy=f64::from(image.height())/f64::from(h);
 let x=((i64::from(cursor.ptScreenPos.x)-i64::from(origin.x)-i64::from(bits.0.xHotspot)) as f64*sx).round();
 let y=((i64::from(cursor.ptScreenPos.y)-i64::from(origin.y)-i64::from(bits.0.yHotspot)) as f64*sy).round();
 let cw=(f64::from(iw)*sx).round().clamp(1.0,8192.0) as i32;let ch=(f64::from(ih)*sy).round().clamp(1.0,8192.0) as i32;
 if x>=f64::from(image.width())||y>=f64::from(image.height())||x+f64::from(cw)<=0.0||y+f64::from(ch)<=0.0{return Ok(None);}
 let left=(x.max(0.0) as u32).min(image.width());let top=(y.max(0.0) as u32).min(image.height());
 let right=((x+f64::from(cw)).min(f64::from(image.width())) as u32).max(left);let bottom=((y+f64::from(ch)).min(f64::from(image.height())) as u32).max(top);
 let (rw,rh)=(right-left,bottom-top);let scratch=(rw as usize)*(rh as usize)*4;
 let bytes=image.byte_len();let scratch_total=if transparent {scratch.checked_mul(2)} else {Some(scratch)};
 if scratch_total.and_then(|n|bytes.checked_add(n)).is_none_or(|n|n>free){return Err(GameError::Limit("game cursor overlay copy budget"));}
 let dc=unsafe{CreateCompatibleDC(HDC::default())};if dc.is_invalid(){return Err(err("CreateCompatibleDC"));}
 let mut dib=Dib{dc,bitmap:HBITMAP::default(),old:HGDIOBJ::default(),bits:ptr::null_mut()};
 let desc=BITMAPINFO{bmiHeader:BITMAPINFOHEADER{biSize:size_of::<BITMAPINFOHEADER>() as u32,biWidth:rw as i32,biHeight:-(rh as i32),biPlanes:1,biBitCount:32,biCompression:BI_RGB.0,..Default::default()},..Default::default()};
 dib.bitmap=unsafe{CreateDIBSection(dc,&desc,DIB_RGB_COLORS,&mut dib.bits,HANDLE::default(),0)}.map_err(|_|err("CreateDIBSection"))?;
 if dib.bits.is_null(){return Err(GameError::Native("CreateDIBSection returned no cursor pixels".into()));}
 dib.old=unsafe{SelectObject(dc,dib.bitmap)};if dib.old.is_invalid()||dib.old.0 as isize == -1{return Err(err("SelectObject"));}
 let pixels=unsafe{std::slice::from_raw_parts_mut(dib.bits.cast::<u8>(),scratch)};
 if transparent {
  // Render against black and white to recover coverage and premultiplied
  // cursor colour. A decreasing response identifies XOR pixels. For those,
  // black reveals the XOR mask and black XOR white reveals the AND mask.
  // Apply those masks per colour channel to the captured game, including
  // coloured XOR cursors rather than treating every channel as inverted.
  pixels.fill(0);
  unsafe{DrawIconEx(dc,(x-f64::from(left)) as i32,(y-f64::from(top)) as i32,icon.0,cw,ch,0,HBRUSH::default(),DI_NORMAL).map_err(|_|err("DrawIconEx black"))?;let _=GdiFlush();}
  let mut black=Vec::new();black.try_reserve_exact(scratch).map_err(|_|GameError::Limit("game cursor overlay allocation"))?;black.extend_from_slice(pixels);
  pixels.fill(255);
  unsafe{DrawIconEx(dc,(x-f64::from(left)) as i32,(y-f64::from(top)) as i32,icon.0,cw,ch,0,HBRUSH::default(),DI_NORMAL).map_err(|_|err("DrawIconEx white"))?;let _=GdiFlush();}
  let mut output=Vec::new();output.try_reserve_exact(bytes).map_err(|_|GameError::Limit("game cursor overlay allocation"))?;output.extend_from_slice(image.pixels());
  for row in 0..rh as usize {let base=((top as usize+row)*image.width() as usize+left as usize)*4;let start=row*rw as usize*4;
   for ((dark,light),dst) in black[start..start+rw as usize*4].chunks_exact(4).zip(pixels[start..start+rw as usize*4].chunks_exact(4)).zip(output[base..base+rw as usize*4].chunks_exact_mut(4)){
    if (0..3).any(|c|light[c]<dark[c]) {
     for c in 0..3 {
      let xor=dark[2-c];let and=xor^light[2-c];
      dst[c]=(dst[c]&and)^xor;
     }
     // XOR on a transparent game pixel is background-dependent and cannot
     // be expressed exactly as straight RGBA for later scene composition.
     dst[3]=255;continue;
    }
    let trans=((0..3).map(|c|u32::from(light[c]-dark[c])).sum::<u32>()+1)/3;
    let ca=255-trans;let da=u32::from(dst[3]);let oa=ca+(da*trans+127)/255;
    if oa==0 {dst.copy_from_slice(&[0,0,0,0]);continue;}
    for c in 0..3 {let src=u32::from(dark[2-c]);let premul=src*255+(u32::from(dst[c])*da*trans+127)/255;dst[c]=((premul+oa/2)/oa).min(255) as u8;}
    dst[3]=oa as u8;
   }
  }
  return RgbaFrame::new(image.width(),image.height(),output).map(Some).map_err(|_|GameError::Invalid("game cursor output"));
 }
 for row in 0..rh as usize {let base=((top as usize+row)*image.width() as usize+left as usize)*4;let start=row*rw as usize*4;
  for (src,dst) in image.pixels()[base..base+rw as usize*4].chunks_exact(4).zip(pixels[start..start+rw as usize*4].chunks_exact_mut(4)){dst.copy_from_slice(&[src[2],src[1],src[0],255]);}}
 unsafe{DrawIconEx(dc,(x-f64::from(left)).clamp(i32::MIN as f64,i32::MAX as f64) as i32,(y-f64::from(top)).clamp(i32::MIN as f64,i32::MAX as f64) as i32,icon.0,cw,ch,0,HBRUSH::default(),DI_NORMAL).map_err(|_|err("DrawIconEx"))?;let _=GdiFlush();}
 let mut output=Vec::new();output.try_reserve_exact(bytes).map_err(|_|GameError::Limit("game cursor overlay allocation"))?;output.extend_from_slice(image.pixels());
 for row in 0..rh as usize {let base=((top as usize+row)*image.width() as usize+left as usize)*4;let start=row*rw as usize*4;
  for (src,dst) in pixels[start..start+rw as usize*4].chunks_exact(4).zip(output[base..base+rw as usize*4].chunks_exact_mut(4)){dst.copy_from_slice(&[src[2],src[1],src[0],255]);}}
 RgbaFrame::new(image.width(),image.height(),output).map(Some).map_err(|_|GameError::Invalid("game cursor output"))
}

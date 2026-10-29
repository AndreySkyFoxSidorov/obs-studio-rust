//! Bounded CPU DXGI pointer composition; no GPU or OS calls in constructors.
//! DXGI position is bitmap top-left/output-relative. HotSpot is not subtracted.
use super::{CaptureError,CaptureLimits,native_budget::{NativeCopyBudget,NativeCopyKind,NativeCopyLease}};
use skylive_media::RgbaFrame;
use std::sync::Arc;
use windows::Win32::Graphics::Dxgi::{DXGI_OUTDUPL_FRAME_INFO,DXGI_OUTDUPL_POINTER_SHAPE_INFO,IDXGIOutputDuplication,DXGI_ERROR_MORE_DATA};
const MAX_SHAPE_BYTES:usize=4*1024*1024;
const MAX_CURSOR_EDGE:u32=1024;
struct Shape {bytes:Vec<u8>,info:DXGI_OUTDUPL_POINTER_SHAPE_INFO,height:u32,_credit:NativeCopyLease}
#[derive(Clone,Default)]
pub(super) struct Snapshot {shape:Option<Arc<Shape>>,position:[i32;2],visible:bool}
#[derive(Default)]
pub(super) struct Pointer {current:Snapshot,initialized:bool}
impl Pointer {
    pub(super) fn update(&mut self,duplication:&IDXGIOutputDuplication,frame:&DXGI_OUTDUPL_FRAME_INFO,budget:&NativeCopyBudget)->Result<Snapshot,CaptureError>{
        if !self.initialized || frame.LastMouseUpdateTime!=0 {
            self.current.visible=frame.PointerPosition.Visible.as_bool();
            if self.current.visible {self.current.position=[frame.PointerPosition.Position.x,frame.PointerPosition.Position.y];}
            self.initialized=true;
        }
        if frame.PointerShapeBufferSize!=0 || (self.current.visible&&self.current.shape.is_none()) {
            let mut size=frame.PointerShapeBufferSize as usize;
            if size==0 {
                let mut needed=0u32;let mut info=DXGI_OUTDUPL_POINTER_SHAPE_INFO::default();
                match unsafe{duplication.GetFramePointerShape(0,std::ptr::null_mut(),&mut needed,&mut info)}{
                    Err(error) if error.code()==DXGI_ERROR_MORE_DATA=>{},
                    Err(error)=>return Err(CaptureError::Backend(format!("DXGI cursor shape size: {error}"))),Ok(())=>{},
                }
                size=needed as usize;
            }
            // A shape may grow between the frame-info query and retrieval. One
            // bounded retry replaces the candidate allocation, not a FIFO.
            let mut installed=false;
            for _ in 0..2 {
                if size==0 || size>MAX_SHAPE_BYTES{return Err(CaptureError::BudgetExceeded("DXGI cursor shape4MiB"));}
                let credit=budget.reserve(NativeCopyKind::Dxgi,size).ok_or(CaptureError::BudgetExceeded("DXGI shape/native-copy budget"))?;
                let mut bytes=Vec::new();bytes.try_reserve_exact(size).map_err(|_|CaptureError::BudgetExceeded("DXGI cursor allocation"))?;bytes.resize(size,0);
                let mut needed=0u32;let mut info=DXGI_OUTDUPL_POINTER_SHAPE_INFO::default();
                match unsafe{duplication.GetFramePointerShape(size as u32,bytes.as_mut_ptr().cast(),&mut needed,&mut info)} {
                    Err(error) if error.code()==DXGI_ERROR_MORE_DATA=>{size=needed as usize;drop(bytes);drop(credit);continue;},
                    Err(error)=>return Err(CaptureError::Backend(format!("DXGI GetFramePointerShape: {error}"))),Ok(())=>{},
                }
                if needed==0 || needed as usize>bytes.len(){return Err(CaptureError::InvalidFrame("DXGI cursor shape exceeds returned allocation"));}
                // Validate the API's actual extent, not the zero-filled spare
                // capacity; the lease conservatively still covers allocation.
                bytes.truncate(needed as usize);
                let height=validate(&bytes,info)?;
                self.current.shape=Some(Arc::new(Shape {bytes,info,height,_credit:credit}));installed=true;break;
            }
            if !installed{return Err(CaptureError::Unavailable("DXGI cursor shape changed during bounded retrieval"));}
        }
        if self.current.visible&&self.current.shape.is_none(){return Err(CaptureError::Unavailable("DXGI visible pointer has no admitted shape"));}
        Ok(self.current.clone())
    }
}
fn validate(bytes:&[u8],info:DXGI_OUTDUPL_POINTER_SHAPE_INFO)->Result<u32,CaptureError>{
    let mono=info.Type==1;let height=if mono {if info.Height%2!=0{return Err(CaptureError::InvalidFrame("DXGI monochrome masks have uneven height"));}info.Height/2}else{info.Height};
    if !matches!(info.Type,1|2|4) || info.Width==0 || height==0 || info.Width>MAX_CURSOR_EDGE || height>MAX_CURSOR_EDGE {return Err(CaptureError::InvalidFrame("DXGI cursor type or dimensions"));}
    let minimum=if mono {(info.Width as usize).div_ceil(8)}else{info.Width as usize*4};
    let extent=(info.Pitch as usize).checked_mul(info.Height as usize).ok_or(CaptureError::BudgetExceeded("DXGI cursor pitch overflow"))?;
    if (info.Pitch as usize)<minimum || extent>bytes.len() || extent>MAX_SHAPE_BYTES {return Err(CaptureError::InvalidFrame("DXGI cursor pitch or buffer extent"));}
    if info.HotSpot.x<0 || info.HotSpot.y<0 || info.HotSpot.x>=info.Width as i32 || info.HotSpot.y>=height as i32 {return Err(CaptureError::InvalidFrame("DXGI cursor hotspot extent"));}
    if info.Type==4 {for row in 0..height as usize {for x in 0..info.Width as usize {if !matches!(bytes[row*info.Pitch as usize+x*4+3],0|255){return Err(CaptureError::InvalidFrame("DXGI masked-color alpha must be0 or255"));}}}}
    Ok(height)
}
pub(super) struct Base {pub image:RgbaFrame,pub pointer:Snapshot,_credit:NativeCopyLease}
impl Base {
    pub(super) fn new(image:RgbaFrame,pointer:Snapshot,credit:NativeCopyLease)->Self{Self {image,pointer,_credit:credit}}
    pub(super) fn composite(&self,pointer:&Snapshot,budget:&NativeCopyBudget,limits:CaptureLimits)->Result<(RgbaFrame,NativeCopyLease),CaptureError>{
        let bytes=self.image.byte_len();
        if bytes>limits.max_frame_bytes || bytes.checked_mul(2).is_none_or(|v|v>limits.max_raw_bytes){return Err(CaptureError::BudgetExceeded("DXGI clean desktop and cursor output"));}
        // The returned frame transfers to the capture owner's resident-frame
        // admission; while copying, this additional native-copy credit covers
        // source+destination overlap. Base/shape credits survive in the state.
        let output_credit=budget.reserve(NativeCopyKind::Dxgi,bytes).ok_or(CaptureError::BudgetExceeded("DXGI cursor output/native-copy budget"))?;
        let mut pixels=Vec::new();pixels.try_reserve_exact(bytes).map_err(|_|CaptureError::BudgetExceeded("DXGI cursor output allocation"))?;pixels.extend_from_slice(self.image.pixels());
        if pointer.visible {
            let shape=pointer.shape.as_ref().ok_or(CaptureError::InvalidFrame("DXGI visible pointer lost its shape"))?;
            let width=self.image.width() as i64;let height=self.image.height() as i64;
            let left=i64::from(pointer.position[0]);let top=i64::from(pointer.position[1]);
            for y in 0..shape.height {let dy=top+i64::from(y);if dy<0||dy>=height{continue;}
                for x in 0..shape.info.Width {let dx=left+i64::from(x);if dx<0||dx>=width{continue;}
                    let to=(dy as usize*width as usize+dx as usize)*4;
                    let row=y as usize*shape.info.Pitch as usize;
                    match shape.info.Type {
                        1=>{let bit=0x80u8>>(x%8);let and=if shape.bytes[row+x as usize/8]&bit!=0 {255}else{0};let xor_row=(y+shape.height) as usize*shape.info.Pitch as usize;let xor=if shape.bytes[xor_row+x as usize/8]&bit!=0{255}else{0};for channel in &mut pixels[to..to+3]{*channel=(*channel&and)^xor;}},
                        2=>{let from=row+x as usize*4;let alpha=u32::from(shape.bytes[from+3]);for (channel,source) in [(0,2),(1,1),(2,0)]{pixels[to+channel]=((u32::from(shape.bytes[from+source])*alpha+u32::from(pixels[to+channel])*(255-alpha)+127)/255) as u8;}},
                        4=>{let from=row+x as usize*4;for (channel,source) in [(0,2),(1,1),(2,0)]{let value=shape.bytes[from+source];pixels[to+channel]=if shape.bytes[from+3]==0 {value}else{pixels[to+channel]^value};}},
                        _=>return Err(CaptureError::InvalidFrame("DXGI pointer type changed after admission")),
                    }
                    pixels[to+3]=255;
                }
            }
        }
        let image=RgbaFrame::new(self.image.width(),self.image.height(),pixels).map_err(|_|CaptureError::InvalidFrame("DXGI pointer output RGBA"))?;
        Ok((image,output_credit))
    }
}

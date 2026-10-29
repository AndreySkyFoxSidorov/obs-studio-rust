//! OBS32.2.2 crop_filter, SDR RGBA8 target. Integer offsets sample texel centers.
use crate::{MediaError,ColorCorrectionInput};
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub struct CropPadSettings {pub relative:bool,pub left:i32,pub top:i32,pub right:i32,pub bottom:i32,pub width:u32,pub height:u32}
impl Default for CropPadSettings {fn default()->Self{Self {relative:true,left:0,top:0,right:0,bottom:0,width:0,height:0}}}
impl CropPadSettings {
    pub fn dimensions(self,input:(u32,u32))->Result<(u32,u32),MediaError>{
        if ![self.left,self.top,self.right,self.bottom].into_iter().all(|v|(-8192..=8192).contains(&v)) || self.width>8192 || self.height>8192 {return Err(MediaError::InvalidBuffer);}
        let (w,h)=if self.relative {((i64::from(input.0)-i64::from(self.left)-i64::from(self.right)).max(1),
            (i64::from(input.1)-i64::from(self.top)-i64::from(self.bottom)).max(1))}else{(i64::from(self.width.max(1)),i64::from(self.height.max(1)))};
        if w>8192 || h>8192 || w*h>32*1024*1024 || input.0==0 || input.1==0 {return Err(MediaError::BudgetExceeded);}
        Ok((w as u32,h as u32))
    }
}
pub(crate) fn apply(input:&[u8],width:u32,height:u32,mode:ColorCorrectionInput,settings:CropPadSettings)->Result<(Vec<u8>,u32,u32),MediaError>{
    let (w,h)=settings.dimensions((width,height))?;
    let bytes=(w as usize).checked_mul(h as usize).and_then(|v|v.checked_mul(4)).ok_or(MediaError::Overflow)?;
    let mut output=Vec::new();output.try_reserve_exact(bytes).map_err(|_|MediaError::BudgetExceeded)?;output.resize(bytes,0);
    // Integer crop/pad maps a contiguous input span to a contiguous output
    // span. Compute that intersection once instead of checking every pixel.
    let start=(-i64::from(settings.left)).clamp(0,i64::from(w));
    let end=(i64::from(width)-i64::from(settings.left)).clamp(start,i64::from(w));
    let count=(end-start) as usize;
    if count==0 {return Ok((output,w,h));}
    for y in 0..h {
        let sy=i64::from(y)+i64::from(settings.top);
        if sy<0 || sy>=i64::from(height){continue;}
        let sx=(start+i64::from(settings.left)) as usize;
        let from=(sy as usize*width as usize+sx)*4;
        let to=(y as usize*w as usize+start as usize)*4;
        let src=&input[from..from+count*4];
        let dst=&mut output[to..to+count*4];
        // sampled_bytes is exactly identity for premultiplied input and for
        // opaque pixels in every encoding; cameras commonly provide opaque
        // rows, which can therefore be copied without per-pixel conversion.
        if mode==ColorCorrectionInput::PremultipliedLinearSrgb || src.chunks_exact(4).all(|pixel|pixel[3]==255) {
            dst.copy_from_slice(src);
        } else {
            for (source,target) in src.chunks_exact(4).zip(dst.chunks_exact_mut(4)) {
                let value=crate::obs_keyer::sampled_bytes([source[0],source[1],source[2],source[3]],mode);
                target.copy_from_slice(&value);
            }
        }
    }
    Ok((output,w,h))
}

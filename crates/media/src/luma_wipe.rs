//! OBS32 luma_wipe_transition.effect, GPL-2.0-or-later. SDR scalar port;
//! source images are straight encoded RGBA8, output is opaque black-backed.
use crate::{CompositeLimits,MediaError,RgbaFrame,color_correction_decode_byte,directional::progress};

/// Decoded immutable UNORM red plane; consumers borrow it without pixel copies.
pub struct LumaWipeMask{width:u32,height:u32,red:Vec<u8>}
impl LumaWipeMask{
    pub fn new(width:u32,height:u32,red:Vec<u8>)->Result<Self,MediaError>{
        if width==0||height==0||width>2048||height>2048||red.len()!=width as usize*height as usize||red.capacity()>4*1024*1024{return Err(MediaError::InvalidBuffer);}
        Ok(Self{width,height,red})
    }
    pub fn byte_len(&self)->usize{self.red.len()}
    fn sample(&self,x:u32,y:u32,width:u32,height:u32)->f32{
        // Pixel-center UV and linear clamp, with no sRGB decode on l_tex.
        let sx=(((x as f32+0.5)/width as f32)*self.width as f32-0.5).clamp(0.0,(self.width-1) as f32);
        let sy=(((y as f32+0.5)/height as f32)*self.height as f32-0.5).clamp(0.0,(self.height-1) as f32);
        let x0=sx.floor() as u32;let y0=sy.floor() as u32;let x1=(x0+1).min(self.width-1);let y1=(y0+1).min(self.height-1);
        let wx=sx-x0 as f32;let wy=sy-y0 as f32;
        let at=|xx:u32,yy:u32|f32::from(self.red[yy as usize*self.width as usize+xx as usize])/255.0;
        let upper=at(x0,y0)*(1.0-wx)+at(x1,y0)*wx;
        let lower=at(x0,y1)*(1.0-wx)+at(x1,y1)*wx;
        upper*(1.0-wy)+lower*wy
    }
}
fn encode(value:f32)->u8{
    let v=value.clamp(0.0,1.0);
    let encoded=if v<=0.0031308{v*12.92}else{1.055*v.powf(1.0/2.4)-0.055};
    (encoded.clamp(0.0,1.0)*255.0).round() as u8
}
/// Includes both inputs, a third mask layer, output and seven pixel visits.
/// Branch order is meaningful when softness==0 or luma reaches an endpoint.
pub fn luma_wipe_on_black(a:&RgbaFrame,b:&RgbaFrame,mask:&LumaWipeMask,elapsed:u64,duration:u64,
    softness:f32,invert:bool,limits:CompositeLimits)->Result<RgbaFrame,MediaError>{
    if !softness.is_finite()||!(0.0..=1.0).contains(&softness){return Err(MediaError::InvalidTransition);}
    let t=progress(elapsed,duration)?;
    if a.width()!=b.width()||a.height()!=b.height(){return Err(MediaError::InvalidDimensions);}
    let source_bytes=if a.pixels().as_ptr()==b.pixels().as_ptr(){a.byte_len()}else{a.byte_len().checked_add(b.byte_len()).ok_or(MediaError::Overflow)?};
    let inputs=source_bytes.checked_add(mask.byte_len()).ok_or(MediaError::Overflow)?;
    let visits=(u64::from(a.width())*u64::from(a.height())).checked_mul(7).ok_or(MediaError::Overflow)?;
    if limits.max_layers<3||inputs>limits.max_input_bytes||a.byte_len()>limits.max_output_bytes||visits>limits.max_pixel_visits{return Err(MediaError::BudgetExceeded);}
    let time=(1.0+softness)*t;
    let mut out=Vec::new();out.try_reserve_exact(a.byte_len()).map_err(|_|MediaError::BudgetExceeded)?;out.resize(a.byte_len(),0);
    for(y,((ra,rb),target)) in a.pixels().chunks_exact(a.width() as usize*4).zip(b.pixels().chunks_exact(b.width() as usize*4)).zip(out.chunks_exact_mut(a.width() as usize*4)).enumerate(){
        for(x,((pa,pb),dest)) in ra.chunks_exact(4).zip(rb.chunks_exact(4)).zip(target.chunks_exact_mut(4)).enumerate(){
            let mut luma=mask.sample(x as u32,y as u32,a.width(),a.height());if invert{luma=1.0-luma;}
            let weight=if luma<=time-softness{1.0}else if luma>=time{0.0}else{(time-luma)/softness};
            // Fully selected opaque scene pixels need no sRGB round trip.
            // Most pixels outside the moving soft edge take this exact path.
            let selected=if weight==0.0{Some(pa)}else if weight==1.0{Some(pb)}else{None};
            if let Some(pixel)=selected{
                if pixel[3]==255{dest.copy_from_slice(pixel);continue;}
                if pixel[3]==0{dest.copy_from_slice(&[0,0,0,255]);continue;}
            }
            let aa=f32::from(pa[3])/255.0;let ab=f32::from(pb[3])/255.0;
            for channel in 0..3{
                let ca=color_correction_decode_byte(pa[channel])*aa;
                let cb=color_correction_decode_byte(pb[channel])*ab;
                dest[channel]=encode(ca*(1.0-weight)+cb*weight);
            }
            dest[3]=255;
        }
    }
    RgbaFrame::new(a.width(),a.height(),out)
}

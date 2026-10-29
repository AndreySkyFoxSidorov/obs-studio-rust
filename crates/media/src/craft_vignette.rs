//! EffectCraft CC Vignette SDR adaptation. SPDX-License-Identifier: MIT.
//! Copyright (c) 2026 ArtCraft Team and the EffectCraft contributors.
//! Donor: stylize2.rs at991904888a20b7da72b2a4880b21c7d912154736.
//! See assets/licenses/craft/effectcraft-MIT.txt and references.json.
use crate::{ColorCorrectionInput, MediaError, SourceBlend};
use crate::color_correction::encode;
use crate::obs_keyer::sample_lp;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VignetteSettings {
    pub amount:f32,
    pub angle_of_view:f32,
    /// Percent of the current input width/height, not preview/project coordinates.
    pub center:[f32;2],
    pub pin_highlights:f32,
}
impl Default for VignetteSettings {
    fn default()->Self{Self{amount:50.0,angle_of_view:60.0,center:[50.0,50.0],pin_highlights:0.0}}
}
impl VignetteSettings {
    pub fn validate(self)->Result<(),MediaError>{
        if self.amount.is_finite() && (-100.0..=100.0).contains(&self.amount)
            && self.angle_of_view.is_finite() && (0.0..=179.0).contains(&self.angle_of_view)
            && self.center.iter().all(|value|value.is_finite() && (-800.0..=800.0).contains(value))
            && self.pin_highlights.is_finite() && (0.0..=100.0).contains(&self.pin_highlights) {
            Ok(())
        } else { Err(MediaError::InvalidBuffer) }
    }
    pub fn is_identity(self)->bool{self.amount==0.0 || self.angle_of_view==0.0}
}

/// Operates on the admitted chain's current owned buffer. Zero raster/workspace
/// allocation; native pixels and immutable input Arcs are never mutated.
pub(crate) fn in_place(pixels:&mut [u8],width:u32,height:u32,ingress:SourceBlend,
    settings:VignetteSettings)->Result<SourceBlend,MediaError>{
    settings.validate()?;
    if width==0 || height==0 || width>8192 || height>8192
        || (width as usize).checked_mul(height as usize).and_then(|v|v.checked_mul(4))!=Some(pixels.len()) {
        return Err(MediaError::InvalidDimensions);
    }
    // Identity retains byte values and their exact original blend contract.
    if settings.is_identity(){return Ok(ingress);}
    let amount=settings.amount/100.0;
    let pin=settings.pin_highlights/100.0;
    let w=f64::from(width);let h=f64::from(height);
    let center=[w*f64::from(settings.center[0])/100.0,h*f64::from(settings.center[1])/100.0];
    let half_diagonal=(w*w+h*h).sqrt()*0.5;
    let distance=half_diagonal.max(1.0)/(f64::from(settings.angle_of_view).to_radians()*0.5).tan();
    let encoding=match ingress {
        SourceBlend::LinearSrgb=>ColorCorrectionInput::LinearAlpha,
        SourceBlend::Encoded|SourceBlend::EncodedPremultipliedSrgb=>ColorCorrectionInput::EncodedAlpha,
        SourceBlend::PremultipliedLinearSrgb=>ColorCorrectionInput::PremultipliedLinearSrgb,
    };
    for (y,row) in pixels.chunks_exact_mut(width as usize*4).enumerate(){
        let y=y as f64+0.5-center[1];
        for (x,pixel) in row.chunks_exact_mut(4).enumerate(){
            let raw=[pixel[0],pixel[1],pixel[2],pixel[3]];
            // Normalize actual ingress BEFORE donor alpha-zero skip: hidden straight
            // RGB cannot become emitted premultiplied energy. Real framebuffer
            // emission with alpha0 remains unchanged, as in the donor's skip branch.
            let sampled=sample_lp(raw,encoding);
            if sampled[3]<=0.0 {
                if ingress!=SourceBlend::PremultipliedLinearSrgb {pixel[..3].fill(0);}
                continue;
            }
            let c=[sampled[0]/sampled[3],sampled[1]/sampled[3],sampled[2]/sampled[3]];
            let x=x as f64+0.5-center[0];
            let r=(x*x+y*y).sqrt()/distance;
            let falloff=(1.0/(1.0+r*r)).powi(2) as f32;
            let mut gain=1.0-amount*(1.0-falloff);
            if pin>0.0 {
                let luminance=(0.2126*c[0]+0.7152*c[1]+0.0722*c[2]).clamp(0.0,1.0);
                gain+=(1.0-gain)*pin*luminance;
            }
            for channel in 0..3 {pixel[channel]=encode((c[channel]*gain).max(0.0)*sampled[3]);}
            // Alpha is not recomputed/rounded. Source transfer is the explicit
            // sRGB-encoded premultiplied-linear framebuffer contract.
        }
    }
    Ok(SourceBlend::PremultipliedLinearSrgb)
}

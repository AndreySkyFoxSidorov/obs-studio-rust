//! Adapted EffectCraft point kernels, selected under MIT.
//! Copyright (c) 2026 ArtCraft Team and the EffectCraft/PhotoCraft contributors.
//! See assets/licenses/craft/effectcraft-MIT.txt, photocraft-MIT.txt and references.json.
//! Straight encoded-sRGB RGBA8 ingress/egress; float arithmetic between effects.
use crate::{MediaError, RgbaFrame};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorKeyMatch {
    Rgb,
    Hue,
    Chroma,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorKeyView {
    Final,
    Source,
    Matte,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CraftVideoEffect {
    Invert { amount:f32 },
    Grayscale { amount:f32 },
    Posterize { levels:u16 },
    /// Precomputed EV gain and inverse gamma; no per-pixel parameter power.
    Exposure { gain:f32, offset:f32, inverse_gamma:f32, bypass_linear:bool },
    TemperatureTint { temperature:f32, tint:f32 },
    Vibrance { vibrance:f32, saturation:f32 },
    Tint {black:[u8;3],white:[u8;3],amount:f32},
    Tritone {shadows:[u8;3],midtones:[u8;3],highlights:[u8;3],blend:f32},
    ColorBalance {shadows:[f32;3],midtones:[f32;3],highlights:[f32;3],preserve_luminosity:bool},
    ChannelMixer {matrix:[[f32;4];3],monochrome:bool},
    GammaPedestalGain {gamma:[f32;3],pedestal:[f32;3],gain:[f32;3],black_stretch:f32},
    Threshold { level:u16 },
    /// Fixed EffectCraft Photo Filter Sepia RGB[170,120,55]; density is percent.
    Sepia { density:f32, preserve_luminosity:bool },
    /// Already resolved exact donor tint; encoded-sRGB components in0..1.
    PhotoFilter {color:[f32;3],density:f32,preserve_luminosity:bool},
    LinearColorKey {
        color: [u8; 3],
        matching: ColorKeyMatch,
        tolerance: f32,
        softness: f32,
        keep: bool,
        view: ColorKeyView,
    },
    BrightnessContrast {
        brightness: f32,
        contrast: f32,
        legacy: bool,
    },
    /// Green or blue primary-channel suppression, adapted from Key Light's
    /// balanced suppression kernel; this is not the full Key Light keyer.
    SpillSuppression {
        blue: bool,
        balance: f32,
        amount: f32,
    },
}
impl CraftVideoEffect {
    /// True when the stage preserves straight RGBA byte values exactly.
    pub fn is_passthrough(self)->bool{matches!(self,Self::Sepia{density,..}|Self::PhotoFilter{density,..}if density==0.0)||matches!(self,Self::Vibrance{vibrance,saturation} if vibrance==0.0 && saturation==0.0)||matches!(self,Self::Tint{amount,..} if amount==0.0)||matches!(self,Self::Tritone{blend,..} if blend==100.0)||matches!(self,Self::ChannelMixer{matrix,monochrome:false} if matrix==[[100.0,0.0,0.0,0.0],[0.0,100.0,0.0,0.0],[0.0,0.0,100.0,0.0]])}
    pub(crate) fn validate(self) -> Result<(), MediaError> {
        let pct = |x: f32| x.is_finite() && (0.0..=100.0).contains(&x);
        let valid = match self {
            Self::Invert{amount}|Self::Grayscale{amount}=>pct(amount),
            Self::Sepia{density,..}=>pct(density),
            Self::PhotoFilter{color,density,..}=>pct(density)&&color.iter().all(|value|value.is_finite()&&(0.0..=1.0).contains(value)),
            Self::Posterize{levels}=>(2..=255).contains(&levels),
            Self::Threshold{level}=>(1..=255).contains(&level),
            Self::Exposure{gain,offset,inverse_gamma,..}=>gain.is_finite()&&(1.0/1048576.0..=1048576.0).contains(&gain)&&offset.is_finite()&&(-0.5..=0.5).contains(&offset)&&inverse_gamma.is_finite()&&(1.0/9.99..=100.0).contains(&inverse_gamma),
            Self::TemperatureTint{temperature,tint}=>[temperature,tint].iter().all(|v|v.is_finite()&&(-100.0..=100.0).contains(v)),
            Self::Vibrance{vibrance,saturation}=>[vibrance,saturation].iter().all(|v|v.is_finite()&&(-100.0..=100.0).contains(v)),
            Self::Tint{amount,..}=>pct(amount),
            Self::Tritone{blend,..}=>pct(blend),
            Self::ColorBalance{shadows,midtones,highlights,..}=>shadows.into_iter().chain(midtones).chain(highlights).all(|v|v.is_finite()&&(-100.0..=100.0).contains(&v)),
            Self::ChannelMixer{matrix,..}=>matrix.into_iter().flatten().all(|v|v.is_finite()&&(-200.0..=200.0).contains(&v)),
            Self::GammaPedestalGain{gamma,pedestal,gain,black_stretch}=>gamma.into_iter().all(|v|v.is_finite()&&(0.1..=10.0).contains(&v))&&pedestal.into_iter().all(|v|v.is_finite()&&(-2.0..=2.0).contains(&v))&&gain.into_iter().all(|v|v.is_finite()&&(0.0..=4.0).contains(&v))&&black_stretch.is_finite()&&(0.0..=4.0).contains(&black_stretch),
            Self::LinearColorKey {
                tolerance,
                softness,
                ..
            } => pct(tolerance) && pct(softness),
            Self::BrightnessContrast {
                brightness,
                contrast,
                ..
            } => {
                brightness.is_finite()
                    && contrast.is_finite()
                    && (-150.0..=150.0).contains(&brightness)
                    && (-100.0..=100.0).contains(&contrast)
            }
            Self::SpillSuppression {
                balance, amount, ..
            } => pct(balance) && pct(amount),
        };
        if valid {
            Ok(())
        } else {
            Err(MediaError::InvalidBuffer)
        }
    }
}
#[derive(Clone, Copy, Debug)]
pub struct CraftVideoLimits {
    pub max_output_bytes: usize,
    pub max_pixel_stages: usize,
}

/// One owned output buffer for the complete ordered chain, with admission and
/// validation before allocation. The caller owns scheduling, scope and caching.
pub fn craft_video_effects(
    input: &RgbaFrame,
    effects: &[CraftVideoEffect],
    limits: CraftVideoLimits,
) -> Result<RgbaFrame, MediaError> {
    if effects.len() > crate::source_effects::MAX_SOURCE_VIDEO_EFFECTS {
        return Err(MediaError::BudgetExceeded);
    }
    let count = input.byte_len() / 4;
    let visits = count
        .checked_mul(effects.len())
        .ok_or(MediaError::Overflow)?;
    if input.byte_len() > limits.max_output_bytes || visits > limits.max_pixel_stages {
        return Err(MediaError::BudgetExceeded);
    }
    for effect in effects {
        effect.validate()?;
    }
    // A neutral Sepia is byte-identical, including hidden alpha-zero RGB.
    if !effects.is_empty()&&effects.iter().all(|effect|effect.is_passthrough()){return Ok(input.clone());}
    let mut output = input.pixels().to_vec();
    for pixel in output.chunks_exact_mut(4) {
        let mut c = [
            f32::from(pixel[0]) / 255.0,
            f32::from(pixel[1]) / 255.0,
            f32::from(pixel[2]) / 255.0,
        ];
        let mut alpha = f32::from(pixel[3]) / 255.0;
        for effect in effects {
            (c, alpha) = process_craft_pixel(c, alpha, effect);
        }
        if alpha == 0.0 {
            pixel.fill(0);
        } else {
            for channel in 0..3 {
                pixel[channel] = (c[channel].clamp(0.0, 1.0) * 255.0).round() as u8;
            }
            pixel[3] = (alpha.clamp(0.0, 1.0) * 255.0).round() as u8;
        }
    }
    RgbaFrame::new(input.width(), input.height(), output)
}

pub(crate) fn process_craft_pixel(
    mut c: [f32; 3],
    mut alpha: f32,
    effect: &CraftVideoEffect,
) -> ([f32; 3], f32) {
    match *effect {
        // PhotoCraft compose/adjust.rs Invert. Amount is an explicit post-mix.
        CraftVideoEffect::Invert{amount}=>{let k=amount/100.0;c=c.map(|v|v+(1.0-2.0*v)*k);}
        // EffectCraft color_fx.rs tint: fixed black/white endpoints, Rec.601.
        CraftVideoEffect::Grayscale{amount}=>{let y=(0.299*c[0]+0.587*c[1]+0.114*c[2]).clamp(0.0,1.0);let k=amount/100.0;c=c.map(|v|v+(y-v)*k);}
        // Existing Sepia remains the exact fixed preset specialization.
        CraftVideoEffect::Sepia{density,preserve_luminosity}=>{
            c=photo_filter_pixel(c,[170.0f32/255.0,120.0f32/255.0,55.0f32/255.0],density,preserve_luminosity);
        }
        CraftVideoEffect::PhotoFilter{color,density,preserve_luminosity}=>{
            c=photo_filter_pixel(c,color,density,preserve_luminosity);
        }
        // PhotoCraft's exact SDR8 input bins and floored output levels.
        CraftVideoEffect::Posterize{levels}=>{let n=f32::from(levels);c=c.map(|v|{let x=(v.clamp(0.0,1.0)*255.0).round();let bin=(x*n/256.0).floor().min(n-1.0);(bin*255.0/(n-1.0)).floor()/255.0});}
        // EffectCraft master Exposure, signed2.2 conversion. The two signed
        // output powers combine algebraically; clip to this host's SDR range.
        CraftVideoEffect::Exposure{gain,offset,inverse_gamma,bypass_linear}=>{c=c.map(|v|{let x=if bypass_linear{v}else{v.abs().powf(2.2).copysign(v)};let exponent=if bypass_linear{inverse_gamma}else{inverse_gamma/2.2};let x=x*gain+offset;x.abs().powf(exponent).copysign(x).clamp(0.0,1.0)});}
        // EffectCraft color3.rs basic white balance, not a Kelvin simulation.
        CraftVideoEffect::TemperatureTint{temperature,tint}=>{let t=temperature/100.0;let m=tint/100.0;c=[c[0]*(1.0+0.25*t),c[1]*(1.0-0.25*m),c[2]*(1.0-0.25*t)];}
        // EffectCraft color_fx.rs vibrance: HSL with separately controlled
        // saturation, operating on straight encoded RGB and preserving alpha.
        CraftVideoEffect::Vibrance{vibrance,saturation}=>{
            let (h,s,l)=rgb_to_hsl(c);
            let s=(s*(1.0+saturation/100.0)+(vibrance/100.0)*(1.0-s)*s.min(0.5)).clamp(0.0,1.0);
            c=hsl_to_rgb(h,s,l);
        }
        // PhotoCraft threshold_white(U8): rounded byte luminosity, halves down.
        CraftVideoEffect::Tritone{shadows,midtones,highlights,blend}=>{let l=(0.299*c[0]+0.587*c[1]+0.114*c[2]).clamp(0.0,1.0);let (a,b,t)=if l<0.5{(shadows,midtones,l*2.0)}else{(midtones,highlights,(l-0.5)*2.0)};let mix=1.0-blend/100.0;for i in 0..3{let low=f32::from(a[i])/255.0;let high=f32::from(b[i])/255.0;let target=low+(high-low)*t;c[i]+=(target-c[i])*mix;}}
        CraftVideoEffect::ColorBalance{shadows,midtones,highlights,preserve_luminosity}=>{
            let luminance=|v:[f32;3]|0.2126*v[0]+0.7152*v[1]+0.0722*v[2];
            let original=c;let l=luminance(c).clamp(0.0,1.0);
            let ws=(1.0-l*2.0).clamp(0.0,1.0);let wh=(l*2.0-1.0).clamp(0.0,1.0);let wm=1.0-ws-wh;
            for i in 0..3 {c[i]=(c[i]+(shadows[i]/100.0*ws+midtones[i]/100.0*wm+highlights[i]/100.0*wh)*0.5).max(0.0);}
            if preserve_luminosity {let d=luminance(original)-luminance(c);c=c.map(|v|(v+d).max(0.0));}
        }
        CraftVideoEffect::ChannelMixer{matrix,monochrome}=>{
            // EffectCraft processes straight RGB; alpha is preserved.
            let row=|r:[f32;4]| (c[0]*(r[0]/100.0)+c[1]*(r[1]/100.0)+c[2]*(r[2]/100.0)+r[3]/100.0).max(0.0);
            c=if monochrome {let value=row(matrix[0]);[value;3]} else {[row(matrix[0]),row(matrix[1]),row(matrix[2])]};
        }
        CraftVideoEffect::GammaPedestalGain{gamma,pedestal,gain,black_stretch}=>{
            for i in 0..3 {
                let v=c[i];
                let v=if black_stretch>0.0 && v>0.0 && v<1.0 {v+black_stretch*0.25*v*(1.0-v).powi(4)} else {v};
                c[i]=(pedestal[i]+(gain[i]-pedestal[i])*v.max(0.0).powf(gamma[i])).max(0.0);
            }
        }
        CraftVideoEffect::Tint{black,white,amount}=>{let l=(0.299*c[0]+0.587*c[1]+0.114*c[2]).clamp(0.0,1.0);let k=amount/100.0;for i in 0..3{let b=f32::from(black[i])/255.0;let w=f32::from(white[i])/255.0;let target=b+(w-b)*l;c[i]+=(target-c[i])*k;}}
        CraftVideoEffect::Threshold{level}=>{let q=c.map(|v|(v.clamp(0.0,1.0)*255.0).round());let white=30.0*q[0]+59.0*q[1]+11.0*q[2]>100.0*f32::from(level)-50.0;c=[if white{1.0}else{0.0};3];}

        CraftVideoEffect::LinearColorKey {
            color,
            matching,
            tolerance,
            softness,
            keep,
            view,
        } => {
            if view == ColorKeyView::Source {
                return (c, alpha);
            }
            let key = color.map(|v| f32::from(v) / 255.0);
            let distance = match matching {
                ColorKeyMatch::Rgb => {
                    ((c[0] - key[0]).powi(2) + (c[1] - key[1]).powi(2) + (c[2] - key[2]).powi(2))
                        .sqrt()
                        / 3_f32.sqrt()
                }
                ColorKeyMatch::Hue => hue_distance(c, key),
                ColorKeyMatch::Chroma => {
                    let a = chroma(c);
                    let b = chroma(key);
                    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt()
                        / std::f32::consts::SQRT_2
                }
            };
            let tolerance = tolerance / 100.0;
            let softness = softness / 100.0;
            let mut matte = if softness > 1e-6 {
                ((distance - tolerance) / softness).clamp(0.0, 1.0)
            } else if distance > tolerance {
                1.0
            } else {
                0.0
            };
            if keep {
                matte = 1.0 - matte;
            }
            alpha *= matte;
            if view == ColorKeyView::Matte {
                c = [alpha; 3];
                alpha = 1.0;
            }
        }
        CraftVideoEffect::BrightnessContrast {
            brightness,
            contrast,
            legacy,
        } => {
            let brightness = brightness / 100.0;
            let contrast = contrast / 100.0;
            if legacy {
                let coefficient = if contrast >= 0.0 {
                    1.0 / (1.0 - contrast * 0.99)
                } else {
                    1.0 + contrast
                };
                c = c.map(|v| ((v + brightness - 0.5) * coefficient + 0.5).max(0.0));
            } else {
                c = c.map(|v| bc_modern(v, brightness, contrast));
            }
        }
        CraftVideoEffect::SpillSuppression {
            blue,
            balance,
            amount,
        } => {
            let primary = if blue { 2 } else { 1 };
            let a = (primary + 1) % 3;
            let b = (primary + 2) % 3;
            let balance = balance / 100.0;
            let limit = c[a] * balance + c[b] * (1.0 - balance);
            c[primary] -= (c[primary] - limit).max(0.0) * (amount / 100.0);
        }
    }
    (c, alpha)
}

// EffectCraft color_fx.rs::photo_filter + mix; color::luminance. Same selected
// f32 operation order for the full Photo Filter and the retained Sepia wrapper.
fn photo_filter_pixel(c:[f32;3],color:[f32;3],density:f32,preserve_luminosity:bool)->[f32;3]{
    if density==0.0{return c;}
    let filtered=[c[0]*color[0],c[1]*color[1],c[2]*color[2]];
    let amount=density/100.0;
    let mut output=[c[0]+(filtered[0]-c[0])*amount,c[1]+(filtered[1]-c[1])*amount,c[2]+(filtered[2]-c[2])*amount];
    if preserve_luminosity{
        let l0=0.2126*c[0]+0.7152*c[1]+0.0722*c[2];
        let l1=(0.2126*output[0]+0.7152*output[1]+0.0722*output[2]).max(1e-6);
        output=output.map(|value|value*l0/l1);
    }
    output
}

// EffectCraft crates/effects/src/color_fx.rs::bc_modern, adapted only in naming
// and formatting. Parameters are normalized authored percentages.
fn bc_modern(v: f32, b: f32, c: f32) -> f32 {
    if v >= 1.0 {
        return v;
    }
    let mut v = v.max(0.0);
    if b != 0.0 {
        let e = 2_f32.powf(1.24 * b.abs());
        v = 1.0 - (1.0 - v).powf(if b > 0.0 { e } else { 1.0 / e });
    }
    if c != 0.0 {
        let g = (1.0 + 0.53 * c.abs()).max(0.05);
        let g = if c > 0.0 { g } else { 1.0 / g };
        v = if v < 0.5 {
            0.5 * (2.0 * v).powf(g)
        } else {
            1.0 - 0.5 * (2.0 - 2.0 * v).max(0.0).powf(g)
        };
    }
    v
}
fn chroma(c: [f32; 3]) -> [f32; 2] {
    let s = c[0] + c[1] + c[2];
    if s <= 1e-6 {
        [1.0 / 3.0; 2]
    } else {
        [c[0] / s, c[1] / s]
    }
}
fn hue_distance(a: [f32; 3], b: [f32; 3]) -> f32 {
    let (ha, sa, _) = rgb_to_hsl(a);
    let (hb, _, _) = rgb_to_hsl(b);
    if sa < 0.02 {
        return 1.0;
    }
    let d = (ha - hb).abs();
    d.min(1.0 - d) * 2.0
}
fn rgb_to_hsl(c: [f32; 3]) -> (f32, f32, f32) {
    let [r, g, b] = c;
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) * 0.5;
    if (max - min).abs() < 1e-7 {
        return (0.0, 0.0, l);
    }
    let d = max - min;
    let s = if l > 0.5 {
        d / (2.0 - max - min)
    } else {
        d / (max + min)
    };
    let h = if max == r {
        (((g - b) / d + if g < b { 6.0 } else { 0.0 }) / 6.0).rem_euclid(1.0)
    } else if max == g {
        ((b - r) / d + 2.0) / 6.0
    } else {
        ((r - g) / d + 4.0) / 6.0
    };
    (h, s, l)
}

fn hsl_to_rgb(h:f32,s:f32,l:f32)->[f32;3]{
    if s<=0.0{return [l;3];}
    let q=if l<0.5{l*(1.0+s)}else{l+s-l*s};
    let p=2.0*l-q;
    let f=|t:f32|{let t=t.rem_euclid(1.0);if t<1.0/6.0{p+(q-p)*6.0*t}else if t<0.5{q}else if t<2.0/3.0{p+(q-p)*(2.0/3.0-t)*6.0}else{p}};
    [f(h+1.0/3.0),f(h),f(h-1.0/3.0)]
}

#[cfg(test)]
mod tests {
    use super::*;
    fn run(pixels: Vec<u8>, effects: &[CraftVideoEffect]) -> RgbaFrame {
        let frame = RgbaFrame::new(pixels.len() as u32 / 4, 1, pixels).unwrap();
        craft_video_effects(
            &frame,
            effects,
            CraftVideoLimits {
                max_output_bytes: 1024,
                max_pixel_stages: 1024,
            },
        )
        .unwrap()
    }
    fn key(view: ColorKeyView, keep: bool) -> CraftVideoEffect {
        CraftVideoEffect::LinearColorKey {
            color: [0, 255, 0],
            matching: ColorKeyMatch::Rgb,
            tolerance: 0.0,
            softness: 0.0,
            keep,
            view,
        }
    }
    #[test]
    fn exact_screen_foreground_source_and_matte_preserve_alpha_semantics() {
        let pixels = vec![0, 255, 0, 255, 255, 0, 0, 128];
        assert_eq!(
            run(pixels.clone(), &[key(ColorKeyView::Final, false)]).pixels(),
            [0, 0, 0, 0, 255, 0, 0, 128]
        );
        assert_eq!(
            run(pixels.clone(), &[key(ColorKeyView::Final, true)]).pixels(),
            [0, 255, 0, 255, 0, 0, 0, 0]
        );
        assert_eq!(
            run(pixels.clone(), &[key(ColorKeyView::Source, false)]).pixels(),
            pixels
        );
        assert_eq!(
            run(pixels, &[key(ColorKeyView::Matte, false)]).pixels(),
            [0, 0, 0, 255, 128, 128, 128, 255]
        );
    }
    #[test]
    fn legacy_ramp_spill_and_order_have_independent_literal_results() {
        let bc = CraftVideoEffect::BrightnessContrast {
            brightness: 50.0,
            contrast: 0.0,
            legacy: true,
        };
        assert_eq!(
            run(vec![0, 64, 128, 255], &[bc]).pixels(),
            [128, 192, 255, 255]
        );
        let spill = CraftVideoEffect::SpillSuppression {
            blue: false,
            balance: 50.0,
            amount: 100.0,
        };
        assert_eq!(
            run(vec![100, 240, 60, 128], &[spill]).pixels(),
            [100, 80, 60, 128]
        );
        let pixels = vec![0, 255, 0, 255];
        assert_eq!(
            run(pixels.clone(), &[key(ColorKeyView::Final, false), bc]).pixels(),
            [0, 0, 0, 0]
        );
        assert_eq!(
            run(pixels, &[bc, key(ColorKeyView::Final, false)]).pixels(),
            [128, 255, 128, 255]
        );
    }
    #[test]
    fn budgets_and_invalid_config_reject_without_touching_the_input() {
        let frame = RgbaFrame::solid(2, 2, [0, 255, 0, 255]).unwrap();
        for limits in [
            CraftVideoLimits {
                max_output_bytes: 15,
                max_pixel_stages: 4,
            },
            CraftVideoLimits {
                max_output_bytes: 16,
                max_pixel_stages: 3,
            },
        ] {
            assert_eq!(
                craft_video_effects(&frame, &[key(ColorKeyView::Final, false)], limits),
                Err(MediaError::BudgetExceeded)
            );
        }
        let invalid = CraftVideoEffect::BrightnessContrast {
            brightness: f32::NAN,
            contrast: 0.0,
            legacy: false,
        };
        assert_eq!(
            craft_video_effects(
                &frame,
                &[invalid],
                CraftVideoLimits {
                    max_output_bytes: 16,
                    max_pixel_stages: 4
                }
            ),
            Err(MediaError::InvalidBuffer)
        );
        assert_eq!(frame.pixels(), [0, 255, 0, 255].repeat(4));
    }
}

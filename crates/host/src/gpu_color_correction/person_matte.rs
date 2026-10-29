//! Bounded owned mask transport on the existing compute owner. No NN or CPU
//! matte implementation lives here. Axis coefficients retain the CPU f64 map.
use super::*;
use skylive_media::PersonMatteLimits;

const MASK_LEN:usize=256*256;
const MASK_BYTES:usize=MASK_LEN*4;
const AXIS_BYTES:usize=16;
const MAX_AXIS_LEN:usize=4096;
pub(super) const MATTE_BYTES:usize=MASK_BYTES+2*MAX_AXIS_LEN*AXIS_BYTES;

pub(super) struct Matte {pub payload:Vec<u8>,threshold:f32,softness:f32}
impl Matte {
    fn prepare(dimensions:(u32,u32),mask:&[f32],threshold:f32,softness:f32)->Result<Self,Error> {
        let length=payload_bytes(dimensions)?;
        let mut payload=Vec::new();
        payload.try_reserve_exact(length).map_err(|_|Error::Limit("GPU matte payload allocation"))?;
        for value in mask {payload.extend_from_slice(&value.to_le_bytes());}
        for destination_len in [dimensions.0,dimensions.1] {
            for destination in 0..destination_len {
                // Same center mapping, clamp and f64->f32 conversion as the
                // unchanged CPU implementation. WGSL does not recompute axes.
                let coordinate=((f64::from(destination)+0.5)*256.0/f64::from(destination_len)-0.5).clamp(0.0,255.0);
                let first=coordinate.floor() as u32;
                let second=(first+1).min(255);
                let weight=(coordinate-f64::from(first)) as f32;
                payload.extend_from_slice(&first.to_le_bytes());
                payload.extend_from_slice(&second.to_le_bytes());
                payload.extend_from_slice(&weight.to_le_bytes());
                payload.extend_from_slice(&0u32.to_le_bytes());
            }
        }
        Ok(Self {payload,threshold,softness})
    }
    pub(super) fn parameters(&self,image:&RgbaFrame,blend:SourceBlend)->[u8;PARAM_BYTES] {
        let mut bytes=[0;PARAM_BYTES];
        let policy=match blend {SourceBlend::PremultipliedLinearSrgb=>1u32,
            SourceBlend::Encoded|SourceBlend::LinearSrgb|SourceBlend::EncodedPremultipliedSrgb=>0};
        for (index,value) in [(image.byte_len()/4) as u32,policy,image.width(),image.height()].into_iter().enumerate(){
            bytes[index*4..index*4+4].copy_from_slice(&value.to_le_bytes());
        }
        bytes[16..20].copy_from_slice(&self.threshold.to_le_bytes());
        bytes[20..24].copy_from_slice(&self.softness.to_le_bytes());
        bytes
    }
}

fn payload_bytes((width,height):(u32,u32))->Result<usize,Error> {
    if width==0||height==0||width as usize>MAX_AXIS_LEN||height as usize>MAX_AXIS_LEN {
        return Err(Error::Unsupported("person matte dimensions outside GPU axis profile"));
    }
    (width as usize).checked_add(height as usize).and_then(|n|n.checked_mul(AXIS_BYTES))
        .and_then(|n|n.checked_add(MASK_BYTES)).ok_or(Error::Limit("GPU matte axes overflow"))
}

/// Metadata-only admission, before inference, input copies or native startup.
/// The returned peak includes owned input/readback plus two packed payloads
/// (request storage and queue upload staging), including uniform upload bytes.
fn admit(dimensions:(u32,u32),threshold:f32,softness:f32,cpu:PersonMatteLimits,
    free_cpu_bytes:usize,profile:Limits)->Result<usize,Error> {
    if !cfg!(any(target_os="windows",target_os="macos"))||!cfg!(target_endian="little") {
        return Err(Error::Unsupported("DX12/Metal little-endian native targets only"));
    }
    if !threshold.is_finite()||!(0.0..=1.0).contains(&threshold)
        ||!softness.is_finite()||!(0.001..=1.0).contains(&softness) {
        return Err(Error::Invalid(MediaError::InvalidBuffer));
    }
    let payload=payload_bytes(dimensions)?;
    let pixels=(dimensions.0 as usize).checked_mul(dimensions.1 as usize).ok_or(Error::Limit("GPU matte pixel overflow"))?;
    let bytes=pixels.checked_mul(4).ok_or(Error::Limit("GPU matte frame overflow"))?;
    let input_bytes=bytes.checked_add(MASK_BYTES).ok_or(Error::Limit("GPU matte input overflow"))?;
    let visits=(pixels as u64).checked_mul(5).and_then(|n|n.checked_add(MASK_LEN as u64)).ok_or(Error::Limit("GPU matte sampling overflow"))?;
    if input_bytes>cpu.max_input_bytes||bytes>cpu.max_output_bytes||visits>cpu.max_pixel_visits {
        return Err(Error::Limit("existing CPU person-matte admission"));
    }
    let cpu_peak=bytes.checked_mul(2).and_then(|n|n.checked_add(payload.checked_mul(2)?))
        .and_then(|n|n.checked_add(PARAM_BYTES+MASK_BYTES)).ok_or(Error::Limit("GPU matte CPU staging overflow"))?.max(2*TABLE_BYTES);
    let device_peak=bytes.checked_mul(3).and_then(|n|n.checked_add(TABLE_BYTES+PARAM_BYTES+MATTE_BYTES))
        .ok_or(Error::Limit("GPU matte device allocation overflow"))?;
    let invocations=(pixels as u64).div_ceil(64)*64;
    if bytes>profile.max_frame_bytes||cpu_peak>profile.max_cpu_transient_bytes||cpu_peak>free_cpu_bytes
        ||device_peak>profile.max_device_bytes||invocations>profile.max_dispatch_invocations {
        return Err(Error::Limit("GPU matte frame/staging/device/dispatch admission"));
    }
    Ok(cpu_peak)
}

impl Runtime {
    pub fn preflight_person_matte(&self,dimensions:(u32,u32),threshold:f32,softness:f32,
        limits:PersonMatteLimits,free_cpu_bytes:usize)->Result<usize,Error> {
        let peak=admit(dimensions,threshold,softness,limits,free_cpu_bytes,self.limits)?;
        let state=self.state.lock().map_err(|_|Error::Closed)?;
        if let Some(error)=&state.failure{return Err(error.clone());}
        if let Some(owner)=&state.owner {
            owner.client.preflight_person_matte(dimensions,threshold,softness,limits,free_cpu_bytes)?;
        }
        Ok(peak)
    }
    pub fn apply_person_matte(&self,input:impl Into<SharedVideoFrame>,blend:SourceBlend,mask:&[f32],
        threshold:f32,softness:f32,limits:PersonMatteLimits,free_cpu_bytes:usize)->Result<(SourceEffectFrame,Report),Error> {
        let input = input.into();
        self.preflight_person_matte((input.width(),input.height()),threshold,softness,limits,free_cpu_bytes)?;
        validate_mask(mask)?;
        self.client()?.apply_person_matte(input,blend,mask,threshold,softness,limits,free_cpu_bytes)
    }
}
impl Client {
    pub fn preflight_person_matte(&self,dimensions:(u32,u32),threshold:f32,softness:f32,
        limits:PersonMatteLimits,free_cpu_bytes:usize)->Result<usize,Error> {
        let peak=admit(dimensions,threshold,softness,limits,free_cpu_bytes,self.shared.limits)?;
        // Preserve a worker's initialization/device diagnostic even though it
        // also closes the service after recording that failure.
        if let Some(error)=self.shared.failure.lock().map_err(|_|Error::Closed)?.clone(){return Err(error);}
        if self.shared.closed.load(Ordering::Acquire){return Err(Error::Closed);}
        Ok(peak)
    }
    /// Background render owners only. Mask and axes remain owned by the GPU
    /// request until actual completion/retirement, including after caller timeout.
    pub fn apply_person_matte(&self,input:impl Into<SharedVideoFrame>,blend:SourceBlend,mask:&[f32],
        threshold:f32,softness:f32,limits:PersonMatteLimits,free_cpu_bytes:usize)->Result<(SourceEffectFrame,Report),Error> {
        let input = input.into();
        self.preflight_person_matte((input.width(),input.height()),threshold,softness,limits,free_cpu_bytes)?;
        validate_mask(mask)?;
        let permit=self.reserve()?;
        let matte=Matte::prepare((input.width(),input.height()),mask,threshold,softness)?;
        let payload_sha256: [u8;32]=Sha256::digest(&matte.payload).into();
        let mut settings=[0;14];settings[0]=threshold.to_bits();settings[1]=softness.to_bits();
        self.submit(input,blend,settings,Operation::PersonMatte(matte),payload_sha256,permit)
    }
}
fn validate_mask(mask:&[f32])->Result<(),Error> {
    if mask.len()!=MASK_LEN||mask.iter().any(|p|!p.is_finite()||!(0.0..=1.0).contains(p)) {
        return Err(Error::Invalid(MediaError::InvalidBuffer));
    }
    Ok(())
}

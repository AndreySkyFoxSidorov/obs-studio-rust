//! Immutable LUT data and its existing table-store credit travel together.
//! No parsing, file access, GUI work or second native service.
use super::*;
use skylive_media::{ColorLut,ColorLutSettings,ColorLutLimits,LutKind};
use std::sync::Weak;

pub(crate) const MAX_LUT_BYTES:usize=24*1024*1024;
pub(crate) const UPLOAD_SCRATCH:usize=65532; // RGB triples, also four-byte aligned.
/// Implemented only by the host's bounded LUT table owner.
pub(crate) trait LutRetention:Send+Sync {}
#[derive(Clone)]
pub(crate) struct LutLease {value:Arc<ColorLut>,retention:Arc<dyn LutRetention>}
impl LutLease {
    pub(crate) fn new(value:Arc<ColorLut>,retention:Arc<dyn LutRetention>)->Self{Self {value,retention}}
    fn identity(&self)->usize{Arc::as_ptr(&self.retention) as *const () as usize}
}
pub(super) struct LutJob {pub lease:LutLease,settings:ColorLutSettings}
pub(super) struct NativeTable {pub buffer:wgpu::Buffer,pub bytes:usize,owner:Weak<dyn LutRetention>,uploaded:bool}
impl LutJob {
    pub(super) fn parameters(&self,image:&RgbaFrame,blend:SourceBlend)->[u8;PARAM_BYTES]{
        let mut bytes=[0;PARAM_BYTES];
        let policy=match blend{SourceBlend::LinearSrgb=>0,SourceBlend::Encoded|SourceBlend::EncodedPremultipliedSrgb=>1,SourceBlend::PremultipliedLinearSrgb=>2};
        for(i,v)in [(image.byte_len()/4)as u32,policy,image.width(),image.height(),self.lease.value.size(),u32::from(self.lease.value.kind()==LutKind::ThreeD),u32::from(self.settings.passthrough_alpha),0].into_iter().enumerate(){bytes[i*4..i*4+4].copy_from_slice(&v.to_le_bytes());}
        let(min,max)=self.lease.value.domain();let(scale,offset)=self.lease.value.texture_transform();
        for(row,values)in [min,max,scale,offset].into_iter().enumerate(){for(i,v)in values.into_iter().enumerate(){let start=32+row*16+i*4;bytes[start..start+4].copy_from_slice(&v.to_le_bytes());}}
        bytes[44..48].copy_from_slice(&self.settings.amount.to_le_bytes());bytes
    }
}
pub(super) fn admit(dimensions:(u32,u32),table_bytes:usize,settings:ColorLutSettings,cpu:ColorLutLimits,free:usize,profile:Limits)->Result<usize,Error>{
    if !cfg!(any(target_os="windows",target_os="macos"))||!cfg!(target_endian="little"){return Err(Error::Unsupported("DX12/Metal little-endian native targets only"));}
    let required=skylive_media::color_lut_requirements(dimensions,table_bytes,settings).map_err(Error::Invalid)?;
    if table_bytes==0||table_bytes>MAX_LUT_BYTES||required.input_bytes>cpu.max_input_bytes||required.output_bytes>cpu.max_output_bytes||required.texel_visits>cpu.max_texel_visits{return Err(Error::Limit("existing CPU LUT admission"));}
    let frame=required.output_bytes;
    // Readback Vec + queued input upload + queued table upload + fixed conversion
    // scratch. The retained CPU table is charged separately by LutStore/chain.
    let peak=frame.checked_mul(2).and_then(|n|n.checked_add(table_bytes)).and_then(|n|n.checked_add(UPLOAD_SCRATCH+PARAM_BYTES)).ok_or(Error::Limit("LUT GPU staging overflow"))?.max(2*TABLE_BYTES);
    let device=frame.checked_mul(3).and_then(|n|n.checked_add(TABLE_BYTES+PARAM_BYTES+MATTE_BYTES)).and_then(|n|n.checked_add(2*table_bytes)).ok_or(Error::Limit("LUT GPU device overflow"))?;
    if frame>profile.max_frame_bytes||dimensions.0>4096||dimensions.1>4096||peak>free||peak>profile.max_cpu_transient_bytes||device>profile.max_device_bytes||(frame as u64/4).div_ceil(64)*64>profile.max_dispatch_invocations{return Err(Error::Limit("LUT GPU frame/staging/device/dispatch admission"));}
    Ok(peak)
}
impl Runtime {
    pub(crate) fn preflight_lut(&self,dimensions:(u32,u32),table_bytes:usize,settings:ColorLutSettings,cpu:ColorLutLimits,free:usize)->Result<usize,Error>{
        let peak=admit(dimensions,table_bytes,settings,cpu,free,self.limits)?;
        let state=self.state.lock().map_err(|_|Error::Closed)?;
        if let Some(error)=&state.failure{return Err(error.clone());}
        if let Some(owner)=&state.owner {owner.client.preflight_lut(dimensions,table_bytes,settings,cpu,free)?;}
        Ok(peak)
    }
    pub(crate) fn apply_lut(&self,input:impl Into<SharedVideoFrame>,blend:SourceBlend,lease:LutLease,settings:ColorLutSettings,cpu:ColorLutLimits,free:usize)->Result<(SourceEffectFrame,Report),Error>{
        let input = input.into();
        if lease.value.needs_cpu_resolve(){return Err(Error::Unsupported("Resolve CUBE requires CPU processing"));}
        self.preflight_lut((input.width(),input.height()),lease.value.byte_len(),settings,cpu,free)?;
        self.client()?.apply_lut(input,blend,lease,settings,cpu,free)
    }
}
impl Client {
    pub(crate) fn preflight_lut(&self,dimensions:(u32,u32),table_bytes:usize,settings:ColorLutSettings,cpu:ColorLutLimits,free:usize)->Result<usize,Error>{
        let peak=admit(dimensions,table_bytes,settings,cpu,free,self.shared.limits)?;
        if let Some(error)=self.shared.failure.lock().map_err(|_|Error::Closed)?.clone(){return Err(error);}
        if self.shared.closed.load(Ordering::Acquire){return Err(Error::Closed);}
        Ok(peak)
    }
    pub(crate) fn apply_lut(&self,input:impl Into<SharedVideoFrame>,blend:SourceBlend,lease:LutLease,settings:ColorLutSettings,cpu:ColorLutLimits,free:usize)->Result<(SourceEffectFrame,Report),Error>{
        let input = input.into();
        if lease.value.needs_cpu_resolve(){return Err(Error::Unsupported("Resolve CUBE requires CPU processing"));}
        self.preflight_lut((input.width(),input.height()),lease.value.byte_len(),settings,cpu,free)?;
        let mut authored=[0;14];authored[0]=settings.amount.to_bits();authored[1]=u32::from(settings.passthrough_alpha);
        // Weak identity pins the allocation against address reuse. No repeated
        // 24MiB table hash or retained uncredited table copy is needed.
        let digest=Sha256::digest((lease.identity()as u64).to_le_bytes()).into();
        let permit=self.reserve()?;
        self.submit(input,blend,authored,Operation::Lut(LutJob{lease,settings}),digest,permit)
    }
}
impl Native {
    pub(super) fn prepare_lut(&mut self,job:&LutJob,frame_bytes:usize)->Result<(),Error>{
        let bytes=job.lease.value.entries().len()*12;
        let owner=Arc::downgrade(&job.lease.retention);
        if self.lut_table.as_ref().is_some_and(|old|old.bytes==bytes&&Weak::ptr_eq(&old.owner,&owner)){return self.ensure_slot(frame_bytes,2*bytes);}
        // Native::apply has already settled previous queue/map work. Release the
        // old table BEFORE frame capacity/table replacement admission.
        if let Some(old)=self.lut_table.take(){old.buffer.destroy();}
        self.ensure_slot(frame_bytes,2*bytes)?;
        self.lut_table=Some(NativeTable{buffer:buffer(&self.device,"OBS LUT RGB table",bytes,wgpu::BufferUsages::STORAGE|wgpu::BufferUsages::COPY_DST),bytes,owner,uploaded:false});
        Ok(())
    }
    pub(super) fn upload_lut(&mut self,job:&LutJob){
        let Some(table)=self.lut_table.as_mut() else{return;};
        if table.uploaded{return;}
        let mut scratch=[0u8;UPLOAD_SCRATCH];let mut offset=0u64;
        for chunk in job.lease.value.entries().chunks(UPLOAD_SCRATCH/12){
            for(i,value)in chunk.iter().flatten().enumerate(){scratch[i*4..i*4+4].copy_from_slice(&value.to_le_bytes());}
            let len=chunk.len()*12;self.queue.write_buffer(&table.buffer,offset,&scratch[..len]);offset+=len as u64;
        }
        table.uploaded=true;
    }
}

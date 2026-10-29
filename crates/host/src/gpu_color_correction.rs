//! Headless compute/readback service. Native objects never leave its worker.
//! No UI surface or hidden CPU fallback. Call `Client::apply` from render owners.
use sha2::{Digest, Sha256};
use skylive_media::{ColorCorrectionCoefficients, ColorCorrectionSettings, MediaError, RgbaFrame, SharedVideoFrame,
    SourceBlend, SourceEffectFrame, SourceEffectsLimits, SourceVideoEffect,
    color_correction_decode_byte, color_correction_linear_alpha_byte, source_effect_requirements};
use std::{borrow::Cow, fmt, future::Future, pin::pin, sync::{Arc, Mutex, atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering}, mpsc::{self, Receiver, SyncSender}},
    task::{Context, Poll, Wake, Waker}, thread, time::{Duration, Instant}};

#[path = "gpu_color_correction/person_matte.rs"]
mod person_matte;
use person_matte::{Matte, MATTE_BYTES};
#[path = "gpu_color_correction/keyers.rs"]
mod keyers;
use keyers::Keyer;
#[path = "gpu_color_correction/craft.rs"]
mod craft;
use craft::Craft;
#[path = "gpu_color_correction/lut.rs"]
mod lut;
use lut::LutJob;
pub(crate) use lut::{LutLease,LutRetention};
const SHADER: &str = include_str!("gpu_color_correction/color_correction.wgsl");
const MATTE_SHADER: &str = include_str!("gpu_color_correction/person_matte.wgsl");
const COLOR_KEY_SHADER:&str=include_str!("gpu_color_correction/color_key.wgsl");
const CHROMA_KEY_SHADER:&str=include_str!("gpu_color_correction/chroma_key.wgsl");
const LUMA_KEY_SHADER:&str=include_str!("gpu_color_correction/luma_key.wgsl");
const SHARPEN_SHADER:&str=include_str!("gpu_color_correction/sharpen.wgsl");
const CRAFT_SHADER:&str=include_str!("gpu_color_correction/craft.wgsl");
const LUT_SHADER:&str=include_str!("gpu_color_correction/lut.wgsl");
const MAX_FRAME: usize = 1920 * 1080 * 4;
const TABLE_BYTES: usize = 256 * 4 + 65536 * 4;
const PARAM_BYTES: usize = 96;
pub const ALGORITHM: &str = "obs-cc-keyers-sharpen-person-matte-craft14-lut-sdr-wgpu24-v8";
static ENGINE: AtomicBool = AtomicBool::new(false);
static NEXT_DEVICE: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug)]
pub enum Error {
    Busy, Closed, Limit(&'static str), Invalid(MediaError), Unsupported(&'static str),
    Initialization(String), Device(String), Mapping(String), Timeout, Cancelled, Stale,
    /// This particular frame was rejected before the worker accepted it.
    /// Preserve the underlying diagnostic while allowing a bounded CPU retry.
    NotSubmitted(Box<Error>),
}
impl Error { pub fn code(&self)->&'static str { match self {
    Self::Busy=>"gpu_filter_busy",Self::Closed=>"gpu_filter_closed",Self::Limit(_)=>"gpu_filter_limit",
    Self::Invalid(_)=>"gpu_filter_invalid",Self::Unsupported(_)=>"gpu_filter_unsupported",
    Self::Initialization(_)=>"gpu_filter_initialization",Self::Device(_)=>"gpu_filter_device",
    Self::Mapping(_)=>"gpu_filter_mapping",Self::Timeout=>"gpu_filter_timeout",
    Self::Cancelled=>"gpu_filter_cancelled",Self::Stale=>"gpu_filter_stale",
    Self::NotSubmitted(error)=>error.code(),
} }
    pub(crate) fn underlying(&self)->&Self {
        match self { Self::NotSubmitted(error)=>error.underlying(), _=>self }
    }
}
impl fmt::Display for Error { fn fmt(&self,f:&mut fmt::Formatter<'_>)->fmt::Result { write!(f,"{}: {self:?}",self.code()) } }
impl std::error::Error for Error {}

#[derive(Clone, Debug, serde::Serialize)]
pub struct Report { pub backend:String, pub adapter:String, pub driver:String, pub driver_info:String,
    pub device_generation:u64, pub shader_sha256:String, pub algorithm:&'static str }

#[derive(Clone, Copy, Debug)]
pub struct Limits { pub max_frame_bytes:usize, pub max_device_bytes:usize, pub max_cpu_transient_bytes:usize,
    pub max_dispatch_invocations:u64, pub deadline:Duration }
impl Default for Limits { fn default()->Self { Self {max_frame_bytes:MAX_FRAME,
    max_device_bytes:3*MAX_FRAME+TABLE_BYTES+PARAM_BYTES+MATTE_BYTES+2*lut::MAX_LUT_BYTES,
    max_cpu_transient_bytes:2*MAX_FRAME+PARAM_BYTES+2*MATTE_BYTES+65_536*4+lut::MAX_LUT_BYTES+lut::UPLOAD_SCRATCH,max_dispatch_invocations:1920*1080,
    deadline:Duration::from_secs(5)} } }

#[derive(Clone, Debug, PartialEq, Eq)]
struct Key { request:u64, device:u64, input:usize, width:u32, height:u32, ingress:SourceBlend,
    settings:[u32;14], operation:u32, payload_sha256:[u8;32] }
struct Reply { key:Key, result:Result<SourceEffectFrame,Error>, report:Report, _permit:Permit }
enum Operation { ColorCorrection(ColorCorrectionCoefficients), PersonMatte(Matte), Keyer(Keyer), KeyerPair(Keyer,Keyer), PointRun(keyers::PointRun), Craft(Craft), Lut(LutJob) }
struct Request { key:Key, image:SharedVideoFrame, operation:Operation,
    cancelled:Arc<AtomicBool>, reply:SyncSender<Reply>, _permit:Permit }
enum Message { Frame(Box<Request>), Close }
struct Shared { closed:AtomicBool, retired:AtomicBool, admitted:AtomicUsize, next:AtomicU64,
    completed:AtomicU64,generation:u64, failure:Mutex<Option<Error>>, report:Mutex<Option<Report>>, limits:Limits }
struct Permit(Arc<Shared>);
impl Drop for Permit { fn drop(&mut self) { self.0.admitted.fetch_sub(1,Ordering::AcqRel); } }
struct EngineCredit;
impl Drop for EngineCredit { fn drop(&mut self) { ENGINE.store(false,Ordering::Release); } }

/// Sendable pixel-only port. A request owns immutable pixels and exact settings;
/// its unique receipt is checked before returning to the existing render job.
/// The outer Preview/Program owner retains its complete domain freshness fence.
#[derive(Clone)]
pub struct Client { sender:SyncSender<Message>, shared:Arc<Shared> }
pub struct Owner { client:Client, handle:Option<thread::JoinHandle<()>> }
impl Owner {
    pub fn client(&self)->Client { self.client.clone() }
    pub fn request_close(&self) { self.client.shared.closed.store(true,Ordering::Release); let _=self.client.sender.try_send(Message::Close); }
    pub fn retired(&self)->bool { self.client.shared.retired.load(Ordering::Acquire) }
    /// Background/control owner only. Driver teardown has no finite join guarantee.
    pub fn join(mut self)->thread::Result<()> { self.request_close(); self.handle.take().map_or(Ok(()),thread::JoinHandle::join) }
}
impl Drop for Owner { fn drop(&mut self) { self.request_close(); } }

/// Composition-root handle shared by Preview, Program and output owners.
/// Construction is metadata only. The first admitted worker call starts one
/// native service; every clone retains that same Owner through retirement.
#[derive(Clone)]
pub struct Runtime { state:Arc<Mutex<RuntimeState>>, limits:Limits }
struct RuntimeState { owner:Option<Owner>, failure:Option<Error> }
impl Runtime {
    pub fn new(limits:Limits)->Self {
        Self {state:Arc::new(Mutex::new(RuntimeState {owner:None,failure:None})),limits}
    }
    fn client(&self)->Result<Client,Error> {
        let mut state=self.state.lock().map_err(|_|Error::Closed)?;
        if let Some(error)=&state.failure {return Err(error.clone());}
        if state.owner.is_none() {
            match start(self.limits) {
                Ok(owner)=>state.owner=Some(owner),
                Err(error)=>{
                    // A different retiring root may still own the global
                    // credit. Busy declines this call and may be retried.
                    if !matches!(error,Error::Busy) {state.failure=Some(error.clone());}
                    return Err(error);
                }
            }
        }
        state.owner.as_ref().map(Owner::client).ok_or(Error::Closed)
    }
    pub fn supports_frame(&self,input:&RgbaFrame)->bool {
        if input.byte_len()>self.limits.max_frame_bytes || input.width()>4096 || input.height()>4096 {return false;}
        self.state.lock().is_ok_and(|state|state.failure.is_none()
            && state.owner.as_ref().is_none_or(|owner|owner.client.supports_frame(input)))
    }
    pub fn staging_bytes(&self,input:&RgbaFrame)->usize {input.byte_len().saturating_add(PARAM_BYTES)}
    pub(crate) fn identity(&self)->u64 {Arc::as_ptr(&self.state) as usize as u64}
    pub(crate) fn inflight(&self)->usize {
        match self.state.lock() {Ok(state)=>state.owner.as_ref().map_or(0,|owner|owner.client.inflight()),Err(_)=>usize::MAX}
    }
    pub fn report(&self)->Option<Report> {
        self.state.lock().ok().and_then(|state|state.owner.as_ref().and_then(|owner|owner.client.report()))
    }
    pub fn completed_gpu_jobs(&self)->u64 {
        self.state.lock().ok().and_then(|state|state.owner.as_ref().map(|owner|owner.client.completed_gpu_jobs())).unwrap_or(0)
    }
    pub fn apply(&self,input:impl Into<SharedVideoFrame>,ingress:SourceBlend,settings:ColorCorrectionSettings,
        cpu_limits:SourceEffectsLimits,free_cpu_bytes:usize)->Result<(SourceEffectFrame,Report),Error> {
        let input=input.into();
        // Reject the complete point-stage footprint before starting a thread,
        // allocating a device or retaining another frame on the shared service.
        admit(&input,settings,cpu_limits,free_cpu_bytes,self.limits)?;
        self.client()?.apply(input,ingress,settings,cpu_limits,free_cpu_bytes)
    }
}

/// Acquires the process-wide device credit before spawning or allocating a context.
/// At most one native service and two admitted frame calls exist across all hosts.
pub fn start(limits:Limits)->Result<Owner,Error> {
    if !cfg!(any(target_os="windows",target_os="macos")) || !cfg!(target_endian="little") { return Err(Error::Unsupported("DX12/Metal little-endian native targets only")); }
    if limits.max_frame_bytes==0 || limits.max_frame_bytes>MAX_FRAME || limits.max_frame_bytes%4!=0
        || limits.max_device_bytes<TABLE_BYTES+PARAM_BYTES+MATTE_BYTES || limits.max_cpu_transient_bytes<2*TABLE_BYTES
        || limits.max_dispatch_invocations==0 || limits.deadline.is_zero() { return Err(Error::Limit("invalid GPU admission profile")); }
    ENGINE.compare_exchange(false,true,Ordering::AcqRel,Ordering::Acquire).map_err(|_|Error::Busy)?;
    let credit=EngineCredit;
    let generation=NEXT_DEVICE.fetch_update(Ordering::AcqRel,Ordering::Acquire,|v|v.checked_add(1)).map_err(|_|Error::Limit("GPU generation exhausted"))?;
    let shared=Arc::new(Shared {closed:AtomicBool::new(false),retired:AtomicBool::new(false),admitted:AtomicUsize::new(0),next:AtomicU64::new(1),completed:AtomicU64::new(0),generation,
        failure:Mutex::new(None),report:Mutex::new(None),limits});
    let (sender,receiver)=mpsc::sync_channel(1);
    let worker_shared=shared.clone();
    let handle=thread::Builder::new().name("skylive-gpu-color-correction".into()).stack_size(4*1024*1024)
        .spawn(move||worker(receiver,worker_shared,credit)).map_err(|e|Error::Initialization(e.to_string()))?;
    Ok(Owner {client:Client {sender,shared},handle:Some(handle)})
}

fn admit(input:&RgbaFrame,settings:ColorCorrectionSettings,cpu_limits:SourceEffectsLimits,
    free_cpu_bytes:usize,profile:Limits)->Result<(),Error> {
    let required=source_effect_requirements(input,&[SourceVideoEffect::ColorCorrection(settings)]).map_err(Error::Invalid)?;
    if required.output_bytes>cpu_limits.max_output_bytes || required.workspace_bytes>cpu_limits.max_workspace_bytes
        || required.pixel_stages>cpu_limits.max_pixel_stages || required.texel_visits>cpu_limits.max_texel_visits { return Err(Error::Limit("existing CPU point-stage admission")); }
    let bytes=input.byte_len();
    let cpu_peak=bytes.checked_mul(2).and_then(|n|n.checked_add(PARAM_BYTES)).ok_or(Error::Limit("GPU CPU staging overflow"))?.max(2*TABLE_BYTES);
    let device_peak=bytes.checked_mul(3).and_then(|n|n.checked_add(TABLE_BYTES+PARAM_BYTES+MATTE_BYTES)).ok_or(Error::Limit("GPU allocation overflow"))?;
    let invocations=(bytes as u64/4).div_ceil(64)*64;
    if bytes>profile.max_frame_bytes || input.width()>4096 || input.height()>4096
        || cpu_peak>profile.max_cpu_transient_bytes || cpu_peak>free_cpu_bytes
        || device_peak>profile.max_device_bytes || invocations>profile.max_dispatch_invocations { return Err(Error::Limit("GPU frame/staging/device/dispatch admission")); }
    Ok(())
}

impl Client {
    pub(crate) fn identity(&self)->u64 {self.shared.generation}
    pub(crate) fn inflight(&self)->usize {self.shared.admitted.load(Ordering::Acquire)}

    pub fn report(&self)->Option<Report> { self.shared.report.lock().ok().and_then(|v|v.clone()) }
    pub fn completed_gpu_jobs(&self)->u64 {self.shared.completed.load(Ordering::Acquire)}
    pub fn retired(&self)->bool { self.shared.retired.load(Ordering::Acquire) }
    pub fn supports_frame(&self,input:&RgbaFrame)->bool {input.byte_len()<=self.shared.limits.max_frame_bytes && input.width()<=4096 && input.height()<=4096 && !self.shared.closed.load(Ordering::Acquire) && self.shared.failure.lock().is_ok_and(|value|value.is_none())}
    pub fn staging_bytes(&self,input:&RgbaFrame)->usize {input.byte_len().saturating_add(PARAM_BYTES)}
    /// Blocking pixel port, only for the existing CPU render/headless owners.
    /// Run the full ordered-chain preflight before this point-stage admission.
    pub fn apply(&self,input:impl Into<SharedVideoFrame>,ingress:SourceBlend,settings:ColorCorrectionSettings,
        cpu_limits:SourceEffectsLimits,free_cpu_bytes:usize)->Result<(SourceEffectFrame,Report),Error> {
        let input=input.into();
        admit(&input,settings,cpu_limits,free_cpu_bytes,self.shared.limits)?;
        let coefficients=ColorCorrectionCoefficients::new(settings).map_err(Error::Invalid)?;
        let mut authored=[0u32;14];authored[..10].copy_from_slice(&coefficients.authored_bits());
        let permit=self.reserve()?;
        self.submit(input,ingress,authored,Operation::ColorCorrection(coefficients),[0;32],permit)
    }
    fn reserve(&self)->Result<Permit,Error> {
        let failure=self.shared.failure.lock().map_err(|_|Error::NotSubmitted(Box::new(Error::Closed)))?.clone();
        if let Some(error)=failure {return Err(Error::NotSubmitted(Box::new(error)));}
        if self.shared.closed.load(Ordering::Acquire) {return Err(Error::NotSubmitted(Box::new(Error::Closed)));}
        self.shared.admitted.fetch_update(Ordering::AcqRel,Ordering::Acquire,|v|v.checked_add(1).filter(|v|*v<=2)).map_err(|_|Error::Busy)?;
        Ok(Permit(self.shared.clone()))
    }
    fn submit(&self,input:impl Into<SharedVideoFrame>,ingress:SourceBlend,settings:[u32;14],operation:Operation,
        payload_sha256:[u8;32],permit:Permit)->Result<(SourceEffectFrame,Report),Error> {
        let input=input.into();
        let profile=self.shared.limits;
        let request=self.shared.next.fetch_update(Ordering::AcqRel,Ordering::Acquire,|v|v.checked_add(1)).map_err(|_|Error::Limit("GPU request identity exhausted"))?;
        let operation_id=match &operation {Operation::ColorCorrection(_)=>0,Operation::PersonMatte(_)=>1,Operation::Keyer(keyer)=>keyer.operation_id(),Operation::KeyerPair(_,_)=>8,Operation::PointRun(_)=>9,Operation::Craft(_)=>6,Operation::Lut(_)=>7};
        let key=Key {request,device:self.shared.generation,input:input.pixels().as_ptr() as usize,width:input.width(),height:input.height(),ingress,settings,operation:operation_id,payload_sha256};
        let cancelled=Arc::new(AtomicBool::new(false));let (send,receive)=mpsc::sync_channel(1);
        self.sender.try_send(Message::Frame(Box::new(Request {key:key.clone(),image:input,operation,cancelled:cancelled.clone(),reply:send,_permit:permit}))).map_err(|error|match error {
            mpsc::TrySendError::Full(_)=>Error::Busy,
            mpsc::TrySendError::Disconnected(_)=>Error::NotSubmitted(Box::new(
                self.shared.failure.lock().ok().and_then(|failure|failure.clone()).unwrap_or(Error::Closed)
            )),
        })?;
        match receive.recv_timeout(profile.deadline) {
            Ok(reply)=> {let Reply {key:actual,result,report,_permit}=reply;if actual!=key {return Err(Error::Stale)};result.map(|frame|(frame,report))}
            Err(mpsc::RecvTimeoutError::Timeout)=>{cancelled.store(true,Ordering::Release);Err(Error::Timeout)}
            Err(mpsc::RecvTimeoutError::Disconnected)=>Err(self.shared.failure.lock().ok().and_then(|v|v.clone()).unwrap_or(Error::Closed)),
        }
    }
}

struct Slot { capacity:usize,input:wgpu::Buffer,output:wgpu::Buffer,readback:wgpu::Buffer,bindings:wgpu::BindGroup,matte_bindings:wgpu::BindGroup }
struct Native { slot:Option<Slot>,uniform:wgpu::Buffer,decode:wgpu::Buffer,linear:wgpu::Buffer,matte:wgpu::Buffer,lut_table:Option<lut::NativeTable>,
    pipeline:wgpu::ComputePipeline,matte_pipeline:wgpu::ComputePipeline,color_key_pipeline:wgpu::ComputePipeline,chroma_key_pipeline:wgpu::ComputePipeline,luma_key_pipeline:wgpu::ComputePipeline,sharpen_pipeline:wgpu::ComputePipeline,craft_pipeline:wgpu::ComputePipeline,lut_pipeline:wgpu::ComputePipeline,lut_layout:wgpu::BindGroupLayout,layout:wgpu::BindGroupLayout,queue:wgpu::Queue,device:wgpu::Device,
    _instance:wgpu::Instance,signals:Arc<Mutex<Option<Error>>>,report:Report,limits:Limits,
    _thread_only:std::marker::PhantomData<std::rc::Rc<()>> }
impl Native {
    async fn new(limits:Limits,generation:u64)->Result<Self,Error> {
        let backends=if cfg!(target_os="windows") {wgpu::Backends::DX12}else{wgpu::Backends::METAL};
        let instance=wgpu::Instance::new(&wgpu::InstanceDescriptor {backends,..Default::default()});
        let adapter=instance.request_adapter(&wgpu::RequestAdapterOptions {power_preference:wgpu::PowerPreference::HighPerformance,force_fallback_adapter:false,compatible_surface:None}).await.ok_or(Error::Unsupported("no compatible native compute adapter"))?;
        let info=adapter.get_info();
        if info.device_type==wgpu::DeviceType::Cpu {return Err(Error::Unsupported("software adapters are not a hardware GPU backend"));}
        let required=wgpu::Limits {max_storage_buffers_per_shader_stage:5,max_storage_buffer_binding_size:limits.max_frame_bytes.max(MATTE_BYTES).max(lut::MAX_LUT_BYTES) as u32,max_buffer_size:limits.max_frame_bytes.max(MATTE_BYTES).max(lut::MAX_LUT_BYTES) as u64,..wgpu::Limits::downlevel_defaults()};
        let (device,queue)=adapter.request_device(&wgpu::DeviceDescriptor {label:Some("SkyLive point filters"),required_features:wgpu::Features::empty(),required_limits:required,memory_hints:Default::default()},None).await.map_err(|e|Error::Initialization(e.to_string()))?;
        let signals=Arc::new(Mutex::new(None::<Error>));
        let lost=signals.clone();device.set_device_lost_callback(move|reason,detail|{if let Ok(mut slot)=lost.lock(){if slot.is_none(){*slot=Some(Error::Device(format!("{reason:?}: {detail}")));}}});
        let uncaptured=signals.clone();device.on_uncaptured_error(Box::new(move|error:wgpu::Error|{if let Ok(mut slot)=uncaptured.lock(){if slot.is_none(){*slot=Some(Error::Device(format!("{error}")));}}}));
        device.push_error_scope(wgpu::ErrorFilter::Validation);
        let layout=device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {label:Some("CC packed buffers"),entries:&[
            binding(0,wgpu::BufferBindingType::Storage {read_only:true}),binding(1,wgpu::BufferBindingType::Storage {read_only:false}),
            binding(2,wgpu::BufferBindingType::Uniform),binding(3,wgpu::BufferBindingType::Storage {read_only:true}),binding(4,wgpu::BufferBindingType::Storage {read_only:true}),]});
        let pipeline_layout=device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {label:Some("CC compute layout"),bind_group_layouts:&[&layout],push_constant_ranges:&[]});
        let shader=device.create_shader_module(wgpu::ShaderModuleDescriptor {label:Some(ALGORITHM),source:wgpu::ShaderSource::Wgsl(Cow::Borrowed(SHADER))});
        let pipeline=device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {label:Some(ALGORITHM),layout:Some(&pipeline_layout),module:&shader,entry_point:Some("main"),compilation_options:Default::default(),cache:None});
        let matte_shader=device.create_shader_module(wgpu::ShaderModuleDescriptor {label:Some("Person matte SDR"),source:wgpu::ShaderSource::Wgsl(Cow::Borrowed(MATTE_SHADER))});
        let matte_pipeline=device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {label:Some("Person matte SDR"),layout:Some(&pipeline_layout),module:&matte_shader,entry_point:Some("main"),compilation_options:Default::default(),cache:None});
        let color_key_shader=device.create_shader_module(wgpu::ShaderModuleDescriptor{label:Some("OBS Color Key v2 SDR"),source:wgpu::ShaderSource::Wgsl(Cow::Borrowed(COLOR_KEY_SHADER))});
        let color_key_pipeline=device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor{label:Some("OBS Color Key v2 SDR"),layout:Some(&pipeline_layout),module:&color_key_shader,entry_point:Some("main"),compilation_options:Default::default(),cache:None});
        let chroma_key_shader=device.create_shader_module(wgpu::ShaderModuleDescriptor{label:Some("OBS Chroma Key v2 SDR"),source:wgpu::ShaderSource::Wgsl(Cow::Borrowed(CHROMA_KEY_SHADER))});
        let chroma_key_pipeline=device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor{label:Some("OBS Chroma Key v2 SDR"),layout:Some(&pipeline_layout),module:&chroma_key_shader,entry_point:Some("main"),compilation_options:Default::default(),cache:None});
        let luma_key_shader=device.create_shader_module(wgpu::ShaderModuleDescriptor{label:Some("OBS Luma Key v2 SDR"),source:wgpu::ShaderSource::Wgsl(Cow::Borrowed(LUMA_KEY_SHADER))});
        let luma_key_pipeline=device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor{label:Some("OBS Luma Key v2 SDR"),layout:Some(&pipeline_layout),module:&luma_key_shader,entry_point:Some("main"),compilation_options:Default::default(),cache:None});
        let sharpen_shader=device.create_shader_module(wgpu::ShaderModuleDescriptor{label:Some("OBS Sharpen v2 SDR"),source:wgpu::ShaderSource::Wgsl(Cow::Borrowed(SHARPEN_SHADER))});
        let sharpen_pipeline=device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor{label:Some("OBS Sharpen v2 SDR"),layout:Some(&pipeline_layout),module:&sharpen_shader,entry_point:Some("main"),compilation_options:Default::default(),cache:None});
        let craft_shader=device.create_shader_module(wgpu::ShaderModuleDescriptor{label:Some("Craft ordered point kernels"),source:wgpu::ShaderSource::Wgsl(Cow::Borrowed(CRAFT_SHADER))});
        let craft_pipeline=device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor{label:Some("Craft ordered point kernels"),layout:Some(&pipeline_layout),module:&craft_shader,entry_point:Some("main"),compilation_options:Default::default(),cache:None});
        let lut_layout=device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor{label:Some("OBS LUT packed buffers"),entries:&[
            binding(0,wgpu::BufferBindingType::Storage{read_only:true}),binding(1,wgpu::BufferBindingType::Storage{read_only:false}),
            binding(2,wgpu::BufferBindingType::Uniform),binding(3,wgpu::BufferBindingType::Storage{read_only:true}),
            binding(4,wgpu::BufferBindingType::Storage{read_only:true}),binding(5,wgpu::BufferBindingType::Storage{read_only:true})]});
        let lut_pipeline_layout=device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor{label:Some("OBS LUT compute layout"),bind_group_layouts:&[&lut_layout],push_constant_ranges:&[]});
        let lut_shader=device.create_shader_module(wgpu::ShaderModuleDescriptor{label:Some("OBS Apply LUT SDR"),source:wgpu::ShaderSource::Wgsl(Cow::Borrowed(LUT_SHADER))});
        let lut_pipeline=device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor{label:Some("OBS Apply LUT SDR"),layout:Some(&lut_pipeline_layout),module:&lut_shader,entry_point:Some("main"),compilation_options:Default::default(),cache:None});
        if let Some(error)=device.pop_error_scope().await {return Err(Error::Initialization(error.to_string()));}
        let uniform=buffer(&device,"CC parameters",PARAM_BYTES,wgpu::BufferUsages::UNIFORM|wgpu::BufferUsages::COPY_DST);
        let decode=buffer(&device,"CC exact byte decode",256*4,wgpu::BufferUsages::STORAGE|wgpu::BufferUsages::COPY_DST);
        let linear=buffer(&device,"CC exact linear-alpha ingress",65536*4,wgpu::BufferUsages::STORAGE|wgpu::BufferUsages::COPY_DST);
        let matte=buffer(&device,"Person mask and exact axes",MATTE_BYTES,wgpu::BufferUsages::STORAGE|wgpu::BufferUsages::COPY_DST);
        let mut decode_data=Vec::with_capacity(256*4);for byte in 0..=255 {decode_data.extend_from_slice(&color_correction_decode_byte(byte).to_le_bytes());}
        let mut linear_data=Vec::with_capacity(65536*4);for alpha in 0..=255 {for color in 0..=255 {linear_data.extend_from_slice(&u32::from(color_correction_linear_alpha_byte(color,alpha)).to_le_bytes());}}
        queue.write_buffer(&decode,0,&decode_data);queue.write_buffer(&linear,0,&linear_data);queue.submit([]);
        // Finish initialization uploads before per-frame staging admission.
        device.poll(wgpu::Maintain::Wait);
        let report=Report {backend:format!("{:?}",info.backend),adapter:info.name,driver:info.driver,driver_info:info.driver_info,device_generation:generation,shader_sha256:{let mut hash=Sha256::new();hash.update(SHADER.as_bytes());hash.update([0]);hash.update(MATTE_SHADER.as_bytes());hash.update([0]);hash.update(COLOR_KEY_SHADER.as_bytes());hash.update([0]);hash.update(CHROMA_KEY_SHADER.as_bytes());hash.update([0]);hash.update(LUMA_KEY_SHADER.as_bytes());hash.update([0]);hash.update(SHARPEN_SHADER.as_bytes());hash.update([0]);hash.update(CRAFT_SHADER.as_bytes());hash.update([0]);hash.update(LUT_SHADER.as_bytes());format!("{:x}",hash.finalize())},algorithm:ALGORITHM};
        Ok(Self {slot:None,uniform,decode,linear,matte,lut_table:None,pipeline,matte_pipeline,color_key_pipeline,chroma_key_pipeline,luma_key_pipeline,sharpen_pipeline,craft_pipeline,lut_pipeline,lut_layout,layout,queue,device,_instance:instance,signals,report,limits,_thread_only:std::marker::PhantomData})
    }
    fn point_run_pipeline(&self,stage:&keyers::RunStage)->&wgpu::ComputePipeline {
        match stage {
            keyers::RunStage::Correction(_)=>&self.pipeline,
            keyers::RunStage::Keyer(keyer)=>match keyer {
                Keyer::Color(_)=>&self.color_key_pipeline,Keyer::Chroma(_)=>&self.chroma_key_pipeline,
                Keyer::Luma(_)=>&self.luma_key_pipeline,Keyer::Sharpen(_)=>&self.sharpen_pipeline,
            },
        }
    }
    fn ensure_slot(&mut self,bytes:usize,lut_bytes:usize)->Result<(),Error> {
        let max_device_bytes=self.limits.max_device_bytes;
        let fits=|capacity:usize|capacity.checked_mul(3).and_then(|n|n.checked_add(TABLE_BYTES+PARAM_BYTES+MATTE_BYTES)).and_then(|n|n.checked_add(lut_bytes)).is_some_and(|n|n<=max_device_bytes);
        if self.slot.as_ref().is_some_and(|slot|slot.capacity>=bytes&&fits(slot.capacity)) {return Ok(());}
        // Previous requests/maps settle before this method. Retire old capacity
        // before growing; never overlap old/new frame-sized device allocations.
        if let Some(slot)=self.slot.take(){slot.input.destroy();slot.output.destroy();slot.readback.destroy();drop(slot);}
        if !fits(bytes){return Err(Error::Limit("GPU retained frame and LUT slot admission"));}
        let input=buffer(&self.device,"CC input",bytes,wgpu::BufferUsages::STORAGE|wgpu::BufferUsages::COPY_DST|wgpu::BufferUsages::COPY_SRC);
        let output=buffer(&self.device,"CC output",bytes,wgpu::BufferUsages::STORAGE|wgpu::BufferUsages::COPY_SRC);
        let readback=buffer(&self.device,"CC readback",bytes,wgpu::BufferUsages::MAP_READ|wgpu::BufferUsages::COPY_DST);
        let bindings=self.device.create_bind_group(&wgpu::BindGroupDescriptor {label:Some("CC frame"),layout:&self.layout,entries:&[
            wgpu::BindGroupEntry {binding:0,resource:input.as_entire_binding()},wgpu::BindGroupEntry {binding:1,resource:output.as_entire_binding()},
            wgpu::BindGroupEntry {binding:2,resource:self.uniform.as_entire_binding()},wgpu::BindGroupEntry {binding:3,resource:self.decode.as_entire_binding()},wgpu::BindGroupEntry {binding:4,resource:self.linear.as_entire_binding()},]});
        let matte_bindings=self.device.create_bind_group(&wgpu::BindGroupDescriptor {label:Some("Person matte frame"),layout:&self.layout,entries:&[
            wgpu::BindGroupEntry {binding:0,resource:input.as_entire_binding()},wgpu::BindGroupEntry {binding:1,resource:output.as_entire_binding()},
            wgpu::BindGroupEntry {binding:2,resource:self.uniform.as_entire_binding()},wgpu::BindGroupEntry {binding:3,resource:self.decode.as_entire_binding()},wgpu::BindGroupEntry {binding:4,resource:self.matte.as_entire_binding()},]});
        self.slot=Some(Slot {capacity:bytes,input,output,readback,bindings,matte_bindings});Ok(())
    }
    fn apply(&mut self,request:&Request,closing:&AtomicBool)->Result<SourceEffectFrame,Error> {
        if request.cancelled.load(Ordering::Acquire)||closing.load(Ordering::Acquire){return Err(Error::Cancelled);}
        let mut output=Vec::new();output.try_reserve_exact(request.image.byte_len()).map_err(|_|Error::Limit("GPU CPU readback allocation"))?;
        if let Operation::Lut(job)=&request.operation{self.prepare_lut(job,request.image.byte_len())?;}else{
            let retained=self.lut_table.as_ref().map_or(0,|table|table.bytes);
            if request.image.byte_len().checked_mul(3).and_then(|n|n.checked_add(TABLE_BYTES+PARAM_BYTES+MATTE_BYTES+retained)).is_none_or(|n|n>self.limits.max_device_bytes){if let Some(table)=self.lut_table.take(){table.buffer.destroy();}}
            self.ensure_slot(request.image.byte_len(),self.lut_table.as_ref().map_or(0,|table|table.bytes))?;
        }
        let parameters=match &request.operation {
            Operation::ColorCorrection(coefficients)=>parameters(&request.image,request.key.ingress,coefficients),
            Operation::Keyer(keyer)=>keyer.parameters(&request.image,request.key.ingress),
            Operation::KeyerPair(first,_)=>first.parameters(&request.image,request.key.ingress),
            Operation::PointRun(run)=>run.stages[0].parameters(&request.image,request.key.ingress),
            Operation::Craft(craft)=>{self.queue.write_buffer(&self.matte,0,&craft.payload);craft.parameters(&request.image,request.key.ingress)},
            Operation::Lut(job)=>{self.upload_lut(job);job.parameters(&request.image,request.key.ingress)},
            Operation::PersonMatte(matte)=>{
                self.queue.write_buffer(&self.matte,0,&matte.payload);
                matte.parameters(&request.image,request.key.ingress)
            }
        };
        let slot=self.slot.as_ref().ok_or(Error::Closed)?;
        let lut_bindings=if matches!(&request.operation,Operation::Lut(_)){let table=self.lut_table.as_ref().ok_or(Error::Closed)?;Some(self.device.create_bind_group(&wgpu::BindGroupDescriptor{label:Some("OBS LUT frame"),layout:&self.lut_layout,entries:&[
            wgpu::BindGroupEntry{binding:0,resource:slot.input.as_entire_binding()},wgpu::BindGroupEntry{binding:1,resource:slot.output.as_entire_binding()},
            wgpu::BindGroupEntry{binding:2,resource:self.uniform.as_entire_binding()},wgpu::BindGroupEntry{binding:3,resource:self.decode.as_entire_binding()},
            wgpu::BindGroupEntry{binding:4,resource:self.linear.as_entire_binding()},wgpu::BindGroupEntry{binding:5,resource:table.buffer.as_entire_binding()}]}))}else{None};
        self.queue.write_buffer(&slot.input,0,request.image.pixels());self.queue.write_buffer(&self.uniform,0,&parameters);
        let mut encoder=self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {label:Some("CC actual compute/readback")});
        {let mut pass=encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {label:Some(ALGORITHM),timestamp_writes:None});match &request.operation {Operation::ColorCorrection(_)=>{pass.set_pipeline(&self.pipeline);pass.set_bind_group(0,&slot.bindings,&[]);},
            Operation::Lut(_)=>{pass.set_pipeline(&self.lut_pipeline);if let Some(bindings)=&lut_bindings{pass.set_bind_group(0,bindings,&[]);}},
            Operation::Craft(_)=>{pass.set_pipeline(&self.craft_pipeline);pass.set_bind_group(0,&slot.matte_bindings,&[]);},
            Operation::PersonMatte(_)=>{pass.set_pipeline(&self.matte_pipeline);pass.set_bind_group(0,&slot.matte_bindings,&[]);},Operation::PointRun(run)=>{pass.set_pipeline(self.point_run_pipeline(&run.stages[0]));pass.set_bind_group(0,&slot.bindings,&[]);},Operation::Keyer(keyer)|Operation::KeyerPair(keyer,_)=>{pass.set_pipeline(match keyer {Keyer::Color(_)=>&self.color_key_pipeline,Keyer::Chroma(_)=>&self.chroma_key_pipeline,Keyer::Luma(_)=>&self.luma_key_pipeline,Keyer::Sharpen(_)=>&self.sharpen_pipeline});pass.set_bind_group(0,&slot.bindings,&[]);}}pass.dispatch_workgroups((request.image.byte_len() as u32/4).div_ceil(64),1,1);}
        if let Operation::PointRun(run)=&request.operation {
            // Each submission orders the following queue write after the previous
            // dispatch, preserving the same uniform buffer and byte boundary.
            self.queue.submit([encoder.finish()]);
            let reverse=self.device.create_bind_group(&wgpu::BindGroupDescriptor {label:Some("OBS point run reverse"),layout:&self.layout,entries:&[
                wgpu::BindGroupEntry {binding:0,resource:slot.output.as_entire_binding()},wgpu::BindGroupEntry {binding:1,resource:slot.input.as_entire_binding()},
                wgpu::BindGroupEntry {binding:2,resource:self.uniform.as_entire_binding()},wgpu::BindGroupEntry {binding:3,resource:self.decode.as_entire_binding()},wgpu::BindGroupEntry {binding:4,resource:self.linear.as_entire_binding()},]});
            for (offset,keyer) in run.stages[1..run.count].iter().enumerate() {
                let stage=offset+1;
                let parameters=keyer.parameters(&request.image,SourceBlend::PremultipliedLinearSrgb);
                self.queue.write_buffer(&self.uniform,0,&parameters);
                let mut encoder=self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {label:Some("OBS point run")});
                {let mut pass=encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {label:Some(ALGORITHM),timestamp_writes:None});
                    pass.set_pipeline(self.point_run_pipeline(keyer));
                    pass.set_bind_group(0,if stage%2==1 {&reverse}else{&slot.bindings},&[]);
                    pass.dispatch_workgroups((request.image.byte_len() as u32/4).div_ceil(64),1,1);}
                if stage+1==run.count {
                    let final_buffer=if stage%2==1 {&slot.input}else{&slot.output};
                    encoder.copy_buffer_to_buffer(final_buffer,0,&slot.readback,0,request.image.byte_len() as u64);
                }
                self.queue.submit([encoder.finish()]);
            }
        } else if let Operation::KeyerPair(_,second)=&request.operation {
            // Queue ordering makes the second write occur after the first dispatch,
            // so each compute sees its own immutable parameters.
            self.queue.submit([encoder.finish()]);
            let reverse=self.device.create_bind_group(&wgpu::BindGroupDescriptor {label:Some("OBS keyer pair reverse"),layout:&self.layout,entries:&[
                wgpu::BindGroupEntry {binding:0,resource:slot.output.as_entire_binding()},wgpu::BindGroupEntry {binding:1,resource:slot.input.as_entire_binding()},
                wgpu::BindGroupEntry {binding:2,resource:self.uniform.as_entire_binding()},wgpu::BindGroupEntry {binding:3,resource:self.decode.as_entire_binding()},wgpu::BindGroupEntry {binding:4,resource:self.linear.as_entire_binding()},]});
            let second_parameters=second.parameters(&request.image,SourceBlend::PremultipliedLinearSrgb);
            self.queue.write_buffer(&self.uniform,0,&second_parameters);
            let mut second_encoder=self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {label:Some("OBS keyer pair final")});
            {let mut pass=second_encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {label:Some(ALGORITHM),timestamp_writes:None});
                pass.set_pipeline(match second {Keyer::Color(_)=>&self.color_key_pipeline,Keyer::Chroma(_)=>&self.chroma_key_pipeline,Keyer::Luma(_)=>&self.luma_key_pipeline,Keyer::Sharpen(_)=>&self.sharpen_pipeline});
                pass.set_bind_group(0,&reverse,&[]);pass.dispatch_workgroups((request.image.byte_len() as u32/4).div_ceil(64),1,1);}
            second_encoder.copy_buffer_to_buffer(&slot.input,0,&slot.readback,0,request.image.byte_len() as u64);
            self.queue.submit([second_encoder.finish()]);
        } else {
            encoder.copy_buffer_to_buffer(&slot.output,0,&slot.readback,0,request.image.byte_len() as u64);self.queue.submit([encoder.finish()]);
        }
        let slice=slot.readback.slice(0..request.image.byte_len() as u64);let (send,receive)=mpsc::sync_channel(1);
        slice.map_async(wgpu::MapMode::Read,move|result|{let _=send.try_send(result.map_err(|e|e.to_string()));});
        let began=Instant::now();let mut mapping=None;let mut terminal=None;
        loop {
            let empty=self.device.poll(wgpu::Maintain::Poll).is_queue_empty();
            if mapping.is_none(){match receive.try_recv(){Ok(value)=>mapping=Some(value),Err(mpsc::TryRecvError::Empty)=>{},Err(mpsc::TryRecvError::Disconnected)=>mapping=Some(Err("GPU map callback disconnected".into()))}}
            if terminal.is_none(){
                terminal=self.signals.lock().ok().and_then(|value|value.clone());
                if terminal.is_none()&&(request.cancelled.load(Ordering::Acquire)||closing.load(Ordering::Acquire)){terminal=Some(Error::Cancelled);}
                if terminal.is_none()&&began.elapsed()>=self.limits.deadline {terminal=Some(Error::Timeout);}
            }
            // A timeout never releases this job or its global/frame credits.
            // Continue retiring on this background owner until native quiescence.
            if empty&&(mapping.is_some()||matches!(terminal,Some(Error::Device(_)))){break;}
            thread::park_timeout(Duration::from_millis(1));
        }
        let result=if let Some(error)=terminal {Err(error)} else {match mapping.take().ok_or(Error::Closed)? {
            Ok(())=>{let view=slice.get_mapped_range();output.extend_from_slice(&view);drop(view);RgbaFrame::new(request.image.width(),request.image.height(),output).map(|image|SourceEffectFrame {image,blend:match &request.operation {Operation::ColorCorrection(_)|Operation::Keyer(_)|Operation::KeyerPair(_,_)|Operation::PointRun(_)|Operation::Lut(_)=>SourceBlend::PremultipliedLinearSrgb,Operation::PersonMatte(_)|Operation::Craft(_)=>request.key.ingress}}).map_err(Error::Invalid)},
            Err(detail)=>Err(Error::Mapping(detail)),}};
        slot.readback.unmap();result
    }
}
impl Drop for Native { fn drop(&mut self) {self.device.poll(wgpu::Maintain::Wait);if let Some(slot)=self.slot.take(){slot.readback.unmap();slot.input.destroy();slot.output.destroy();slot.readback.destroy();}self.device.destroy();} }
fn worker(receiver:Receiver<Message>,shared:Arc<Shared>,_credit:EngineCredit) {
    // A panic in a driver or wgpu setup must retire the worker and publish a
    // useful diagnostic before dropping queued reply channels. Initialization
    // has not submitted a frame, so PreferGpu may safely use the CPU there.
    // After initialization, preserve the stricter device-failure semantics:
    // a request may already have reached the GPU and must not be duplicated.
    let mut initialized=false;
    let outcome=std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
    let native=block_on(Native::new(shared.limits,shared.generation));
    match native {
        Err(error)=>{if let Ok(mut failure)=shared.failure.lock(){*failure=Some(error);}shared.closed.store(true,Ordering::Release);}
        Ok(mut native)=>{
            initialized=true;
            if let Ok(mut report)=shared.report.lock(){*report=Some(native.report.clone());}
            loop {
                if shared.closed.load(Ordering::Acquire){break;}
                match receiver.recv_timeout(Duration::from_millis(10)) {
                    Ok(Message::Close)=>break,
                    Ok(Message::Frame(request))=>{
                        let result=native.apply(&request,&shared.closed);let report=native.report.clone();
                        if result.is_ok(){shared.completed.fetch_add(1,Ordering::AcqRel);}
                        let Request {key,reply,_permit,..}=*request;
                        let _=reply.try_send(Reply {key,result,report,_permit});
                        if let Some(error)=native.signals.lock().ok().and_then(|value|value.clone()){if let Ok(mut failure)=shared.failure.lock(){*failure=Some(error);}shared.closed.store(true,Ordering::Release);break;}
                    }
                    Err(mpsc::RecvTimeoutError::Timeout)=>{},Err(mpsc::RecvTimeoutError::Disconnected)=>break,
                }
            }
            drop(native);
        }
    }
    }));
    if let Err(payload)=outcome {
        let detail=payload.downcast_ref::<String>().map(String::as_str)
            .or_else(||payload.downcast_ref::<&str>().copied()).unwrap_or("non-string panic");
        let error=if initialized {Error::Device(format!("GPU worker panicked: {detail}"))}
            else {Error::Initialization(format!("GPU worker initialization panicked: {detail}"))};
        if let Ok(mut failure)=shared.failure.lock(){*failure=Some(error);}
        shared.closed.store(true,Ordering::Release);
    }
    // Queued/cancelled inputs are released only on this owner. Native has
    // already dropped before the context credit and completed status retire.
    drop(receiver);drop(_credit);shared.retired.store(true,Ordering::Release);
}
fn binding(binding:u32,ty:wgpu::BufferBindingType)->wgpu::BindGroupLayoutEntry {wgpu::BindGroupLayoutEntry {binding,visibility:wgpu::ShaderStages::COMPUTE,ty:wgpu::BindingType::Buffer {ty,has_dynamic_offset:false,min_binding_size:None},count:None}}
fn buffer(device:&wgpu::Device,label:&str,size:usize,usage:wgpu::BufferUsages)->wgpu::Buffer {device.create_buffer(&wgpu::BufferDescriptor {label:Some(label),size:size as u64,usage,mapped_at_creation:false})}
fn parameters(image:&RgbaFrame,blend:SourceBlend,coefficients:&ColorCorrectionCoefficients)->[u8;PARAM_BYTES] {
    let mut bytes=[0;PARAM_BYTES];let mut offset=0;
    for row in coefficients.matrix(){for value in row{bytes[offset..offset+4].copy_from_slice(&value.to_le_bytes());offset+=4;}}
    bytes[64..68].copy_from_slice(&coefficients.gamma().to_le_bytes());
    let policy:u32=match blend {SourceBlend::LinearSrgb=>0,SourceBlend::Encoded|SourceBlend::EncodedPremultipliedSrgb=>1,SourceBlend::PremultipliedLinearSrgb=>2};
    for (index,value) in [(image.byte_len()/4) as u32,policy,image.width(),image.height()].into_iter().enumerate(){bytes[80+index*4..84+index*4].copy_from_slice(&value.to_le_bytes());}bytes
}
struct ThreadWake(thread::Thread);
impl Wake for ThreadWake {fn wake(self:Arc<Self>){self.0.unpark();}fn wake_by_ref(self:&Arc<Self>){self.0.unpark();}}
fn block_on<F:Future>(future:F)->F::Output {let waker=Waker::from(Arc::new(ThreadWake(thread::current())));let mut context=Context::from_waker(&waker);let mut future=pin!(future);loop {match future.as_mut().poll(&mut context){Poll::Ready(output)=>return output,Poll::Pending=>thread::park_timeout(Duration::from_millis(5)),}}}

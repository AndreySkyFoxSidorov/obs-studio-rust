//! Host-owned shared D3D11 texture readback; game Present never maps/waits here.
use super::*;
use windows::{core::Interface,Win32::{Foundation::{BOOL,HANDLE,HMODULE},Graphics::{Direct3D::*,Direct3D11::*,Dxgi::{*,Common::*}}}};
#[derive(Clone,Copy)]
pub(super) struct Descriptor {pub generation:u64,pub handle:u64,pub width:u32,pub height:u32,pub format:u32,pub adapter:u64,pub sequence:u64,pub qpc:u64}
impl Descriptor {
    pub fn read(h:&wire::Header)->Result<Option<Self>,GameError>{
        for _ in 0..3 {
            let serial=h.load(f::DESCRIPTOR_SERIAL);if serial==0||serial&1!=0{continue;}
            let raw_width=h.load(f::WIDTH);let raw_height=h.load(f::HEIGHT);let raw_format=h.load(f::FORMAT);
            if raw_width>u64::from(u32::MAX)||raw_height>u64::from(u32::MAX)||raw_format>u64::from(u32::MAX){return Err(GameError::Invalid("game descriptor integer range"));}
            let desc=Self {generation:h.load(f::GENERATION),handle:h.load(f::HANDLE),width:raw_width as u32,height:raw_height as u32,
                format:raw_format as u32,adapter:h.load(f::ADAPTER),sequence:h.load(f::SEQUENCE),qpc:h.load(f::QPC)};
            if h.load(f::DESCRIPTOR_SERIAL)!=serial{continue;}
            if desc.generation==0||desc.handle==0||desc.sequence==0||!wire::dimensions(desc.width,desc.height)
                ||!matches!(desc.format,28|29|87|91){return Err(GameError::Invalid("game shared texture descriptor"));}
            return Ok(Some(desc));
        }Ok(None)
    }
}
pub(super) struct Readback {device:ID3D11Device,context:ID3D11DeviceContext,shared:ID3D11Texture2D,keyed:IDXGIKeyedMutex,
    staging:ID3D11Texture2D,query:ID3D11Query,descriptor:Descriptor,pending:bool,held:bool,began:Instant}
fn native(error:windows::core::Error)->GameError{GameError::Native(format!("Game D3D11 HRESULT {:#x}: {error}",error.code().0))}
impl Readback {
    pub fn open(descriptor:Descriptor)->Result<Self,GameError>{
        let factory:IDXGIFactory1=unsafe{CreateDXGIFactory1()}.map_err(native)?;let mut adapter=None;
        for index in 0..16 {
            let current=match unsafe{factory.EnumAdapters1(index)}{Ok(v)=>v,Err(_)=>break};let info=unsafe{current.GetDesc1()}.map_err(native)?;
            let luid=u64::from(info.AdapterLuid.LowPart)|((info.AdapterLuid.HighPart as u32 as u64)<<32);
            if luid==descriptor.adapter {if info.Flags&DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32!=0{return Err(GameError::Unsupported("software game adapters are not supported"));}adapter=Some(current);break;}
        }
        let adapter:IDXGIAdapter=adapter.ok_or(GameError::Unsupported("game graphics adapter is unavailable in the host"))?.cast().map_err(native)?;
        let(mut device,mut context)=(None,None);
        unsafe{D3D11CreateDevice(&adapter,D3D_DRIVER_TYPE_UNKNOWN,HMODULE::default(),D3D11_CREATE_DEVICE_BGRA_SUPPORT,None,D3D11_SDK_VERSION,Some(&mut device),None,Some(&mut context))}.map_err(native)?;
        let device=device.ok_or(GameError::Invalid("game readback device"))?;let context=context.ok_or(GameError::Invalid("game readback context"))?;
        let mut shared:Option<ID3D11Texture2D>=None;
        unsafe{device.OpenSharedResource(HANDLE(descriptor.handle as usize as *mut _),&mut shared)}.map_err(native)?;
        let shared=shared.ok_or(GameError::Invalid("game shared texture"))?;let keyed=shared.cast::<IDXGIKeyedMutex>().map_err(native)?;
        let mut desc=D3D11_TEXTURE2D_DESC::default();unsafe{shared.GetDesc(&mut desc);}
        if desc.Width!=descriptor.width||desc.Height!=descriptor.height||desc.Format.0 as u32!=descriptor.format||desc.ArraySize!=1||desc.MipLevels!=1||desc.SampleDesc.Count!=1 {
            return Err(GameError::Invalid("game shared texture does not match its descriptor"));
        }
        desc.Usage=D3D11_USAGE_STAGING;desc.BindFlags=0;desc.CPUAccessFlags=D3D11_CPU_ACCESS_READ.0 as u32;desc.MiscFlags=0;
        let mut staging=None;unsafe{device.CreateTexture2D(&desc,None,Some(&mut staging))}.map_err(native)?;
        let staging=staging.ok_or(GameError::Invalid("game readback staging texture"))?;
        let mut query=None;unsafe{device.CreateQuery(&D3D11_QUERY_DESC {Query:D3D11_QUERY_EVENT,MiscFlags:0},Some(&mut query))}.map_err(native)?;
        Ok(Self {device,context,shared,keyed,staging,query:query.ok_or(GameError::Invalid("game readback query"))?,descriptor,pending:false,held:false,began:Instant::now()})
    }
    pub fn generation(&self)->u64{self.descriptor.generation}
    fn ready(&mut self)->Result<bool,GameError>{
        if !self.pending{return Ok(true);}let mut done=BOOL(0);
        unsafe{self.context.GetData(&self.query,Some((&mut done as *mut BOOL).cast()),std::mem::size_of::<BOOL>() as u32,D3D11_ASYNC_GETDATA_DONOTFLUSH.0 as u32)}.map_err(native)?;
        if done.as_bool(){self.pending=false;Ok(true)}else{Ok(false)}
    }
    pub fn poll(&mut self,descriptor:Descriptor,h:&wire::Header,free_bytes:usize)->Result<Option<(RgbaFrame,Descriptor)>,GameError>{
        if !self.held {
            let status=unsafe{(Interface::vtable(&self.keyed).AcquireSync)(self.keyed.as_raw(),1,0)}.0;
            if status!=0{return if status==258 {Ok(None)}else{Err(GameError::Native(format!("Game keyed mutex HRESULT {status:#x}")))};}
            self.held=true;
            let locked=Descriptor::read(h)?.ok_or(GameError::Invalid("game descriptor while keyed texture is held"))?;
            if locked.generation!=descriptor.generation||locked.handle!=descriptor.handle||locked.width!=descriptor.width||locked.height!=descriptor.height||locked.format!=descriptor.format {
                unsafe{self.keyed.ReleaseSync(0)}.map_err(native)?;self.held=false;return Ok(None);
            }
            self.descriptor=locked;
            if free_bytes<(descriptor.width as usize*descriptor.height as usize*4){unsafe{self.keyed.ReleaseSync(0)}.map_err(native)?;self.held=false;return Err(GameError::Limit("game resident pixel budget"));}
            unsafe{self.context.CopyResource(&self.staging,&self.shared);self.context.End(&self.query);self.context.Flush();}
            self.pending=true;self.began=Instant::now();
        }
        if !self.ready()? {
            if self.began.elapsed()>Duration::from_secs(5){return Err(GameError::Retiring("game GPU readback timeout; its resources remain owned until retirement".into()));}
            return Ok(None);
        }
        let desc=self.descriptor;let bytes=desc.width as usize*desc.height as usize*4;
        let mut output=Vec::new();output.try_reserve_exact(bytes).map_err(|_|GameError::Limit("game CPU frame allocation"))?;
        let mut mapped=D3D11_MAPPED_SUBRESOURCE::default();
        unsafe{self.context.Map(&self.staging,0,D3D11_MAP_READ,D3D11_MAP_FLAG_DO_NOT_WAIT.0 as u32,Some(&mut mapped))}.map_err(native)?;
        let result=(||->Result<RgbaFrame,GameError>{
            let row=desc.width as usize*4;let pitch=mapped.RowPitch as usize;
            if mapped.pData.is_null()||pitch<row||pitch.checked_mul(desc.height as usize).is_none_or(|size|size>2*wire::MAX_BYTES){return Err(GameError::Invalid("game mapped row layout"));}
            for y in 0..desc.height as usize {
                let source=unsafe{std::slice::from_raw_parts(mapped.pData.cast::<u8>().add(y*pitch),row)};
                output.extend_from_slice(source);
            }
            for pixel in output.chunks_exact_mut(4){if matches!(desc.format,87|91){pixel.swap(0,2);}pixel[3]=255;}
            RgbaFrame::new(desc.width,desc.height,output).map_err(|e|GameError::Native(e.to_string()))
        })();
        unsafe{self.context.Unmap(&self.staging,0);self.keyed.ReleaseSync(0)}.map_err(native)?;self.held=false;result.map(|image|Some((image,desc)))
    }
    pub fn retire(&mut self){
        loop {match self.ready(){Ok(true)=>break,Err(_)=>{if unsafe{self.device.GetDeviceRemovedReason()}.is_err(){break;}},Ok(false)=>{}}
            thread::sleep(Duration::from_millis(2));}
        if self.held{let _=unsafe{self.keyed.ReleaseSync(0)};self.held=false;}
    }
}
impl Drop for Readback{fn drop(&mut self){self.retire();}}

//! Only game backbuffer copies, never CPU readback on Present. One current and
//! one retiring keyed texture, one event fence each. No consumer wait is allowed.
use super::*;
struct Surface {texture:ID3D11Texture2D,keyed:IDXGIKeyedMutex,query:ID3D11Query,desc:D3D11_TEXTURE2D_DESC,
    handle:u64,generation:u64,pending:bool,adapter:u64}
pub(super) struct Gpu {device:ID3D11Device,context:ID3D11DeviceContext,protection:ID3D11Multithread,
    prior_protection:BOOL,current:Option<Surface>,retired:Option<Surface>}
impl Surface {
    fn new(device:&ID3D11Device,mut desc:D3D11_TEXTURE2D_DESC,generation:u64)->::windows::core::Result<Self> {
        desc.MipLevels=1;desc.ArraySize=1;desc.SampleDesc=DXGI_SAMPLE_DESC {Count:1,Quality:0};
        desc.Usage=D3D11_USAGE_DEFAULT;desc.CPUAccessFlags=0;desc.BindFlags=D3D11_BIND_SHADER_RESOURCE.0 as u32;
        desc.MiscFlags=D3D11_RESOURCE_MISC_SHARED_KEYEDMUTEX.0 as u32;
        let mut texture=None;unsafe{device.CreateTexture2D(&desc,None,Some(&mut texture))}?;
        let texture=texture.ok_or_else(||::windows::core::Error::from_hresult(::windows::core::HRESULT(0x80004005u32 as i32)))?;
        let keyed=texture.cast::<IDXGIKeyedMutex>()?;let resource=texture.cast::<IDXGIResource>()?;
        // Legacy DXGI shared handles are borrowed KMT handles. CloseHandle is
        // expressly not used; texture lifetime owns this resource generation.
        let handle=unsafe{resource.GetSharedHandle()}?.0 as usize as u64;
        let dxgi=device.cast::<IDXGIDevice>()?;let adapter=unsafe{dxgi.GetAdapter()}?;
        let info=unsafe{adapter.GetDesc()}?;
        let adapter=u64::from(info.AdapterLuid.LowPart)|((info.AdapterLuid.HighPart as u32 as u64)<<32);
        let mut query=None;unsafe{device.CreateQuery(&D3D11_QUERY_DESC {Query:D3D11_QUERY_EVENT,MiscFlags:0},Some(&mut query))}?;
        let query=query.ok_or_else(||::windows::core::Error::from_hresult(::windows::core::HRESULT(0x80004005u32 as i32)))?;
        Ok(Self {texture,keyed,query,desc,handle,generation,pending:false,adapter})
    }
    fn settled(&mut self,context:&ID3D11DeviceContext,device:&ID3D11Device)->bool {
        if !self.pending{return true;}
        let mut done=BOOL(0);
        match unsafe{context.GetData(&self.query,Some((&mut done as *mut BOOL).cast()),size_of::<BOOL>() as u32,D3D11_ASYNC_GETDATA_DONOTFLUSH.0 as u32)} {
            Ok(()) if done.as_bool()=>{self.pending=false;true},
            Ok(())=>false,
            Err(_)=>unsafe{device.GetDeviceRemovedReason()}.is_err(),
        }
    }
}
impl Gpu {
    pub(super) fn new(device:ID3D11Device,desc:D3D11_TEXTURE2D_DESC,generation:u64)->::windows::core::Result<Self> {
        if unsafe{device.GetCreationFlags()}&D3D11_CREATE_DEVICE_SINGLETHREADED.0 as u32!=0 {
            return Err(::windows::core::Error::from_hresult(::windows::core::HRESULT(0x80004001u32 as i32)));
        }
        let context=unsafe{device.GetImmediateContext()}?;
        let protection=context.cast::<ID3D11Multithread>()?;
        // Allows retirement polling on our worker while the game can still use
        // its immediate context. Restore its original policy after real fences.
        let prior_protection=unsafe{protection.SetMultithreadProtected(BOOL(1))};
        if !unsafe{protection.GetMultithreadProtected()}.as_bool(){
            return Err(::windows::core::Error::from_hresult(::windows::core::HRESULT(0x80004001u32 as i32)));
        }
        let current=match Surface::new(&device,desc,generation) {
            Ok(surface)=>surface,Err(error)=>{unsafe{protection.SetMultithreadProtected(prior_protection);}return Err(error);}
        };
        Ok(Self {device,context,protection,prior_protection,current:Some(current),retired:None})
    }
    pub(super) fn matches(&self,input:&ID3D11Texture2D,desc:D3D11_TEXTURE2D_DESC)->bool {
        let same_device=unsafe{input.GetDevice()}.is_ok_and(|device|device.as_raw()==self.device.as_raw());
        same_device&&self.current.as_ref().is_some_and(|s|s.desc.Width==desc.Width&&s.desc.Height==desc.Height&&s.desc.Format==desc.Format)
    }
    pub(super) fn same_device(&self,device:&ID3D11Device)->bool {self.device.as_raw()==device.as_raw()}
    fn reap(&mut self,h:&Header){
        if self.retired.as_mut().is_some_and(|surface|surface.settled(&self.context,&self.device)&&h.load(f::CONSUMER_GENERATION)>surface.generation){self.retired=None;}
    }
    pub(super) fn can_resize(&mut self,h:&Header)->bool {
        self.reap(h);self.retired.is_none()&&self.current.as_mut().is_none_or(|surface|surface.settled(&self.context,&self.device))
    }
    pub(super) fn resize(&mut self,desc:D3D11_TEXTURE2D_DESC,generation:u64)->Result<(),i32>{
        let next=Surface::new(&self.device,desc,generation).map_err(|e|e.code().0)?;
        self.retired=self.current.take();self.current=Some(next);Ok(())
    }
    pub(super) fn settled(&mut self)->bool {
        self.current.as_mut().is_none_or(|s|s.settled(&self.context,&self.device))
            &&self.retired.as_mut().is_none_or(|s|s.settled(&self.context,&self.device))
    }
    pub(super) fn copy(&mut self,input:&ID3D11Texture2D,h:&Header,qpc:u64)->Result<bool,i32>{
        self.reap(h);let current=self.current.as_mut().ok_or(-50)?;
        if !current.settled(&self.context,&self.device){return Ok(false);}
        // WAIT_TIMEOUT is a positive HRESULT, so windows::Result alone would
        // misclassify it as acquired. Only exact S_OK owns the keyed resource.
        let status=unsafe{(Interface::vtable(&current.keyed).AcquireSync)(current.keyed.as_raw(),0,0)}.0;
        if status!=0{return if status==258 {Ok(false)}else{Err(status)};}
        let mut source_desc=D3D11_TEXTURE2D_DESC::default();unsafe{input.GetDesc(&mut source_desc);}
        unsafe {
            if source_desc.SampleDesc.Count>1 {self.context.ResolveSubresource(&current.texture,0,input,0,current.desc.Format);}
            else{self.context.CopyResource(&current.texture,input);}
            self.context.End(&current.query);self.context.Flush();
        }
        current.pending=true;
        let serial=h.load(f::DESCRIPTOR_SERIAL).checked_add(2).ok_or(-51)?;
        h.store(f::DESCRIPTOR_SERIAL,serial-1);
        h.store(f::HANDLE,current.handle);h.store(f::WIDTH,u64::from(current.desc.Width));h.store(f::HEIGHT,u64::from(current.desc.Height));
        h.store(f::FORMAT,current.desc.Format.0 as u64);h.store(f::ADAPTER,current.adapter);h.store(f::GENERATION,current.generation);
        let sequence=h.load(f::SEQUENCE).checked_add(1).ok_or(-52)?;
        h.store(f::QPC,qpc);h.store(f::SEQUENCE,sequence);h.store(f::DESCRIPTOR_SERIAL,serial);
        // Descriptor publication precedes releasing the texture. Consumer
        // rereads this descriptor after acquiring key1, while producer is gated.
        unsafe{current.keyed.ReleaseSync(1)}.map_err(|e|e.code().0)?;
        h.store(f::STATE,phase::STREAMING);
        Ok(true)
    }
}
impl Drop for Gpu {fn drop(&mut self){
    // The owning worker calls settled until actual GPU retirement first.
    self.current=None;self.retired=None;unsafe{self.protection.SetMultithreadProtected(self.prior_protection);}
}}

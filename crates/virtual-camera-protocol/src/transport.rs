//! Worker-affine file-backed shared frame. No Global file-mapping privilege.
//! Mutex protects every header/payload access across processes. No borrowed
//! slices escape the guard; consumers retain at most one fixed-size copy.
use super::*;
use std::{ffi::c_void,fs::{File,OpenOptions},io,path::{Path,PathBuf},os::windows::{fs::OpenOptionsExt,io::AsRawHandle}};
use windows_core::GUID;
type Handle=*mut c_void;
#[link(name="kernel32")]
unsafe extern "system" {
    fn CreateFileMappingW(file:Handle,attributes:*const c_void,protect:u32,high:u32,low:u32,name:*const u16)->Handle;
    fn MapViewOfFile(mapping:Handle,access:u32,high:u32,low:u32,bytes:usize)->*mut c_void;
    fn UnmapViewOfFile(view:*const c_void)->i32;
    fn CloseHandle(handle:Handle)->i32;
    fn CreateMutexW(attributes:*const SecurityAttributes,owner:i32,name:*const u16)->Handle;
    fn OpenMutexW(access:u32,inherit:i32,name:*const u16)->Handle;
    fn WaitForSingleObject(handle:Handle,timeout:u32)->u32;
    fn ReleaseMutex(handle:Handle)->i32;
    fn LocalFree(memory:Handle)->Handle;
}
#[link(name="advapi32")]
unsafe extern "system" {
    fn ConvertStringSecurityDescriptorToSecurityDescriptorW(value:*const u16,revision:u32,descriptor:*mut Handle,size:*mut u32)->i32;
    fn SetFileSecurityW(path:*const u16,information:u32,descriptor:Handle)->i32;
}
#[link(name="ole32")]
unsafe extern "system" {fn CoCreateGuid(value:*mut GUID)->i32;}
#[repr(C)]struct SecurityAttributes {length:u32,descriptor:Handle,inherit:i32}
struct Security(Handle);
impl Security {
    fn new()->io::Result<Self> {
        let text=wide("D:P(A;;FA;;;OW)(A;;FA;;;SY)(A;;FA;;;LS)");let mut value=std::ptr::null_mut();
        if unsafe{ConvertStringSecurityDescriptorToSecurityDescriptorW(text.as_ptr(),1,&mut value,std::ptr::null_mut())}==0{return Err(io::Error::last_os_error());}
        Ok(Self(value))
    }
}
impl Drop for Security {fn drop(&mut self){unsafe{LocalFree(self.0);}}}
pub fn wide(text:&str)->Vec<u16>{text.encode_utf16().chain(Some(0)).collect()}
#[derive(Clone,Debug)]
pub struct Info {pub file:PathBuf,pub mutex:String,pub token:GUID,pub profile:Profile}
#[derive(Clone,Copy,Debug,Default)]
pub struct Telemetry {pub frames:u64,pub source_starts:u64,pub consumers:u32,pub stopped:bool}
pub struct Mapping {
    file:File,mapping:Handle,view:*mut u8,mutex:Handle,info:Info,producer:bool,
    _owner_thread:std::marker::PhantomData<std::rc::Rc<()>>,
}
struct Guard<'a>(&'a Mapping);
impl Drop for Guard<'_>{fn drop(&mut self){unsafe{ReleaseMutex(self.0.mutex);}}}
impl Mapping {
    pub fn create(profile:Profile)->io::Result<Self> {
        let mut token=GUID::zeroed();if unsafe{CoCreateGuid(&mut token)}<0{return Err(io::Error::other("virtual camera GUID allocation"));}
        let identity=format!("{:032x}",token.to_u128());
        let file=std::env::temp_dir().join(format!("SkyLive-VCam-{identity}.frame"));
        let info=Info {file,mutex:format!("Global\\SkyLive.VCam.{identity}"),token,profile};
        let security=Security::new()?;
        let file=OpenOptions::new().read(true).write(true).create_new(true).share_mode(7).custom_flags(0x04000000).open(&info.file)?;
        file.set_len(MAPPING_BYTES as u64)?;
        let path=wide(&info.file.to_string_lossy());
        if unsafe{SetFileSecurityW(path.as_ptr(),4,security.0)}==0{return Err(io::Error::last_os_error());}
        let attributes=SecurityAttributes {length:std::mem::size_of::<SecurityAttributes>() as u32,descriptor:security.0,inherit:0};
        let name=wide(&info.mutex);let mutex=unsafe{CreateMutexW(&attributes,0,name.as_ptr())};
        if mutex.is_null(){return Err(io::Error::last_os_error());}
        let value=Self::map(file,mutex,info,true)?;
        {let _guard=value.lock()?;unsafe{std::ptr::write_bytes(value.view,0,MAPPING_BYTES);}
            value.write(0,b"SKYVCAM1");value.put32(8,1);value.put32(12,profile.width());value.put32(16,profile.height());value.put32(20,profile.fps());
            value.write(24,&token.to_u128().to_le_bytes());}
        Ok(value)
    }
    pub fn open(info:Info)->io::Result<Self> {
        if !info.file.is_absolute() || info.file.as_os_str().len()>1024 || !info.mutex.starts_with("Global\\SkyLive.VCam.") || info.mutex.len()>128 {
            return Err(io::Error::new(io::ErrorKind::InvalidData,"invalid virtual camera transport identity"));
        }
        let file=OpenOptions::new().read(true).write(true).share_mode(7).open(&info.file)?;
        if file.metadata()?.len()!=MAPPING_BYTES as u64 {return Err(io::Error::new(io::ErrorKind::InvalidData,"virtual camera mapping size"));}
        let name=wide(&info.mutex);let mutex=unsafe{OpenMutexW(0x00100001,0,name.as_ptr())};
        if mutex.is_null(){return Err(io::Error::last_os_error());}
        let value=Self::map(file,mutex,info,false)?;{let _guard=value.lock()?;value.validate()?;}Ok(value)
    }
    fn map(file:File,mutex:Handle,info:Info,producer:bool)->io::Result<Self> {
        let mapping=unsafe{CreateFileMappingW(file.as_raw_handle(),std::ptr::null(),4,0,MAPPING_BYTES as u32,std::ptr::null())};
        if mapping.is_null(){unsafe{CloseHandle(mutex);}return Err(io::Error::last_os_error());}
        let view=unsafe{MapViewOfFile(mapping,0x000f001f,0,0,MAPPING_BYTES)}.cast::<u8>();
        if view.is_null(){unsafe{CloseHandle(mapping);CloseHandle(mutex);}return Err(io::Error::last_os_error());}
        Ok(Self {file,mapping,view,mutex,info,producer,_owner_thread:std::marker::PhantomData})
    }
    fn lock(&self)->io::Result<Guard<'_>> {
        match unsafe{WaitForSingleObject(self.mutex,5)} {
            0=>Ok(Guard(self)),
            0x80=>{unsafe{ReleaseMutex(self.mutex);}Err(io::Error::new(io::ErrorKind::InvalidData,"abandoned virtual camera frame mutex"))},
            0x102=>Err(io::Error::new(io::ErrorKind::WouldBlock,"virtual camera frame in use")),
            _=>Err(io::Error::last_os_error()),
        }
    }
    fn validate(&self)->io::Result<()> {
        let mut magic=[0;8];self.read(0,&mut magic);let mut token=[0;16];self.read(24,&mut token);
        if magic!=*b"SKYVCAM1" || self.get32(8)!=1 || self.get32(12)!=self.info.profile.width() || self.get32(16)!=self.info.profile.height()
            || self.get32(20)!=self.info.profile.fps() || token!=self.info.token.to_u128().to_le_bytes() || self.get32(64)>2 {
            return Err(io::Error::new(io::ErrorKind::InvalidData,"virtual camera frame contract mismatch"));
        }Ok(())
    }
    fn read(&self,offset:usize,output:&mut [u8]){assert!(offset+output.len()<=MAPPING_BYTES);unsafe{std::ptr::copy_nonoverlapping(self.view.add(offset),output.as_mut_ptr(),output.len());}}
    fn write(&self,offset:usize,value:&[u8]){assert!(offset+value.len()<=MAPPING_BYTES);unsafe{std::ptr::copy_nonoverlapping(value.as_ptr(),self.view.add(offset),value.len());}}
    fn get32(&self,offset:usize)->u32{let mut v=[0;4];self.read(offset,&mut v);u32::from_le_bytes(v)}
    fn put32(&self,offset:usize,value:u32){self.write(offset,&value.to_le_bytes());}
    fn get64(&self,offset:usize)->u64{let mut v=[0;8];self.read(offset,&mut v);u64::from_le_bytes(v)}
    fn put64(&self,offset:usize,value:u64){self.write(offset,&value.to_le_bytes());}
    pub fn info(&self)->Info{self.info.clone()}
    pub fn publish(&self,nv12:&[u8],time_100ns:i64)->io::Result<Telemetry> {
        if !self.producer || nv12.len()!=self.info.profile.bytes(){return Err(io::Error::new(io::ErrorKind::InvalidInput,"virtual camera NV12 frame size"));}
        let _guard=self.lock()?;self.validate()?;
        if self.get32(68)!=0{return Err(io::Error::new(io::ErrorKind::BrokenPipe,"virtual camera stopped"));}
        let next=self.get64(40).checked_add(1).ok_or_else(||io::Error::other("virtual camera frame sequence exhausted"))?;
        self.write(HEADER_BYTES,nv12);self.put64(48,time_100ns as u64);self.put32(72,nv12.len() as u32);self.put64(40,next);Ok(self.telemetry_locked())
    }
    pub fn copy_latest(&self,output:&mut [u8])->io::Result<(u64,i64)> {
        if output.len()!=self.info.profile.bytes(){return Err(io::Error::new(io::ErrorKind::InvalidInput,"virtual camera sample allocation"));}
        let _guard=self.lock()?;self.validate()?;
        if self.get32(68)!=0{return Err(io::Error::new(io::ErrorKind::BrokenPipe,"virtual camera stopped"));}
        let frame=self.get64(40);if frame==0{return Err(io::Error::new(io::ErrorKind::WouldBlock,"Program has not produced a frame"));}
        if self.get32(72) as usize!=output.len(){return Err(io::Error::new(io::ErrorKind::InvalidData,"virtual camera sample length"));}
        self.read(HEADER_BYTES,output);Ok((frame,self.get64(48) as i64))
    }
    pub fn consumer(&self,active:bool)->io::Result<()> {
        let _guard=self.lock()?;self.validate()?;let count=self.get32(64);
        if active {if count>=2{return Err(io::Error::new(io::ErrorKind::WouldBlock,"two media source consumers"));}
            let starts=self.get64(56).checked_add(1).ok_or_else(||io::Error::other("consumer identity exhausted"))?;self.put64(56,starts);self.put32(64,count+1);
        }else{self.put32(64,count.saturating_sub(1));}Ok(())
    }
    fn telemetry_locked(&self)->Telemetry{Telemetry {frames:self.get64(40),source_starts:self.get64(56),consumers:self.get32(64),stopped:self.get32(68)!=0}}
    pub fn telemetry(&self)->io::Result<Telemetry>{let _guard=self.lock()?;self.validate()?;Ok(self.telemetry_locked())}
    pub fn stop(&self)->io::Result<()>{if !self.producer{return Ok(());}let _guard=self.lock()?;self.put32(68,1);Ok(())}
}
impl Drop for Mapping {fn drop(&mut self){if self.producer{let _=self.stop();}unsafe{UnmapViewOfFile(self.view.cast());CloseHandle(self.mapping);CloseHandle(self.mutex);}let _=&self.file;}}
pub fn path_text(path:&Path)->io::Result<String>{path.to_str().filter(|v|v.len()<=1024 && !v.contains('\0')).map(str::to_owned).ok_or_else(||io::Error::new(io::ErrorKind::InvalidInput,"virtual camera transport path"))}

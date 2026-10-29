//! Same-user, fixed-size Local namespace mapping. Every cross-process field is
//! atomic; no Rust reference/string/native pointer is part of the wire format.
use super::*;
use std::{ffi::c_void,io};
type Handle=*mut c_void;
#[repr(C)]struct Attributes {length:u32,descriptor:Handle,inherit:i32}
#[link(name="kernel32")]
unsafe extern "system" {
    fn CreateFileMappingW(file:Handle,attributes:*const Attributes,protect:u32,high:u32,low:u32,name:*const u16)->Handle;
    fn OpenFileMappingW(access:u32,inherit:i32,name:*const u16)->Handle;
    fn MapViewOfFile(mapping:Handle,access:u32,high:u32,low:u32,bytes:usize)->Handle;
    fn UnmapViewOfFile(view:*const c_void)->i32;fn CloseHandle(handle:Handle)->i32;
    fn GetLastError()->u32;fn LocalFree(value:Handle)->Handle;
}
#[link(name="advapi32")]
unsafe extern "system" {fn ConvertStringSecurityDescriptorToSecurityDescriptorW(text:*const u16,revision:u32,value:*mut Handle,size:*mut u32)->i32;}
pub struct Mapping {handle:Handle,view:*mut Header}
// Kernel mapping handles have no apartment/thread affinity. Header is entirely
// atomic; the view cannot unmap until the final shared owner has retired.
unsafe impl Send for Mapping {} unsafe impl Sync for Mapping {}
impl Mapping {
    pub fn create(name:&[u16;128])->io::Result<Self>{
        validate_name(name)?;
        let sddl:Vec<u16>="D:P(A;;GA;;;OW)".encode_utf16().chain(Some(0)).collect();let mut descriptor=std::ptr::null_mut();
        if unsafe{ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl.as_ptr(),1,&mut descriptor,std::ptr::null_mut())}==0{return Err(io::Error::last_os_error());}
        let attributes=Attributes {length:std::mem::size_of::<Attributes>() as u32,descriptor,inherit:0};
        let handle=unsafe{CreateFileMappingW((-1isize) as Handle,&attributes,4,0,MAPPING_BYTES as u32,name.as_ptr())};
        let error=unsafe{GetLastError()};unsafe{LocalFree(descriptor);}
        if handle.is_null(){return Err(io::Error::from_raw_os_error(error as i32));}
        if error==183 {unsafe{CloseHandle(handle);}return Err(io::Error::new(io::ErrorKind::AlreadyExists,"game mapping identity already exists"));}
        let mapping=Self::map(handle)?;unsafe{mapping.view.write(Header::default());}Ok(mapping)
    }
    pub fn open(name:&[u16;128])->io::Result<Self>{
        validate_name(name)?;let handle=unsafe{OpenFileMappingW(0x000f001f,0,name.as_ptr())};
        if handle.is_null(){return Err(io::Error::last_os_error());}Self::map(handle)
    }
    fn map(handle:Handle)->io::Result<Self>{
        let view=unsafe{MapViewOfFile(handle,0x000f001f,0,0,MAPPING_BYTES)}.cast::<Header>();
        if view.is_null(){let error=io::Error::last_os_error();unsafe{CloseHandle(handle);}return Err(error);}
        Ok(Self {handle,view})
    }
    pub fn header(&self)->&Header{unsafe{&*self.view}}
}
impl Drop for Mapping {fn drop(&mut self){unsafe{UnmapViewOfFile(self.view.cast());CloseHandle(self.handle);}}}
fn validate_name(name:&[u16;128])->io::Result<()> {
    let end=name.iter().position(|c|*c==0).ok_or_else(||io::Error::new(io::ErrorKind::InvalidInput,"unterminated game mapping name"))?;
    let text=String::from_utf16(&name[..end]).map_err(|_|io::Error::new(io::ErrorKind::InvalidInput,"game mapping name encoding"))?;
    let suffix=text.strip_prefix("Local\\SkyLive.Game.").ok_or_else(||io::Error::new(io::ErrorKind::InvalidInput,"game mapping namespace"))?;
    if suffix.len()!=32||!suffix.bytes().all(|c|c.is_ascii_hexdigit()){return Err(io::Error::new(io::ErrorKind::InvalidInput,"game mapping token"));}Ok(())
}

//! Fixed bounded cross-process ABI. No core, UI, codec or capture dependency.
pub const MAX_NV12_BYTES:usize=1920*1080*3/2;
pub const HEADER_BYTES:usize=256;
pub const MAPPING_BYTES:usize=HEADER_BYTES+MAX_NV12_BYTES;
pub const DLL_NAME:&str="skylive_virtual_camera_source.dll";
pub const CLASS_ID:&str="{EA2290EA-190A-4312-9C23-B05D612B4153}";
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub struct Profile {width:u32,height:u32,fps:u32}
impl Profile {
    pub fn new(width:u32,height:u32,fps:u32)->Option<Self> {
        (matches!((width,height),(1280,720)|(1920,1080)) && fps==30).then_some(Self {width,height,fps})
    }
    pub fn width(self)->u32{self.width}
    pub fn height(self)->u32{self.height}
    pub fn fps(self)->u32{self.fps}
    pub fn bytes(self)->usize{self.width as usize*self.height as usize*3/2}
    pub fn duration(self)->i64{10_000_000/i64::from(self.fps)}
}
#[cfg(target_os="windows")]
pub mod transport;
#[cfg(target_os="windows")]
pub mod installation;
#[cfg(target_os="windows")]
pub mod ids {
    use windows_core::GUID;
    pub const CLASS:GUID=GUID::from_u128(0xea2290ea_190a_4312_9c23_b05d612b4153);
    pub const FILE:GUID=GUID::from_u128(0x87b71d99_8f85_49bb_b20c_67a112b9dd10);
    pub const MUTEX:GUID=GUID::from_u128(0x87b71d99_8f85_49bb_b20c_67a112b9dd11);
    pub const TOKEN:GUID=GUID::from_u128(0x87b71d99_8f85_49bb_b20c_67a112b9dd12);
    pub const WIDTH:GUID=GUID::from_u128(0x87b71d99_8f85_49bb_b20c_67a112b9dd13);
    pub const HEIGHT:GUID=GUID::from_u128(0x87b71d99_8f85_49bb_b20c_67a112b9dd14);
    pub const FPS:GUID=GUID::from_u128(0x87b71d99_8f85_49bb_b20c_67a112b9dd15);
}

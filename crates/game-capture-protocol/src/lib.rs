//! Fixed x64 same-user game-hook ABI. No framework, image cache or source IDs.
use std::sync::atomic::{AtomicU64,Ordering};
pub const DLL_NAME:&str="skylive_game_hook.dll";
pub const ABI:u64=1;
pub const MAGIC:u64=u64::from_le_bytes(*b"SKYGAME1");
pub const INIT_MAGIC:u64=u64::from_le_bytes(*b"SKYGSTA1");
pub const MAX_EDGE:u32=4096;
pub const MAX_PIXELS:u64=8_847_360;
pub const MAX_BYTES:usize=MAX_PIXELS as usize*4;
pub const MAX_PINNED_TARGETS:usize=2;
pub const MAPPING_BYTES:usize=4096;
pub mod field {
    pub const MAGIC:usize=0;pub const ABI:usize=1;pub const HOST_PID:usize=2;pub const PID:usize=3;
    pub const CREATION:usize=4;pub const HWND:usize=5;pub const TOKEN0:usize=6;pub const TOKEN1:usize=7;
    pub const STOP:usize=8;pub const STATE:usize=9;pub const GENERATION:usize=10;pub const HANDLE:usize=11;
    pub const WIDTH:usize=12;pub const HEIGHT:usize=13;pub const FORMAT:usize=14;pub const ADAPTER:usize=15;
    pub const SEQUENCE:usize=16;pub const QPC:usize=17;pub const CONSUMER_GENERATION:usize=18;pub const DROPPED:usize=19;
    pub const ERROR:usize=20;pub const HEARTBEAT:usize=21;pub const FPS:usize=22;pub const HOST_CREATION:usize=23;
    pub const DESCRIPTOR_SERIAL:usize=24;pub const THROTTLED:usize=25;pub const CALLBACKS:usize=26;
}
pub mod state {pub const INITIAL:u64=0;pub const HOOKED:u64=1;pub const STREAMING:u64=2;pub const STOPPING:u64=3;pub const RETIRED:u64=4;pub const FAILED:u64=5;}
#[repr(C,align(64))]
pub struct Header {values:[AtomicU64;32]}
impl Default for Header {fn default()->Self{Self {values:std::array::from_fn(|_|AtomicU64::new(0))}}}
impl Header {
    pub fn load(&self,index:usize)->u64{self.values[index].load(Ordering::Acquire)}
    pub fn store(&self,index:usize,value:u64){self.values[index].store(value,Ordering::Release);}
    pub fn add(&self,index:usize,value:u64)->u64{self.values[index].fetch_add(value,Ordering::AcqRel)}
}
#[repr(C)]
#[derive(Clone,Copy)]
pub struct Init {
    pub magic:u64,pub abi:u64,pub size:u32,pub pid:u32,pub creation:u64,pub hwnd:u64,
    pub host_pid:u32,pub fps:u32,pub host_creation:u64,pub token:[u64;2],
    pub mapping:[u16;128],pub executable:[u16;4096],
}
impl Default for Init {
    fn default()->Self{Self {magic:INIT_MAGIC,abi:ABI,size:std::mem::size_of::<Self>() as u32,pid:0,creation:0,hwnd:0,
        host_pid:0,fps:30,host_creation:0,token:[0;2],mapping:[0;128],executable:[0;4096]}}
}
impl Init {
    pub fn valid(&self)->bool{self.magic==INIT_MAGIC&&self.abi==ABI&&self.size as usize==std::mem::size_of::<Self>()
        &&self.pid>4&&self.host_pid>4&&self.pid!=self.host_pid&&self.creation!=0&&self.host_creation!=0&&self.hwnd!=0
        &&(1..=120).contains(&self.fps)&&self.token!=[0;2]&&self.mapping[0]!=0&&self.mapping.contains(&0)
        &&self.executable[0]!=0&&self.executable.contains(&0)}
}
pub fn dimensions(width:u32,height:u32)->bool{width>0&&height>0&&width<=MAX_EDGE&&height<=MAX_EDGE&&u64::from(width)*u64::from(height)<=MAX_PIXELS}
#[cfg(all(target_os="windows",target_arch="x86_64"))]
pub mod mapping;

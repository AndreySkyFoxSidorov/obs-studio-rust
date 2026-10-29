//! Authored OBS Scaling / Aspect Ratio. Canvas arrives explicitly from its owner.
use crate::Error;
use serde::{Serialize,Deserialize};
#[derive(Clone,Copy,Debug,Default,PartialEq,Eq,Hash,Serialize,Deserialize)]
#[serde(tag="mode",rename_all="snake_case",deny_unknown_fields)]
pub enum ScaleFilterTarget {#[default] None,Canvas,Resolution{width:u32,height:u32},Aspect{width:u32,height:u32}}
impl ScaleFilterTarget {
    pub fn validate(self)->Result<(),Error>{match self {
        Self::None|Self::Canvas=>Ok(()),
        Self::Resolution{width,height}|Self::Aspect{width,height}=>if (1..=8192).contains(&width)&&(1..=8192).contains(&height){
            if matches!(self,Self::Resolution{..})&&u64::from(width)*u64::from(height)>32*1024*1024{return Err(Error::Limit("scaled image exceeds 8192-edge/32M-pixel limits"));}Ok(())
        }else{Err(Error::Invalid("scale filter dimensions/aspect terms must be1..8192"))},
    }}
    pub fn output(self,input:(u32,u32),canvas:Option<(u32,u32)>)->Result<(u32,u32),Error>{
        self.validate()?;if input.0==0||input.1==0{return Err(Error::Invalid("scale filter input dimensions are empty"));}
        let output=match self {
            Self::None=>input,
            Self::Canvas=>canvas.ok_or(Error::Invalid("Canvas scale filter requires explicit canvas dimensions"))?,
            Self::Resolution{width,height}=>(width,height),
            Self::Aspect{width,height}=>{
                let old=f64::from(input.0)/f64::from(input.1);let new=f64::from(width)/f64::from(height);
                if (old-new).abs()<=f64::from(1e-4f32){input}
                else if new>old{((f64::from(input.1)*new) as u32,input.1)}
                else{(input.0,(f64::from(input.0)/new) as u32)}
            }
        };
        if output.0==0||output.1==0||output.0>8192||output.1>8192||u64::from(output.0)*u64::from(output.1)>32*1024*1024{return Err(Error::Limit("scaled image exceeds 8192-edge/32M-pixel limits"));}
        Ok(output)
    }
    pub fn skips(self,input:(u32,u32))->bool{match self {Self::None=>true,Self::Aspect{width,height}=>input.1!=0&&height!=0&&(f64::from(input.0)/f64::from(input.1)-f64::from(width)/f64::from(height)).abs()<=f64::from(1e-4f32),_=>false}}
}
#[derive(Clone,Copy,Debug,Default,PartialEq,Eq,Hash,Serialize,Deserialize)]
#[serde(rename_all="snake_case")]
pub enum ScaleFilterSampling{Point,Bilinear,#[default] Bicubic,Lanczos,Area}
impl ScaleFilterSampling{
    pub const ALL:[Self;5]=[Self::Point,Self::Bilinear,Self::Bicubic,Self::Lanczos,Self::Area];
    pub fn label(self)->&'static str{match self {Self::Point=>"Point",Self::Bilinear=>"Bilinear",Self::Bicubic=>"Bicubic",Self::Lanczos=>"Lanczos",Self::Area=>"Area"}}
    pub fn wire(self)->&'static str{match self {Self::Point=>"point",Self::Bilinear=>"bilinear",Self::Bicubic=>"bicubic",Self::Lanczos=>"lanczos",Self::Area=>"area"}}
    pub fn allows_undistort(self)->bool{matches!(self,Self::Bicubic|Self::Lanczos)}
}

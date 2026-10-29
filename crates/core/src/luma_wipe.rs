//! Exact stock OBS32 luma masks and validated authored softness. No host imports.
use crate::Error;
use serde::{Serialize,Deserialize};
#[derive(Clone,Copy,Debug,PartialEq,Eq,Hash,Serialize,Deserialize)]
pub enum LumaWipePattern {
    #[serde(rename="barndoor-botleft.png")]
    BarndoorBottomLeft,
    #[serde(rename="barndoor-h.png")]
    BarndoorHorizontal,
    #[serde(rename="barndoor-topleft.png")]
    BarndoorTopLeft,
    #[serde(rename="barndoor-v.png")]
    BarndoorVertical,
    #[serde(rename="blinds-h.png")]
    BlindsHorizontal,
    #[serde(rename="box-botleft.png")]
    BoxBottomLeft,
    #[serde(rename="box-botright.png")]
    BoxBottomRight,
    #[serde(rename="box-topleft.png")]
    BoxTopLeft,
    #[serde(rename="box-topright.png")]
    BoxTopRight,
    #[serde(rename="burst.png")]
    Burst,
    #[serde(rename="checkerboard-small.png")]
    CheckerboardSmall,
    #[serde(rename="circles.png")]
    Circles,
    #[serde(rename="clock.png")]
    Clock,
    #[serde(rename="cloud.png")]
    Cloud,
    #[serde(rename="curtain.png")]
    Curtain,
    #[serde(rename="fan.png")]
    Fan,
    #[serde(rename="fractal.png")]
    Fractal,
    #[serde(rename="iris.png")]
    Iris,
    #[serde(rename="linear-h.png")]
    LinearHorizontal,
    #[serde(rename="linear-topleft.png")]
    LinearTopLeft,
    #[serde(rename="linear-topright.png")]
    LinearTopRight,
    #[serde(rename="linear-v.png")]
    LinearVertical,
    #[serde(rename="parallel-zigzag-h.png")]
    ParallelZigzagHorizontal,
    #[serde(rename="parallel-zigzag-v.png")]
    ParallelZigzagVertical,
    #[serde(rename="sinus9.png")]
    Sinus9,
    #[serde(rename="spiral.png")]
    Spiral,
    #[serde(rename="square.png")]
    Square,
    #[serde(rename="squares.png")]
    Squares,
    #[serde(rename="stripes.png")]
    Stripes,
    #[serde(rename="strips-h.png")]
    StripsHorizontal,
    #[serde(rename="strips-v.png")]
    StripsVertical,
    #[serde(rename="watercolor.png")]
    Watercolor,
    #[serde(rename="zigzag-h.png")]
    ZigzagHorizontal,
    #[serde(rename="zigzag-v.png")]
    ZigzagVertical,
}
impl Default for LumaWipePattern{fn default()->Self{Self::LinearHorizontal}}
impl LumaWipePattern{
    pub const ALL:[Self;34]=[Self::BarndoorBottomLeft,Self::BarndoorHorizontal,Self::BarndoorTopLeft,Self::BarndoorVertical,Self::BlindsHorizontal,Self::BoxBottomLeft,Self::BoxBottomRight,Self::BoxTopLeft,Self::BoxTopRight,Self::Burst,Self::CheckerboardSmall,Self::Circles,Self::Clock,Self::Cloud,Self::Curtain,Self::Fan,Self::Fractal,Self::Iris,Self::LinearHorizontal,Self::LinearTopLeft,Self::LinearTopRight,Self::LinearVertical,Self::ParallelZigzagHorizontal,Self::ParallelZigzagVertical,Self::Sinus9,Self::Spiral,Self::Square,Self::Squares,Self::Stripes,Self::StripsHorizontal,Self::StripsVertical,Self::Watercolor,Self::ZigzagHorizontal,Self::ZigzagVertical];
    pub fn filename(self)->&'static str{match self{
        Self::BarndoorBottomLeft=>"barndoor-botleft.png",
        Self::BarndoorHorizontal=>"barndoor-h.png",
        Self::BarndoorTopLeft=>"barndoor-topleft.png",
        Self::BarndoorVertical=>"barndoor-v.png",
        Self::BlindsHorizontal=>"blinds-h.png",
        Self::BoxBottomLeft=>"box-botleft.png",
        Self::BoxBottomRight=>"box-botright.png",
        Self::BoxTopLeft=>"box-topleft.png",
        Self::BoxTopRight=>"box-topright.png",
        Self::Burst=>"burst.png",
        Self::CheckerboardSmall=>"checkerboard-small.png",
        Self::Circles=>"circles.png",
        Self::Clock=>"clock.png",
        Self::Cloud=>"cloud.png",
        Self::Curtain=>"curtain.png",
        Self::Fan=>"fan.png",
        Self::Fractal=>"fractal.png",
        Self::Iris=>"iris.png",
        Self::LinearHorizontal=>"linear-h.png",
        Self::LinearTopLeft=>"linear-topleft.png",
        Self::LinearTopRight=>"linear-topright.png",
        Self::LinearVertical=>"linear-v.png",
        Self::ParallelZigzagHorizontal=>"parallel-zigzag-h.png",
        Self::ParallelZigzagVertical=>"parallel-zigzag-v.png",
        Self::Sinus9=>"sinus9.png",
        Self::Spiral=>"spiral.png",
        Self::Square=>"square.png",
        Self::Squares=>"squares.png",
        Self::Stripes=>"stripes.png",
        Self::StripsHorizontal=>"strips-h.png",
        Self::StripsVertical=>"strips-v.png",
        Self::Watercolor=>"watercolor.png",
        Self::ZigzagHorizontal=>"zigzag-h.png",
        Self::ZigzagVertical=>"zigzag-v.png",
    }}
    pub fn label(self)->&'static str{match self{
        Self::BarndoorBottomLeft=>"Barndoor Bottom Left",
        Self::BarndoorHorizontal=>"Barndoor Horizontal",
        Self::BarndoorTopLeft=>"Barndoor Top Left",
        Self::BarndoorVertical=>"Barndoor Vertical",
        Self::BlindsHorizontal=>"Blinds Horizontal",
        Self::BoxBottomLeft=>"Box Bottom Left",
        Self::BoxBottomRight=>"Box Bottom Right",
        Self::BoxTopLeft=>"Box Top Left",
        Self::BoxTopRight=>"Box Top Right",
        Self::Burst=>"Burst",
        Self::CheckerboardSmall=>"Checkerboard Small",
        Self::Circles=>"Circles",
        Self::Clock=>"Clock",
        Self::Cloud=>"Cloud",
        Self::Curtain=>"Curtain",
        Self::Fan=>"Fan",
        Self::Fractal=>"Fractal",
        Self::Iris=>"Iris",
        Self::LinearHorizontal=>"Linear Horizontal",
        Self::LinearTopLeft=>"Linear Top Left",
        Self::LinearTopRight=>"Linear Top Right",
        Self::LinearVertical=>"Linear Vertical",
        Self::ParallelZigzagHorizontal=>"Parallel Zigzag Horizontal",
        Self::ParallelZigzagVertical=>"Parallel Zigzag Vertical",
        Self::Sinus9=>"Sinus 9",
        Self::Spiral=>"Spiral",
        Self::Square=>"Square",
        Self::Squares=>"Squares",
        Self::Stripes=>"Stripes",
        Self::StripsHorizontal=>"Strips Horizontal",
        Self::StripsVertical=>"Strips Vertical",
        Self::Watercolor=>"Watercolor",
        Self::ZigzagHorizontal=>"Zigzag Horizontal",
        Self::ZigzagVertical=>"Zigzag Vertical",
    }}
    pub fn locale_key(self)->&'static str{match self{
        Self::BarndoorBottomLeft=>"Module.obs-transitions.LumaWipe.Type.BarndoorBottomLeft",
        Self::BarndoorHorizontal=>"Module.obs-transitions.LumaWipe.Type.BarndoorHorizontal",
        Self::BarndoorTopLeft=>"Module.obs-transitions.LumaWipe.Type.BarndoorTopLeft",
        Self::BarndoorVertical=>"Module.obs-transitions.LumaWipe.Type.BarndoorVertical",
        Self::BlindsHorizontal=>"Module.obs-transitions.LumaWipe.Type.BlindsHorizontal",
        Self::BoxBottomLeft=>"Module.obs-transitions.LumaWipe.Type.BoxBottomLeft",
        Self::BoxBottomRight=>"Module.obs-transitions.LumaWipe.Type.BoxBottomRight",
        Self::BoxTopLeft=>"Module.obs-transitions.LumaWipe.Type.BoxTopLeft",
        Self::BoxTopRight=>"Module.obs-transitions.LumaWipe.Type.BoxTopRight",
        Self::Burst=>"Module.obs-transitions.LumaWipe.Type.Burst",
        Self::CheckerboardSmall=>"Module.obs-transitions.LumaWipe.Type.CheckerboardSmall",
        Self::Circles=>"Module.obs-transitions.LumaWipe.Type.Circles",
        Self::Clock=>"Module.obs-transitions.LumaWipe.Type.Clock",
        Self::Cloud=>"Module.obs-transitions.LumaWipe.Type.Cloud",
        Self::Curtain=>"Module.obs-transitions.LumaWipe.Type.Curtain",
        Self::Fan=>"Module.obs-transitions.LumaWipe.Type.Fan",
        Self::Fractal=>"Module.obs-transitions.LumaWipe.Type.Fractal",
        Self::Iris=>"Module.obs-transitions.LumaWipe.Type.Iris",
        Self::LinearHorizontal=>"Module.obs-transitions.LumaWipe.Type.LinearHorizontal",
        Self::LinearTopLeft=>"Module.obs-transitions.LumaWipe.Type.LinearTopLeft",
        Self::LinearTopRight=>"Module.obs-transitions.LumaWipe.Type.LinearTopRight",
        Self::LinearVertical=>"Module.obs-transitions.LumaWipe.Type.LinearVertical",
        Self::ParallelZigzagHorizontal=>"Module.obs-transitions.LumaWipe.Type.ParallelZigzagHorizontal",
        Self::ParallelZigzagVertical=>"Module.obs-transitions.LumaWipe.Type.ParallelZigzagVertical",
        Self::Sinus9=>"Module.obs-transitions.LumaWipe.Type.Sinus9",
        Self::Spiral=>"Module.obs-transitions.LumaWipe.Type.Spiral",
        Self::Square=>"Module.obs-transitions.LumaWipe.Type.Square",
        Self::Squares=>"Module.obs-transitions.LumaWipe.Type.Squares",
        Self::Stripes=>"Module.obs-transitions.LumaWipe.Type.Stripes",
        Self::StripsHorizontal=>"Module.obs-transitions.LumaWipe.Type.StripsHorizontal",
        Self::StripsVertical=>"Module.obs-transitions.LumaWipe.Type.StripsVertical",
        Self::Watercolor=>"Module.obs-transitions.LumaWipe.Type.Watercolor",
        Self::ZigzagHorizontal=>"Module.obs-transitions.LumaWipe.Type.ZigzagHorizontal",
        Self::ZigzagVertical=>"Module.obs-transitions.LumaWipe.Type.ZigzagVertical",
    }}
    pub fn from_filename(value:&str)->Result<Self,Error>{Self::ALL.into_iter().find(|p|p.filename()==value).ok_or(Error::Invalid("unknown stock Luma Wipe mask"))}
}
/// Finite0..1 authored f32 with normalized zero; its bit representation is an
/// equivalence relation, so Copy/Eq/Hash remain valid on TransitionKind.
#[derive(Clone,Copy,Debug,Serialize,Deserialize)]
#[serde(try_from="f32",into="f32")]
pub struct LumaWipeSoftness(f32);
impl LumaWipeSoftness{
    pub fn new(value:f32)->Result<Self,Error>{if !value.is_finite()||!(0.0..=1.0).contains(&value){Err(Error::Invalid("Luma Wipe softness must be finite0..1"))}else{Ok(Self(if value==0.0{0.0}else{value}))}}
    pub fn get(self)->f32{self.0}
}
impl Default for LumaWipeSoftness{fn default()->Self{Self(0.03)}}
impl TryFrom<f32> for LumaWipeSoftness{type Error=Error;fn try_from(value:f32)->Result<Self,Error>{Self::new(value)}}
impl From<LumaWipeSoftness> for f32{fn from(value:LumaWipeSoftness)->f32{value.0}}
impl PartialEq for LumaWipeSoftness{fn eq(&self,other:&Self)->bool{self.0.to_bits()==other.0.to_bits()}}
impl Eq for LumaWipeSoftness{}
impl std::hash::Hash for LumaWipeSoftness{fn hash<H:std::hash::Hasher>(&self,state:&mut H){std::hash::Hash::hash(&self.0.to_bits(),state);}}

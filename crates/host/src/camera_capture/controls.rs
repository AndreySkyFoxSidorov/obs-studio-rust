//! Live device state, not authored source settings or scene Undo.
use super::{CameraError, CameraRequestKey};
use skylive_core::{Id, VideoCaptureBinding};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, atomic::{AtomicU8, Ordering}};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all="snake_case")]
pub enum CameraProperty { Pan, Tilt, Roll, Zoom, Exposure, Iris, Focus }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all="snake_case")]
pub enum ProcAmpProperty { Brightness, Contrast, Hue, Saturation, Sharpness, Gamma, ColorEnable, WhiteBalance, BacklightCompensation, Gain }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag="family", content="property", rename_all="snake_case")]
pub enum CameraControlId { Camera(CameraProperty), ProcAmp(ProcAmpProperty) }
impl CameraControlId {
    pub const ALL:[Self;17]=[
        Self::Camera(CameraProperty::Exposure),Self::Camera(CameraProperty::Focus),Self::Camera(CameraProperty::Zoom),
        Self::Camera(CameraProperty::Pan),Self::Camera(CameraProperty::Tilt),Self::Camera(CameraProperty::Roll),Self::Camera(CameraProperty::Iris),
        Self::ProcAmp(ProcAmpProperty::WhiteBalance),Self::ProcAmp(ProcAmpProperty::Brightness),Self::ProcAmp(ProcAmpProperty::Contrast),
        Self::ProcAmp(ProcAmpProperty::Hue),Self::ProcAmp(ProcAmpProperty::Saturation),Self::ProcAmp(ProcAmpProperty::Sharpness),
        Self::ProcAmp(ProcAmpProperty::Gamma),Self::ProcAmp(ProcAmpProperty::ColorEnable),Self::ProcAmp(ProcAmpProperty::BacklightCompensation),Self::ProcAmp(ProcAmpProperty::Gain)];
    pub fn label(self)->&'static str { match self {
        Self::Camera(p)=>match p {CameraProperty::Pan=>"Pan",CameraProperty::Tilt=>"Tilt",CameraProperty::Roll=>"Roll",CameraProperty::Zoom=>"Zoom",CameraProperty::Exposure=>"Exposure",CameraProperty::Iris=>"Iris",CameraProperty::Focus=>"Focus"},
        Self::ProcAmp(p)=>match p {ProcAmpProperty::Brightness=>"Brightness",ProcAmpProperty::Contrast=>"Contrast",ProcAmpProperty::Hue=>"Hue",ProcAmpProperty::Saturation=>"Saturation",ProcAmpProperty::Sharpness=>"Sharpness",ProcAmpProperty::Gamma=>"Gamma",ProcAmpProperty::ColorEnable=>"Color Enable",ProcAmpProperty::WhiteBalance=>"White Balance",ProcAmpProperty::BacklightCompensation=>"Backlight Compensation",ProcAmpProperty::Gain=>"Gain"}
    }}
    pub(super) fn native(self)->(bool,i32) { match self {
        Self::Camera(p)=>(true,match p {CameraProperty::Pan=>0,CameraProperty::Tilt=>1,CameraProperty::Roll=>2,CameraProperty::Zoom=>3,CameraProperty::Exposure=>4,CameraProperty::Iris=>5,CameraProperty::Focus=>6}),
        Self::ProcAmp(p)=>(false,match p {ProcAmpProperty::Brightness=>0,ProcAmpProperty::Contrast=>1,ProcAmpProperty::Hue=>2,ProcAmpProperty::Saturation=>3,ProcAmpProperty::Sharpness=>4,ProcAmpProperty::Gamma=>5,ProcAmpProperty::ColorEnable=>6,ProcAmpProperty::WhiteBalance=>7,ProcAmpProperty::BacklightCompensation=>8,ProcAmpProperty::Gain=>9})
    }}
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all="snake_case")]
pub enum ControlMode { Auto, Manual }
impl ControlMode {
    pub(super) fn flags(self)->i32 {match self {Self::Auto=>1,Self::Manual=>2}}
    pub(super) fn from_flags(flags:i32)->Option<Self>{match flags{1=>Some(Self::Auto),2=>Some(Self::Manual),_=>None}}
}
#[derive(Clone, Copy, Debug, Serialize)]
pub struct ControlRange { pub minimum:i32,pub maximum:i32,pub step:i32,pub default:i32,pub flags:i32 }
impl ControlRange {
    pub fn valid(self)->bool {self.minimum<=self.maximum&&self.step>0&&self.default>=self.minimum&&self.default<=self.maximum&&self.flags&3!=0&&self.flags&!3==0}
    pub fn supports(self,mode:ControlMode)->bool {self.flags&mode.flags()!=0}
    pub fn manual_value(self,value:i32)->bool {self.valid()&&self.supports(ControlMode::Manual)&&value>=self.minimum&&value<=self.maximum&&(i64::from(value)-i64::from(self.minimum))%i64::from(self.step)==0}
}
#[derive(Clone, Copy, Debug, Serialize)]
pub struct ControlObserved {pub value:i32,pub mode:ControlMode,pub flags:i32}
#[derive(Clone, Debug, Serialize)]
pub struct CameraControlInfo {pub id:CameraControlId,pub range:ControlRange,pub observed:ControlObserved}
#[derive(Clone, Debug, Serialize)]
pub struct ControlDiagnostic {pub property:Option<CameraControlId>,pub operation:&'static str,pub hresult:Option<i32>,pub detail:&'static str,pub native_values:Option<[i32;5]>}
impl ControlDiagnostic {
    pub fn plain(property:Option<CameraControlId>,operation:&'static str,detail:&'static str)->Self{Self{property,operation,hresult:None,detail,native_values:None}}
}
#[derive(Clone, Debug, Default, Serialize)]
pub struct ControlQuery {pub controls:Vec<CameraControlInfo>,pub diagnostics:Vec<ControlDiagnostic>}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControlLease {pub key:CameraRequestKey,pub source:Id,pub binding:VideoCaptureBinding,pub stream_generation:u64}
#[derive(Clone, Debug, Serialize)]
pub struct ControlCatalog {pub lease:ControlLease,pub query:ControlQuery}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag="mode",content="value",rename_all="snake_case")]
pub enum ControlSetting {Auto,Manual(i32),ManualDefault}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControlChange {pub id:CameraControlId,pub setting:ControlSetting}
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all="snake_case")]
pub enum ControlPhase {BeforeWrite,AfterWrite,Readback}
#[derive(Clone, Debug, Serialize)]
pub struct ControlReceipt {pub phase:ControlPhase,pub requested:ControlChange,pub write_started:bool,pub write_succeeded:bool,pub observed:Option<ControlObserved>,pub diagnostic:Option<ControlDiagnostic>}
impl ControlReceipt {
    pub fn before(change:ControlChange,detail:&'static str)->Self{Self{phase:ControlPhase::BeforeWrite,requested:change,write_started:false,write_succeeded:false,observed:None,diagnostic:Some(ControlDiagnostic::plain(Some(change.id),"validation",detail))}}
}
#[derive(Clone, Debug)]
pub struct ControlWriteGate(Arc<AtomicU8>);
impl ControlWriteGate {
    pub(super) fn new()->Self{Self(Arc::new(AtomicU8::new(0)))}
    pub fn begin_write(&self)->bool{self.0.compare_exchange(0,1,Ordering::AcqRel,Ordering::Acquire).is_ok()}
    pub(super) fn finish(&self){self.0.store(2,Ordering::Release);}
    pub(super) fn before_change(&self)->Result<(),CameraError>{match self.0.compare_exchange(0,3,Ordering::AcqRel,Ordering::Acquire){Ok(_)|Err(2|3)=>Ok(()),_=>Err(CameraError::Busy)}}
    pub(super) fn cancel_queued(&self){let _=self.0.compare_exchange(0,3,Ordering::AcqRel,Ordering::Acquire);}
}

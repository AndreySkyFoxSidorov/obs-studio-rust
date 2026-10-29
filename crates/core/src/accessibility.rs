//! Authored interface colors; independent of the GUI, graphics and OS.
use crate::Error;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all="snake_case")]
pub enum ColorPreset {#[default] Default, ColorBlind1, Custom}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
pub enum InterfaceColor {
    Selection, Crop, Hover,
    MeterGreen, MeterYellow, MeterRed,
    MeterGreenActive, MeterYellowActive, MeterRedActive,
}
impl InterfaceColor {
    pub const ALL:[Self;9]=[Self::Selection,Self::Crop,Self::Hover,Self::MeterGreen,Self::MeterYellow,Self::MeterRed,
        Self::MeterGreenActive,Self::MeterYellowActive,Self::MeterRedActive];
    pub fn locale_key(self)->&'static str {match self {
        Self::Selection=>"Basic.Settings.Accessibility.ColorOverrides.SelectRed",
        Self::Crop=>"Basic.Settings.Accessibility.ColorOverrides.SelectGreen",
        Self::Hover=>"Basic.Settings.Accessibility.ColorOverrides.SelectBlue",
        Self::MeterGreen=>"Basic.Settings.Accessibility.ColorOverrides.MixerGreen",
        Self::MeterYellow=>"Basic.Settings.Accessibility.ColorOverrides.MixerYellow",
        Self::MeterRed=>"Basic.Settings.Accessibility.ColorOverrides.MixerRed",
        Self::MeterGreenActive=>"Basic.Settings.Accessibility.ColorOverrides.MixerGreenActive",
        Self::MeterYellowActive=>"Basic.Settings.Accessibility.ColorOverrides.MixerYellowActive",
        Self::MeterRedActive=>"Basic.Settings.Accessibility.ColorOverrides.MixerRedActive",
    }}
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from="Data")]
pub struct Accessibility {
    override_colors:bool,
    preset:ColorPreset,
    colors:[[u8;3];9],
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Data {override_colors:bool,preset:ColorPreset,colors:[[u8;3];9]}
impl TryFrom<Data> for Accessibility {
    type Error=Error;
    fn try_from(data:Data)->Result<Self,Error> {
        let value=Self {override_colors:data.override_colors,preset:data.preset,colors:data.colors};
        value.validate()?;Ok(value)
    }
}
impl Default for Accessibility {
    fn default()->Self {Self {override_colors:false,preset:ColorPreset::Default,colors:Self::default_colors()}}
}
impl Accessibility {
    // Exact byte order from OBS32.2.2 OBSBasicSettings_A11y.cpp. Stored OBS
    // integers are little-endian RGB; no alpha control is exposed there.
    pub const fn default_colors()->[[u8;3];9] {[
        [255,0,0],[0,255,0],[0,127,255],
        [0x26,0x7f,0x26],[0x7f,0x7f,0x26],[0x7f,0x26,0x26],
        [0x4c,0xff,0x4c],[0xff,0xff,0x4c],[0xff,0x4c,0x4c],
    ]}
    pub const fn color_blind_colors()->[[u8;3];9] {[
        [0xf9,0x49,0x33],[0xc9,0x56,0xff],[0x44,0x9b,0xb0],
        [0x60,0x1a,0x4a],[0x97,0x2c,0x1f],[0x28,0x5b,0x67],
        [0x94,0x2e,0x74],[0xf9,0x49,0x33],[0x63,0xac,0xbe],
    ]}
    pub fn override_colors(self)->bool {self.override_colors}
    pub fn preset(self)->ColorPreset {self.preset}
    pub fn color(self,role:InterfaceColor)->[u8;3] {self.colors[role as usize]}
    pub fn effective_color(self,role:InterfaceColor)->[u8;3] {
        if self.override_colors {self.color(role)}else{Self::default_colors()[role as usize]}
    }
    pub fn with_override(mut self,enabled:bool)->Self {self.override_colors=enabled;self}
    pub fn with_preset(mut self,preset:ColorPreset)->Self {
        self.preset=preset;
        match preset {ColorPreset::Default=>self.colors=Self::default_colors(),ColorPreset::ColorBlind1=>self.colors=Self::color_blind_colors(),ColorPreset::Custom=>{}}
        self
    }
    pub fn with_color(mut self,role:InterfaceColor,color:[u8;3])->Self {self.colors[role as usize]=color;self.preset=ColorPreset::Custom;self}
    pub fn is_default(&self)->bool {*self==Self::default()}
    pub fn validate(self)->Result<(),Error> {
        let expected=match self.preset {ColorPreset::Default=>Some(Self::default_colors()),ColorPreset::ColorBlind1=>Some(Self::color_blind_colors()),ColorPreset::Custom=>None};
        if expected.is_some_and(|palette|palette!=self.colors) {return Err(Error::Invalid("Accessibility preset colors do not match the selected preset."));}
        Ok(())
    }
}

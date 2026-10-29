//! Source-owned channel controls and signed presentation-time audio alignment.
use crate::Error;
use serde::{Deserialize, Serialize};
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "PropertiesData")]
pub struct SourceAudioProperties {
    #[serde(default,skip_serializing_if="PushAudioSettings::is_default")]
    push: PushAudioSettings,
    #[serde(skip_serializing_if="crate::SourceAudioTrackMask::is_all")]
    audio_tracks:crate::SourceAudioTrackMask,
    force_mono: bool,
    balance: f32,
    #[serde(skip_serializing_if = "zero_offset")]
    sync_offset_ms: i32,
}
pub const MIN_AUDIO_SYNC_OFFSET_MS: i32 = -950;
pub const MAX_AUDIO_SYNC_OFFSET_MS: i32 = 20_000;
fn zero_offset(value: &i32) -> bool { *value == 0 }
fn center() -> f32 {
    0.5
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PropertiesData {
    #[serde(default)]
    push: PushAudioSettings,
    #[serde(default)]
    audio_tracks:crate::SourceAudioTrackMask,
    #[serde(default)]
    force_mono: bool,
    #[serde(default = "center")]
    balance: f32,
    #[serde(default)]
    sync_offset_ms: i32,
}
impl TryFrom<PropertiesData> for SourceAudioProperties {
    type Error = Error;
    fn try_from(value: PropertiesData) -> Result<Self, Error> {
        Self::new(value.force_mono, value.balance)?.with_sync_offset_ms(value.sync_offset_ms).map(|properties|properties.with_audio_tracks(value.audio_tracks).with_push(value.push))
    }
}
impl Default for SourceAudioProperties {
    fn default() -> Self {
        Self {
            push:PushAudioSettings::default(),
            audio_tracks:crate::SourceAudioTrackMask::ALL,
            force_mono: false,
            balance: 0.5,
            sync_offset_ms: 0,
        }
    }
}
impl SourceAudioProperties {
    pub fn new(force_mono: bool, balance: f32) -> Result<Self, Error> {
        let result = Self {
            push:PushAudioSettings::default(),
            audio_tracks:crate::SourceAudioTrackMask::ALL,
            force_mono,
            balance,
            sync_offset_ms: 0,
        };
        result.validate()?;
        Ok(result)
    }
    /// The capture timestamp is unchanged. This shifts source presentation on
    /// the project audio sample grid; negative values require real lookahead.
    pub fn with_sync_offset_ms(mut self, value: i32) -> Result<Self, Error> {
        self.sync_offset_ms = value;
        self.validate()?;
        Ok(self)
    }
    pub fn audio_tracks(self)->crate::SourceAudioTrackMask{self.audio_tracks}
    pub fn push(self)->PushAudioSettings{self.push}
    pub fn with_push(mut self,push:PushAudioSettings)->Self{self.push=push;self}
    /// Routing metadata only; no source-clock processing or native format change.
    pub fn with_audio_tracks(mut self,tracks:crate::SourceAudioTrackMask)->Self{self.audio_tracks=tracks;self}
    pub fn sync_offset_ms(self) -> i32 { self.sync_offset_ms }
    pub fn sync_offset_ns(self) -> i64 { i64::from(self.sync_offset_ms) * 1_000_000 }
    pub fn force_mono(self) -> bool {
        self.force_mono
    }
    pub fn balance(self) -> f32 {
        self.balance
    }
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
    pub(crate) fn validate(self) -> Result<(), Error> {
        if !self.balance.is_finite() || !(0.0..=1.0).contains(&self.balance) {
            return Err(Error::Invalid("source balance must be finite in0..1"));
        }
        if !(MIN_AUDIO_SYNC_OFFSET_MS..=MAX_AUDIO_SYNC_OFFSET_MS).contains(&self.sync_offset_ms) {
            return Err(Error::Invalid("audio sync offset must be -950..20000 milliseconds"));
        }
        Ok(())
    }
}

/// Source-local authoring only. Pressed state and clocks belong to the host.
#[derive(Clone,Copy,Debug,Default,PartialEq,Eq,Serialize,Deserialize)]
#[serde(deny_unknown_fields,try_from="PushAudioData")]
pub struct PushAudioSettings { mute_enabled:bool, mute_delay_ms:u32, talk_enabled:bool, talk_delay_ms:u32 }
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PushAudioData {
    #[serde(default)] mute_enabled:bool,
    #[serde(default)] mute_delay_ms:u32,
    #[serde(default)] talk_enabled:bool,
    #[serde(default)] talk_delay_ms:u32,
}
impl TryFrom<PushAudioData> for PushAudioSettings {
    type Error=Error;
    fn try_from(v:PushAudioData)->Result<Self,Error>{Self::new(v.mute_enabled,v.mute_delay_ms,v.talk_enabled,v.talk_delay_ms)}
}
impl PushAudioSettings {
    /// OBS Settings uses 0..=INT_MAX milliseconds for each release delay.
    pub fn new(mute_enabled:bool,mute_delay_ms:u32,talk_enabled:bool,talk_delay_ms:u32)->Result<Self,Error>{
        if mute_delay_ms>i32::MAX as u32||talk_delay_ms>i32::MAX as u32{return Err(Error::Invalid("Push-to-mute/talk delay must be 0..2147483647 milliseconds."));}
        Ok(Self{mute_enabled,mute_delay_ms,talk_enabled,talk_delay_ms})
    }
    pub fn is_default(&self)->bool{*self==Self::default()}
    pub fn mute_enabled(self)->bool{self.mute_enabled}
    pub fn mute_delay_ms(self)->u32{self.mute_delay_ms}
    pub fn talk_enabled(self)->bool{self.talk_enabled}
    pub fn talk_delay_ms(self)->u32{self.talk_delay_ms}
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_literal_endpoints_defaults_and_nonfinite() {
        assert!(SourceAudioProperties::default().is_default());
        for balance in [0.0, 0.49, 0.5, 0.51, 1.0] {
            assert_eq!(
                SourceAudioProperties::new(true, balance).unwrap().balance(),
                balance
            );
        }
        for balance in [-0.01, 1.01, f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert!(SourceAudioProperties::new(false, balance).is_err());
        }
    }
}

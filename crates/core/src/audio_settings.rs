//! Validated authored project audio format. Device formats and conversion state
//! remain runtime concerns. See doc/migration/audio-settings.md.
use crate::Error;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpeakerLayout {
    Mono,
    Stereo,
    TwoPointOne,
    FourPointZero,
    FourPointOne,
    FivePointOne,
    SevenPointOne,
}
impl SpeakerLayout {
    pub const ALL: [Self; 7] = [
        Self::Mono,
        Self::Stereo,
        Self::TwoPointOne,
        Self::FourPointZero,
        Self::FourPointOne,
        Self::FivePointOne,
        Self::SevenPointOne,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::Mono => "Mono",
            Self::Stereo => "Stereo",
            Self::TwoPointOne => "2.1",
            Self::FourPointZero => "4.0",
            Self::FourPointOne => "4.1",
            Self::FivePointOne => "5.1",
            Self::SevenPointOne => "7.1",
        }
    }
    pub fn channels(self) -> u16 {
        match self {
            Self::Mono => 1,
            Self::Stereo => 2,
            Self::TwoPointOne => 3,
            Self::FourPointZero => 4,
            Self::FourPointOne => 5,
            Self::FivePointOne => 6,
            Self::SevenPointOne => 8,
        }
    }
    /// Modern OBS + supplied FFmpeg standard native order (ascending mask bits).
    pub fn channel_mask(self) -> u32 {
        match self {
            Self::Mono => 0x4,
            Self::Stereo => 0x3,
            Self::TwoPointOne => 0xb,
            Self::FourPointZero => 0x107,
            Self::FourPointOne => 0x10f,
            Self::FivePointOne => 0x3f,
            Self::SevenPointOne => 0x63f,
        }
    }
}

/// Authored OBS buffering preference. Runtime growth and readiness belong to
/// the processing owner; zero milliseconds selects the default 45-tick ceiling.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "BufferingData")]
pub struct AudioBuffering {
    max_ms: u32,
    fixed: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BufferingData {
    max_ms: u32,
    fixed: bool,
}
impl TryFrom<BufferingData> for AudioBuffering {
    type Error = Error;
    fn try_from(value: BufferingData) -> Result<Self, Error> {
        Self::new(value.max_ms, value.fixed)
    }
}
impl AudioBuffering {
    pub fn new(max_ms: u32, fixed: bool) -> Result<Self, Error> {
        let value = Self { max_ms, fixed };
        value.validate()?;
        Ok(value)
    }
    pub fn low_latency() -> Self {
        Self {
            max_ms: 20,
            fixed: true,
        }
    }
    pub fn max_ms(self) -> u32 {
        self.max_ms
    }
    pub fn fixed(self) -> bool {
        self.fixed
    }
    pub fn is_low_latency(self) -> bool {
        self.max_ms == 20 && self.fixed
    }
    pub fn max_ticks(self, sample_rate: u32) -> Result<u32, Error> {
        self.validate()?;
        if !matches!(sample_rate, 44100 | 48000) {
            return Err(Error::Invalid(
                "project audio sample rate must be 44100 or 48000 Hz",
            ));
        }
        if self.max_ms == 0 {
            return Ok(45);
        }
        let frames = u64::from(self.max_ms) * u64::from(sample_rate) / 1000;
        Ok(frames.div_ceil(1024) as u32)
    }
    pub(crate) fn validate(self) -> Result<(), Error> {
        if self.max_ms > 1000 {
            return Err(Error::Invalid(
                "audio buffering maximum must be in 0..=1000 milliseconds",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "SettingsData")]
pub struct AudioSettings {
    sample_rate: u32,
    speakers: SpeakerLayout,
    #[serde(default)]
    buffering: AudioBuffering,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SettingsData {
    sample_rate: u32,
    speakers: SpeakerLayout,
    #[serde(default)]
    buffering: AudioBuffering,
}
impl TryFrom<SettingsData> for AudioSettings {
    type Error = Error;
    fn try_from(value: SettingsData) -> Result<Self, Error> {
        Self::new(value.sample_rate, value.speakers)?.with_buffering(value.buffering)
    }
}
impl Default for AudioSettings {
    fn default() -> Self {
        Self {
            sample_rate: 48000,
            speakers: SpeakerLayout::Stereo,
            buffering: AudioBuffering::default(),
        }
    }
}
impl AudioSettings {
    pub(crate) fn effective(
        sample_rate: u32,
        speakers: SpeakerLayout,
        buffering: AudioBuffering,
    ) -> Self {
        Self {
            sample_rate,
            speakers,
            buffering,
        }
    }
    pub fn new(sample_rate: u32, speakers: SpeakerLayout) -> Result<Self, Error> {
        let settings = Self {
            sample_rate,
            speakers,
            buffering: AudioBuffering::default(),
        };
        settings.validate()?;
        Ok(settings)
    }
    pub fn sample_rate(self) -> u32 {
        self.sample_rate
    }
    pub fn speakers(self) -> SpeakerLayout {
        self.speakers
    }
    pub fn channels(self) -> u16 {
        self.speakers.channels()
    }
    pub fn channel_mask(self) -> u32 {
        self.speakers.channel_mask()
    }
    pub fn buffering(self) -> AudioBuffering {
        self.buffering
    }
    pub fn with_buffering(self, buffering: AudioBuffering) -> Result<Self, Error> {
        let settings = Self { buffering, ..self };
        settings.validate()?;
        Ok(settings)
    }
    pub(crate) fn validate(self) -> Result<(), Error> {
        if !matches!(self.sample_rate, 44100 | 48000) {
            return Err(Error::Invalid(
                "project audio sample rate must be 44100 or 48000 Hz",
            ));
        }
        self.buffering.validate()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Command, Session};
    use serde::de::{
        IntoDeserializer, Visitor,
        value::{Error as DeError, MapDeserializer},
    };
    use std::sync::Arc;
    // Serde-only fixtures keep JSON/file dependencies out of the core layer.
    // The host separately verifies the native JSON representation.
    enum Value {
        Number(u64),
        Bool(bool),
        Text(&'static str),
        Map(Vec<(&'static str, Value)>),
    }
    impl<'de> IntoDeserializer<'de, DeError> for Value {
        type Deserializer = Self;
        fn into_deserializer(self) -> Self {
            self
        }
    }
    impl<'de> serde::Deserializer<'de> for Value {
        type Error = DeError;
        fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, DeError> {
            match self {
                Value::Number(n) => visitor.visit_u64(n),
                Value::Bool(value) => visitor.visit_bool(value),
                Value::Text(text) => visitor.visit_str(text),
                Value::Map(fields) => visitor.visit_map(MapDeserializer::new(fields.into_iter())),
            }
        }
        fn deserialize_enum<V: Visitor<'de>>(
            self,
            name: &'static str,
            variants: &'static [&'static str],
            visitor: V,
        ) -> Result<V::Value, DeError> {
            match self {
                Value::Text(text) => serde::Deserializer::deserialize_enum(
                    text.into_deserializer(),
                    name,
                    variants,
                    visitor,
                ),
                other => other.deserialize_any(visitor),
            }
        }
        serde::forward_to_deserialize_any! { bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string bytes byte_buf option unit unit_struct newtype_struct seq tuple tuple_struct map struct identifier ignored_any }
    }
    fn settings_value(rate: u64, speakers: &'static str) -> Value {
        Value::Map(vec![
            ("sample_rate", Value::Number(rate)),
            ("speakers", Value::Text(speakers)),
        ])
    }
    fn buffering_value(ms: u64, fixed: bool) -> Value {
        Value::Map(vec![
            ("max_ms", Value::Number(ms)),
            ("fixed", Value::Bool(fixed)),
        ])
    }
    #[test]
    fn buffering_limits_defaults_and_obs_integer_rounding_have_literal_oracles() {
        let default = AudioBuffering::default();
        assert_eq!((default.max_ms(), default.fixed()), (0, false));
        assert!(!default.is_low_latency());
        for rate in [44100, 48000] {
            assert_eq!(default.max_ticks(rate).unwrap(), 45);
            assert_eq!(
                AudioBuffering::new(0, true)
                    .unwrap()
                    .max_ticks(rate)
                    .unwrap(),
                45
            );
            assert_eq!(AudioBuffering::low_latency().max_ticks(rate).unwrap(), 1);
        }
        assert!(AudioBuffering::low_latency().is_low_latency());
        assert!(!AudioBuffering::new(20, false).unwrap().is_low_latency());
        for (rate, ms, ticks) in [
            (44100, 1, 1),
            (44100, 23, 1),
            (44100, 24, 2),
            (44100, 209, 9),
            (44100, 210, 10),
            (44100, 232, 10),
            (44100, 233, 11),
            (44100, 1000, 44),
            (48000, 20, 1),
            (48000, 21, 1),
            (48000, 22, 2),
            (48000, 640, 30),
            (48000, 641, 31),
            (48000, 1000, 47),
        ] {
            assert_eq!(
                AudioBuffering::new(ms, false)
                    .unwrap()
                    .max_ticks(rate)
                    .unwrap(),
                ticks,
                "rate={rate}, ms={ms}"
            );
        }
        // 209ms at44.1kHz is9216.9 frames: OBS floors to9216 before
        // ceiling to9 blocks. Ceiling milliseconds directly would incorrectly give10.
        for ms in [1001, u32::MAX] {
            assert!(AudioBuffering::new(ms, false).is_err());
        }
        for rate in [0, 8000, 44099, 48001, 96000, u32::MAX] {
            assert!(default.max_ticks(rate).is_err());
        }
    }
    #[test]
    fn buffering_serde_migrates_missing_policy_and_rejects_explicit_invalid_values() {
        let legacy = AudioSettings::deserialize(settings_value(48000, "stereo")).unwrap();
        assert_eq!(legacy.buffering(), AudioBuffering::default());
        let configured = AudioSettings::deserialize(Value::Map(vec![
            ("sample_rate", Value::Number(44100)),
            ("speakers", Value::Text("five_point_one")),
            ("buffering", buffering_value(209, true)),
        ]))
        .unwrap();
        assert_eq!(
            configured.buffering(),
            AudioBuffering::new(209, true).unwrap()
        );
        assert_eq!(
            configured
                .buffering()
                .max_ticks(configured.sample_rate())
                .unwrap(),
            9
        );
        for value in [
            buffering_value(1001, false),
            Value::Map(vec![("max_ms", Value::Number(20))]),
            Value::Map(vec![("fixed", Value::Bool(true))]),
            Value::Map(vec![
                ("max_ms", Value::Number(20)),
                ("fixed", Value::Number(1)),
            ]),
            Value::Map(vec![
                ("max_ms", Value::Number(20)),
                ("fixed", Value::Bool(true)),
                ("extra", Value::Bool(false)),
            ]),
        ] {
            assert!(AudioBuffering::deserialize(value).is_err());
        }
        assert_eq!(
            AudioSettings::new(48000, SpeakerLayout::Stereo)
                .unwrap()
                .buffering(),
            AudioBuffering::default()
        );
    }
    #[test]
    fn buffering_settings_publish_once_and_do_not_enter_scene_history() {
        let mut session = Session::new();
        let before = session.snapshot();
        let desired = before
            .audio()
            .with_buffering(AudioBuffering::low_latency())
            .unwrap();
        assert!(
            session
                .apply(Command::SetAudioSettings { settings: desired })
                .unwrap()
                .changed
        );
        let after = session.snapshot();
        assert_eq!(after.revision(), before.revision() + 1);
        assert_eq!(after.audio().sample_rate(), before.audio().sample_rate());
        assert_eq!(after.audio().speakers(), before.audio().speakers());
        assert_eq!(after.video(), before.video());
        assert_eq!(after.sources(), before.sources());
        assert_eq!(after.scenes(), before.scenes());
        assert!(
            !session
                .apply(Command::SetAudioSettings { settings: desired })
                .unwrap()
                .changed
        );
        assert!(Arc::ptr_eq(&after, &session.snapshot()));
        assert!(!session.can_undo());
        assert!(session.apply(Command::Undo).is_err());
        assert_eq!(session.snapshot().audio(), desired);
        let invalid = AudioSettings {
            sample_rate: 1,
            speakers: SpeakerLayout::Stereo,
            buffering: AudioBuffering::default(),
        };
        assert!(
            invalid
                .with_buffering(AudioBuffering::low_latency())
                .is_err()
        );
    }

    #[test]
    fn seven_obs_layouts_have_literal_native_mask_count_and_label_oracles() {
        for (layout, label, count, mask) in [
            (SpeakerLayout::Mono, "Mono", 1, 0x4u32),
            (SpeakerLayout::Stereo, "Stereo", 2, 0x3),
            (SpeakerLayout::TwoPointOne, "2.1", 3, 0xb),
            (SpeakerLayout::FourPointZero, "4.0", 4, 0x107),
            (SpeakerLayout::FourPointOne, "4.1", 5, 0x10f),
            (SpeakerLayout::FivePointOne, "5.1", 6, 0x3f),
            (SpeakerLayout::SevenPointOne, "7.1", 8, 0x63f),
        ] {
            assert_eq!(layout.label(), label);
            assert_eq!(layout.channels(), count);
            assert_eq!(layout.channel_mask(), mask);
            assert_eq!(mask.count_ones(), u32::from(count));
            for rate in [44100, 48000] {
                let settings = AudioSettings::new(rate, layout).unwrap();
                assert_eq!(settings.sample_rate(), rate);
                assert_eq!(settings.speakers(), layout);
                assert_eq!(settings.channels(), count);
                assert_eq!(settings.channel_mask(), mask);
            }
        }
        assert_eq!(SpeakerLayout::ALL.len(), 7);
    }
    #[test]
    fn invalid_rates_and_explicit_malformed_settings_are_rejected_by_the_same_boundary() {
        for rate in [0, 8000, 44099, 44101, 47999, 48001, 96000, u32::MAX] {
            assert!(AudioSettings::new(rate, SpeakerLayout::Stereo).is_err());
        }
        for value in [
            settings_value(96000, "stereo"),
            settings_value(48000, "quad"),
            Value::Map(vec![("sample_rate", Value::Number(48000))]),
            Value::Map(vec![
                ("sample_rate", Value::Number(48000)),
                ("speakers", Value::Text("stereo")),
                ("device_id", Value::Text("default")),
            ]),
        ] {
            assert!(AudioSettings::deserialize(value).is_err());
        }
    }

    #[test]
    fn settings_noop_preserves_unrelated_authored_state_and_scene_history() {
        let mut session = Session::new();
        let before = session.snapshot();
        assert!(
            !session
                .apply(Command::SetAudioSettings {
                    settings: AudioSettings::default()
                })
                .unwrap()
                .changed
        );
        assert!(Arc::ptr_eq(&before, &session.snapshot()));
        let desired = AudioSettings::new(44100, SpeakerLayout::FivePointOne).unwrap();
        assert!(
            session
                .apply(Command::SetAudioSettings { settings: desired })
                .unwrap()
                .changed
        );
        let after = session.snapshot();
        assert_eq!(after.audio(), desired);
        assert_eq!(after.revision(), before.revision() + 1);
        assert_eq!(after.video(), before.video());
        assert_eq!(after.scenes(), before.scenes());
        assert_eq!(after.sources(), before.sources());
        assert!(
            !session
                .apply(Command::SetAudioSettings { settings: desired })
                .unwrap()
                .changed
        );
        assert!(Arc::ptr_eq(&after, &session.snapshot()));
        assert!(!session.can_undo());
        assert!(session.apply(Command::Undo).is_err());
        assert_eq!(session.snapshot().audio(), desired);
    }
    #[test]
    fn collection_candidate_validation_rejects_invalid_internal_rate_and_preserves_session() {
        let mut session = Session::new();
        let before = session.snapshot();
        let invalid = AudioSettings {
            sample_rate: 1,
            speakers: SpeakerLayout::Mono,
            buffering: AudioBuffering::default(),
        };
        assert!(
            session
                .apply(Command::SetAudioSettings { settings: invalid })
                .is_err()
        );
        assert!(invalid.validate().is_err());
        assert!(Arc::ptr_eq(&before, &session.snapshot()));
    }
}

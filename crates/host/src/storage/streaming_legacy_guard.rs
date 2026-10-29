//! Reject schema5-only profile data in legacy2..4 without a Value clone or retained rows.
use serde::{
    Deserialize, Deserializer,
    de::{self, IgnoredAny, MapAccess, SeqAccess, Visitor},
};
use std::fmt;
pub(super) fn check(bytes: &[u8]) -> Result<(), serde_json::Error> {
    serde_json::from_slice::<EnvelopeGuard>(bytes).map(|_| ())
}
struct EnvelopeGuard;
struct CatalogGuard;
struct RowsGuard;
struct RowGuard;
impl<'de> Deserialize<'de> for EnvelopeGuard {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = EnvelopeGuard;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("native envelope")
            }
            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
                while let Some(key) = map.next_key::<String>()? {
                    if key == "profiles" {
                        map.next_value::<CatalogGuard>()?;
                    } else {
                        map.next_value::<IgnoredAny>()?;
                    }
                }
                Ok(EnvelopeGuard)
            }
        }
        d.deserialize_map(V)
    }
}
impl<'de> Deserialize<'de> for CatalogGuard {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = CatalogGuard;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("profile catalog")
            }
            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
                while let Some(key) = map.next_key::<String>()? {
                    if key == "profiles" {
                        map.next_value::<RowsGuard>()?;
                    } else {
                        map.next_value::<IgnoredAny>()?;
                    }
                }
                Ok(CatalogGuard)
            }
        }
        d.deserialize_map(V)
    }
}
impl<'de> Deserialize<'de> for RowsGuard {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = RowsGuard;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("bounded profile rows")
            }
            fn visit_seq<S: SeqAccess<'de>>(self, mut seq: S) -> Result<Self::Value, S::Error> {
                let mut count = 0;
                while seq.next_element::<RowGuard>()?.is_some() {
                    count += 1;
                    if count > skylive_core::MAX_PROFILES {
                        return Err(de::Error::custom("legacy profile count exceeds bound"));
                    }
                }
                Ok(RowsGuard)
            }
        }
        d.deserialize_seq(V)
    }
}
impl<'de> Deserialize<'de> for RowGuard {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = RowGuard;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("legacy profile without streaming field")
            }
            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
                while let Some(key) = map.next_key::<String>()? {
                    if key == "streaming" {
                        return Err(de::Error::custom(
                            "streaming settings require native schema5",
                        ));
                    }
                    map.next_value::<IgnoredAny>()?;
                }
                Ok(RowGuard)
            }
        }
        d.deserialize_map(V)
    }
}

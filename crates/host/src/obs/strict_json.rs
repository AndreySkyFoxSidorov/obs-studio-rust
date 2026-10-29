//! Decode JSON with unique object keys at every nesting level. Paths use
//! decoded keys, so escape-equivalent spellings are the same identity. The
//! enclosing OBS codec checks byte/depth/structure budgets before this parser.
use super::{ObsError, issue};
use serde::de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Number, Value};
use std::fmt;

enum PathSegment {
    Key(String),
    Index(usize),
}

#[derive(Default)]
struct ParseContext {
    duplicate: Option<String>,
    // Retain each ancestor key once; never retain full ancestor path strings.
    path: Vec<PathSegment>,
}

struct ValueSeed<'a> {
    context: &'a mut ParseContext,
}

impl<'de> DeserializeSeed<'de> for ValueSeed<'_> {
    type Value = Value;

    fn deserialize<D>(self, deserializer: D) -> Result<Value, D::Error>
    where
        D: de::Deserializer<'de>,
    {
        deserializer.deserialize_any(ValueVisitor {
            context: self.context,
        })
    }
}

struct ValueVisitor<'a> {
    context: &'a mut ParseContext,
}

fn diagnostic_path(segments: &[PathSegment]) -> String {
    use fmt::Write;
    let mut path = String::from("$");
    for segment in segments {
        match segment {
            PathSegment::Key(key)
                if !key.is_empty()
                    && key
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_') =>
            {
                path.push('.');
                path.push_str(key);
            }
            PathSegment::Key(key) => {
                // Writing to a String is infallible. Build the complete escaped
                // diagnostic only at the terminal duplicate-key error.
                let _ = write!(&mut path, "[{key:?}]");
            }
            PathSegment::Index(index) => {
                let _ = write!(&mut path, "[{index}]");
            }
        }
    }
    path
}

impl<'de> Visitor<'de> for ValueVisitor<'_> {
    type Value = Value;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON value")
    }

    fn visit_bool<E: de::Error>(self, value: bool) -> Result<Value, E> {
        Ok(Value::Bool(value))
    }

    fn visit_i64<E: de::Error>(self, value: i64) -> Result<Value, E> {
        Ok(Value::Number(value.into()))
    }

    fn visit_u64<E: de::Error>(self, value: u64) -> Result<Value, E> {
        Ok(Value::Number(value.into()))
    }

    fn visit_f64<E: de::Error>(self, value: f64) -> Result<Value, E> {
        Number::from_f64(value)
            .map(Value::Number)
            .ok_or_else(|| E::custom("JSON number must be finite"))
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<Value, E> {
        Ok(Value::String(value.to_owned()))
    }

    fn visit_string<E: de::Error>(self, value: String) -> Result<Value, E> {
        Ok(Value::String(value))
    }

    fn visit_unit<E: de::Error>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_none<E: de::Error>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut access: A) -> Result<Value, A::Error> {
        let mut values = Vec::new();
        loop {
            self.context.path.push(PathSegment::Index(values.len()));
            let result = access.next_element_seed(ValueSeed {
                context: &mut *self.context,
            });
            self.context.path.pop();
            match result? {
                Some(value) => values.push(value),
                None => return Ok(Value::Array(values)),
            }
        }
    }

    fn visit_map<A: MapAccess<'de>>(self, mut access: A) -> Result<Value, A::Error> {
        let mut values = Map::new();
        while let Some(key) = access.next_key::<String>()? {
            if values.contains_key(&key) {
                self.context.path.push(PathSegment::Key(key));
                self.context.duplicate = Some(diagnostic_path(&self.context.path));
                self.context.path.pop();
                return Err(de::Error::custom("duplicate object key"));
            }
            self.context.path.push(PathSegment::Key(key.clone()));
            let result = access.next_value_seed(ValueSeed {
                context: &mut *self.context,
            });
            self.context.path.pop();
            values.insert(key, result?);
        }
        Ok(Value::Object(values))
    }
}

pub(super) fn parse(bytes: &[u8]) -> Result<Value, ObsError> {
    let mut context = ParseContext::default();
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let parsed = ValueSeed {
        context: &mut context,
    }
    .deserialize(&mut deserializer);
    let value = parsed.map_err(|error| match context.duplicate {
        Some(path) => issue(&path, "duplicate_key", error.to_string()),
        None => issue("$", "invalid_json", error.to_string()),
    })?;
    deserializer
        .end()
        .map_err(|error| issue("$", "invalid_json", error.to_string()))?;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_filters_cannot_erase_unsupported_content() {
        let bytes = br#"{"sources":[{"filters":[{"id":"color_filter"}],"filters":[]}]}"#;
        let error = parse(bytes).unwrap_err();
        assert_eq!(error.issues[0].code, "duplicate_key");
        assert_eq!(error.issues[0].path, "$.sources[0].filters");
    }

    #[test]
    fn decoded_keys_reject_escape_equivalent_duplicates() {
        let error = parse(br#"{"settings":{"color":1,"\u0063olor":2}}"#).unwrap_err();
        assert_eq!(error.issues[0].code, "duplicate_key");
        assert_eq!(error.issues[0].path, "$.settings.color");
    }

    #[test]
    fn duplicate_harmless_values_are_also_rejected() {
        let error = parse(br#"{"name":"same","name":"same"}"#).unwrap_err();
        assert_eq!(error.issues[0].code, "duplicate_key");
        assert_eq!(error.issues[0].path, "$.name");
    }

    #[test]
    fn nonduplicate_strings_keep_valid_json_escape_semantics() {
        let parsed = parse(br#"{"\u006eame":"line\n\u041f\u0440\u0438\u0432\u0456\u0442","quoted":"\"","slash":"\\","nested":[true,false,null,-2,3,1.25]}"#).unwrap();
        assert_eq!(parsed["name"], "line\nПривіт");
        assert_eq!(parsed["quoted"], "\"");
        assert_eq!(parsed["slash"], "\\");
        assert_eq!(parsed["nested"][5], 1.25);
    }

    #[test]
    fn path_stack_retains_only_the_sum_of_key_segment_bytes() {
        let mut context = ParseContext::default();
        context.path.push(PathSegment::Key("a".repeat(4096)));
        for index in 0..16 {
            context.path.push(PathSegment::Index(index));
            context.path.push(PathSegment::Key("node".to_owned()));
        }
        let retained_key_bytes: usize = context
            .path
            .iter()
            .map(|segment| match segment {
                PathSegment::Key(key) => key.len(),
                PathSegment::Index(_) => 0,
            })
            .sum();
        assert_eq!(retained_key_bytes, 4096 + 16 * 4);
        assert_eq!(context.path.len(), 33);
        // In particular, the 4-KiB ancestor is not retained once per depth.
        assert!(retained_key_bytes < 4096 * 2);
    }

    #[test]
    fn long_ancestor_key_nested_duplicate_has_an_exact_path_and_unwinds_stack() {
        let ancestor = "a".repeat(4096);
        let mut bytes = format!("{{\"{ancestor}\":");
        for _ in 0..16 {
            bytes.push_str("{\"node\":");
        }
        bytes.push_str("{\"same\":1,\"same\":2}");
        bytes.push_str(&"}".repeat(17));
        let mut context = ParseContext::default();
        let mut deserializer = serde_json::Deserializer::from_slice(bytes.as_bytes());
        assert!(
            ValueSeed {
                context: &mut context
            }
            .deserialize(&mut deserializer)
            .is_err()
        );
        let expected = format!("$.{ancestor}{}.same", ".node".repeat(16));
        assert_eq!(context.duplicate.as_deref(), Some(expected.as_str()));
        assert!(context.path.is_empty());
    }

    #[test]
    fn malformed_json_retains_parser_errors_including_trailing_content() {
        for bytes in [b"{\"name\":}".as_slice(), b"{} false", br#"{"name":"\q"}"#] {
            let error = parse(bytes).unwrap_err();
            assert_eq!(error.issues[0].code, "invalid_json");
            assert!(error.issues[0].message.contains("line 1 column"));
        }
    }
}

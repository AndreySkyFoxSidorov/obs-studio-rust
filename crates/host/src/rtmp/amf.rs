//! Bounded AMF0 subset used by RTMP commands, independently implemented.
use super::Error;
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Value {
    Number(f64),
    Boolean(bool),
    String(String),
    Object(Vec<(String, Value)>),
    Null,
    Array(Vec<Value>),
}
pub(crate) fn encode(values: &[Value]) -> Result<Vec<u8>, Error> {
    let mut out = Vec::new();
    let mut count = 0;
    for value in values {
        write(value, &mut out, 0, &mut count)?;
    }
    Ok(out)
}
fn text(value: &str, out: &mut Vec<u8>) -> Result<(), Error> {
    if value.len() > 16384 {
        return Err(Error::Limit("AMF string"));
    }
    out.extend_from_slice(&(value.len() as u16).to_be_bytes());
    out.extend_from_slice(value.as_bytes());
    Ok(())
}
fn write(value: &Value, out: &mut Vec<u8>, depth: usize, count: &mut usize) -> Result<(), Error> {
    *count += 1;
    if depth > 8 || *count > 64 {
        return Err(Error::Limit("AMF depth/value count"));
    }
    match value {
        Value::Number(n) => {
            if !n.is_finite() {
                return Err(Error::Invalid("AMF nonfinite number"));
            }
            out.push(0);
            out.extend_from_slice(&n.to_be_bytes());
        }
        Value::Boolean(v) => {
            out.extend_from_slice(&[1, u8::from(*v)]);
        }
        Value::String(v) => {
            out.push(2);
            text(v, out)?;
        }
        Value::Object(fields) => {
            out.push(3);
            if fields.len() > 64 {
                return Err(Error::Limit("AMF object fields"));
            }
            for (k, v) in fields {
                text(k, out)?;
                write(v, out, depth + 1, count)?;
            }
            out.extend_from_slice(&[0, 0, 9]);
        }
        Value::Null => out.push(5),
        Value::Array(values) => {
            out.push(10);
            out.extend_from_slice(&(values.len() as u32).to_be_bytes());
            for v in values {
                write(v, out, depth + 1, count)?;
            }
        }
    }
    if out.len() > 1024 * 1024 {
        return Err(Error::Limit("AMF encoded bytes"));
    }
    Ok(())
}
pub(crate) fn decode(bytes: &[u8]) -> Result<Vec<Value>, Error> {
    let mut reader = Reader {
        bytes,
        at: 0,
        count: 0,
    };
    let mut values = Vec::new();
    while reader.at < bytes.len() {
        values.push(reader.value(0)?);
    }
    Ok(values)
}
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
    count: usize,
}
impl Reader<'_> {
    fn take(&mut self, count: usize) -> Result<&[u8], Error> {
        let end = self
            .at
            .checked_add(count)
            .ok_or(Error::Limit("AMF size overflow"))?;
        let value = self
            .bytes
            .get(self.at..end)
            .ok_or(Error::Protocol("truncated AMF"))?;
        self.at = end;
        Ok(value)
    }
    fn string(&mut self, long: bool) -> Result<String, Error> {
        let len = if long {
            u32::from_be_bytes(self.take(4)?.try_into().unwrap()) as usize
        } else {
            u16::from_be_bytes(self.take(2)?.try_into().unwrap()) as usize
        };
        if len > 16384 {
            return Err(Error::Limit("AMF string"));
        }
        String::from_utf8(self.take(len)?.to_vec())
            .map_err(|_| Error::Protocol("AMF string is not UTF8"))
    }
    fn value(&mut self, depth: usize) -> Result<Value, Error> {
        self.count += 1;
        if depth > 8 || self.count > 64 {
            return Err(Error::Limit("AMF depth/value count"));
        }
        Ok(match self.take(1)?[0] {
            0 => {
                let n = f64::from_be_bytes(self.take(8)?.try_into().unwrap());
                if !n.is_finite() {
                    return Err(Error::Protocol("AMF nonfinite number"));
                }
                Value::Number(n)
            }
            1 => match self.take(1)?[0] {
                0 => Value::Boolean(false),
                1 => Value::Boolean(true),
                _ => return Err(Error::Protocol("AMF boolean")),
            },
            2 => Value::String(self.string(false)?),
            12 => Value::String(self.string(true)?),
            5 | 6 => Value::Null,
            kind @ (3 | 8) => {
                if kind == 8 {
                    self.take(4)?;
                }
                let mut fields = Vec::new();
                loop {
                    if self.bytes.get(self.at..self.at + 3) == Some(&[0, 0, 9]) {
                        self.at += 3;
                        break;
                    }
                    if fields.len() >= 64 {
                        return Err(Error::Limit("AMF object fields"));
                    }
                    let key = self.string(false)?;
                    if fields.iter().any(|(existing, _)| existing == &key) {
                        return Err(Error::Protocol("duplicate AMF object key"));
                    }
                    let value = self.value(depth + 1)?;
                    fields.push((key, value));
                }
                Value::Object(fields)
            }
            10 => {
                let count = u32::from_be_bytes(self.take(4)?.try_into().unwrap()) as usize;
                if count > 64 {
                    return Err(Error::Limit("AMF array"));
                }
                let mut values = Vec::with_capacity(count);
                for _ in 0..count {
                    values.push(self.value(depth + 1)?);
                }
                Value::Array(values)
            }
            _ => return Err(Error::Unsupported("AMF marker outside command subset")),
        })
    }
}
pub(crate) fn property<'a>(values: &'a [Value], key: &str) -> Option<&'a Value> {
    values.iter().find_map(|v| {
        if let Value::Object(fields) = v {
            fields.iter().find(|(k, _)| k == key).map(|(_, v)| v)
        } else {
            None
        }
    })
}

//! RTMP chunk wire state; fixed count/bytes, no implicit allocation from peer sizes.
use super::Error;
use std::collections::BTreeMap;
#[derive(Clone, Debug)]
pub(crate) struct Message {
    pub timestamp: u32,
    pub type_id: u8,
    pub stream: u32,
    pub data: Vec<u8>,
}
#[derive(Clone, Default)]
struct State {
    timestamp: u32,
    delta: u32,
    len: usize,
    type_id: u8,
    stream: u32,
    extended: bool,
    extension: u32,
    active: bool,
    data: Vec<u8>,
}
pub(crate) struct Decoder {
    states: BTreeMap<u32, State>,
    size: usize,
    max_streams: usize,
    max_message: usize,
    max_assembly: usize,
}
impl Decoder {
    pub fn new(max_streams: usize, max_message: usize, max_assembly: usize) -> Self {
        Self {
            states: BTreeMap::new(),
            size: 128,
            max_streams,
            max_message,
            max_assembly,
        }
    }
    pub fn set_size(&mut self, size: u32) -> Result<(), Error> {
        if size == 0 || size > 65536 {
            return Err(Error::Limit("inbound chunk size"));
        }
        self.size = size as usize;
        Ok(())
    }
    pub fn abort(&mut self, id: u32) {
        if let Some(state) = self.states.get_mut(&id) {
            state.data = Vec::new();
            state.active = false;
        }
    }
    pub fn read<F: FnMut(&mut [u8]) -> Result<(), Error>>(
        &mut self,
        read: &mut F,
    ) -> Result<Option<Message>, Error> {
        let mut byte = [0];
        read(&mut byte)?;
        let format = byte[0] >> 6;
        let mut id = u32::from(byte[0] & 63);
        if id == 0 {
            read(&mut byte)?;
            id = 64 + u32::from(byte[0]);
        } else if id == 1 {
            let mut ext = [0; 2];
            read(&mut ext)?;
            id = 64 + u32::from(u16::from_le_bytes(ext));
        }
        if !self.states.contains_key(&id) && self.states.len() >= self.max_streams {
            return Err(Error::Limit("chunk stream count"));
        }
        let previous = self.states.get(&id);
        if previous.is_none() && format != 0 {
            return Err(Error::Protocol("compressed chunk without previous header"));
        }
        let continuation = previous.is_some_and(|s| s.active);
        if continuation && format != 3 {
            return Err(Error::Protocol(
                "new header interrupts unfinished message; Abort required",
            ));
        }
        let mut state = previous.map_or_else(State::default, |s| State {
            timestamp: s.timestamp,
            delta: s.delta,
            len: s.len,
            type_id: s.type_id,
            stream: s.stream,
            extended: s.extended,
            extension: s.extension,
            active: s.active,
            data: Vec::new(),
        });
        // Never clone an assembling payload: move it only after the header is checked.
        state.data = Vec::new();
        let mut header = [0u8; 11];
        let count = match format {
            0 => 11,
            1 => 7,
            2 => 3,
            _ => 0,
        };
        read(&mut header[..count])?;
        if format <= 2 {
            let value = u24(&header[..3]);
            state.extended = value == 0xffffff;
            state.extension = value;
            if state.extended {
                let mut ext = [0; 4];
                read(&mut ext)?;
                state.extension = u32::from_be_bytes(ext);
            }
            if format == 0 {
                state.timestamp = state.extension;
                state.delta = state.extension;
            } else {
                state.delta = state.extension;
                state.timestamp = state.timestamp.wrapping_add(state.delta);
            }
            if format <= 1 {
                state.len = u24(&header[3..6]) as usize;
                state.type_id = header[6];
            }
            if format == 0 {
                state.stream = u32::from_le_bytes(header[7..11].try_into().unwrap());
            }
        } else {
            if state.extended {
                let mut ext = [0; 4];
                read(&mut ext)?;
                if u32::from_be_bytes(ext) != state.extension {
                    return Err(Error::Protocol(
                        "inconsistent extended continuation timestamp",
                    ));
                }
            }
            if !continuation {
                state.timestamp = state.timestamp.wrapping_add(state.delta);
            }
        }
        if state.len > self.max_message {
            return Err(Error::Limit("inbound message bytes"));
        }
        if !continuation {
            let allocated = self
                .states
                .values()
                .filter(|s| s.active)
                .map(|s| s.len)
                .sum::<usize>();
            if state.len > self.max_assembly.saturating_sub(allocated) {
                return Err(Error::Limit("aggregate inbound assembly bytes"));
            }
            state.data = Vec::with_capacity(state.len);
        } else {
            state.data = std::mem::take(&mut self.states.get_mut(&id).unwrap().data);
        }
        state.active = true;
        let next = (state.len - state.data.len()).min(self.size);
        let offset = state.data.len();
        state.data.resize(offset + next, 0);
        read(&mut state.data[offset..])?;
        let result = if state.data.len() == state.len {
            state.active = false;
            Some(Message {
                timestamp: state.timestamp,
                type_id: state.type_id,
                stream: state.stream,
                data: std::mem::take(&mut state.data),
            })
        } else {
            None
        };
        self.states.insert(id, state);
        Ok(result)
    }
}
pub(crate) fn write<F: FnMut(&[u8]) -> Result<(), Error>>(
    message: &Message,
    id: u32,
    size: usize,
    write: &mut F,
) -> Result<(), Error> {
    if !(2..=65599).contains(&id) || !(1..=65536).contains(&size) || message.data.len() > 0xffffff {
        return Err(Error::Invalid("outgoing chunk fields"));
    }
    let mut first = true;
    let mut offset = 0;
    loop {
        let format = if first { 0 } else { 3 };
        let mut header = Vec::with_capacity(18);
        if id < 64 {
            header.push(format << 6 | id as u8);
        } else if id < 320 {
            header.extend_from_slice(&[format << 6, (id - 64) as u8]);
        } else {
            header.push(format << 6 | 1);
            header.extend_from_slice(&((id - 64) as u16).to_le_bytes());
        }
        let extended = message.timestamp >= 0xffffff;
        if first {
            put24(&mut header, message.timestamp.min(0xffffff));
            put24(&mut header, message.data.len() as u32);
            header.push(message.type_id);
            header.extend_from_slice(&message.stream.to_le_bytes());
        }
        if extended {
            header.extend_from_slice(&message.timestamp.to_be_bytes());
        }
        write(&header)?;
        let end = (offset + size).min(message.data.len());
        write(&message.data[offset..end])?;
        offset = end;
        if offset == message.data.len() {
            return Ok(());
        }
        first = false;
    }
}
pub(crate) fn u24(value: &[u8]) -> u32 {
    u32::from(value[0]) << 16 | u32::from(value[1]) << 8 | u32::from(value[2])
}
pub(crate) fn put24(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&[(value >> 16) as u8, (value >> 8) as u8, value as u8]);
}

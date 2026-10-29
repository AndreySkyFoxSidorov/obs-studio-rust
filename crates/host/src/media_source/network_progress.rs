//! Bounded append-only byte source for finite progressive media. No media owner
//! publishes it until transport, clock and audio-barrier scopes are attached.
use skylive_mp4::ByteSource;
use std::{io, sync::{Arc, Mutex, TryLockError, atomic::{AtomicUsize, Ordering}}};

const MAX_STREAM: usize = 32 * 1024 * 1024;
const TOTAL: usize = 64 * 1024 * 1024;
static RESERVED: AtomicUsize = AtomicUsize::new(0);
struct Inner { length: usize, state: Mutex<State> }
struct State { bytes: Vec<u8>, terminal: Option<Result<(),String>> }
impl Drop for Inner { fn drop(&mut self) { RESERVED.fetch_sub(self.length,Ordering::AcqRel); } }
#[derive(Clone)]
pub(crate) struct Reader(Arc<Inner>);
pub(crate) struct Writer { shared: Arc<Inner>, completed: bool }

pub(crate) fn create(length: usize) -> io::Result<(Writer,Reader)> {
    if length==0 || length>MAX_STREAM {return Err(io::Error::new(io::ErrorKind::InvalidData,"progressive media exceeds per-stream limit"));}
    RESERVED.fetch_update(Ordering::AcqRel,Ordering::Acquire,|used|
        used.checked_add(length).filter(|total|*total<=TOTAL))
        .map_err(|_|io::Error::new(io::ErrorKind::WouldBlock,"progressive media budget exhausted"))?;
    let inner=Arc::new(Inner {length,state:Mutex::new(State {bytes:Vec::new(),terminal:None})});
    Ok((Writer {shared:Arc::clone(&inner),completed:false},Reader(inner)))
}
impl Writer {
    pub(crate) fn append(&mut self, bytes:&[u8]) -> io::Result<()> {
        let mut state=self.shared.state.lock().map_err(|_|io::Error::other("progressive media buffer poisoned"))?;
        if state.terminal.is_some() {return Err(io::Error::other("progressive media already completed"));}
        let end=state.bytes.len().checked_add(bytes.len()).filter(|end|*end<=self.shared.length)
            .ok_or_else(||io::Error::new(io::ErrorKind::InvalidData,"progressive media exceeds declared size"))?;
        if state.bytes.capacity()<end {
            let desired=end.max(state.bytes.capacity().saturating_mul(2)).max(8192).min(self.shared.length);
            let additional=desired-state.bytes.len();
            state.bytes.try_reserve_exact(additional).map_err(|_|io::Error::other("progressive media allocation failed"))?;
        }
        state.bytes.extend_from_slice(bytes);Ok(())
    }
    pub(crate) fn finish(mut self) -> io::Result<()> {
        let mut state=self.shared.state.lock().map_err(|_|io::Error::other("progressive media buffer poisoned"))?;
        if state.bytes.len()!=self.shared.length {return Err(io::Error::new(io::ErrorKind::UnexpectedEof,"progressive media ended before declared length"));}
        state.terminal=Some(Ok(()));self.completed=true;Ok(())
    }
    pub(crate) fn fail(mut self, error:String) {
        if let Ok(mut state)=self.shared.state.lock(){state.terminal=Some(Err(error));}
        self.completed=true;
    }
}
impl Drop for Writer {
    fn drop(&mut self) {
        if !self.completed {if let Ok(mut state)=self.shared.state.lock(){state.terminal=Some(Err("progressive media transfer retired".into()));}}
    }
}
impl ByteSource for Reader {
    fn len(&self)->u64 {self.0.length as u64}
    fn read_at(&self,offset:u64,output:&mut [u8])->io::Result<()> {
        let start=usize::try_from(offset).map_err(|_|io::Error::new(io::ErrorKind::UnexpectedEof,"media byte offset overflow"))?;
        let end=start.checked_add(output.len()).filter(|end|*end<=self.0.length)
            .ok_or_else(||io::Error::new(io::ErrorKind::UnexpectedEof,"media read outside declared length"))?;
        let state=match self.0.state.try_lock() {
            Ok(state)=>state,
            Err(TryLockError::WouldBlock)=>return Err(io::Error::new(io::ErrorKind::WouldBlock,"media byte buffer is busy")),
            Err(TryLockError::Poisoned(_))=>return Err(io::Error::other("progressive media buffer poisoned")),
        };
        if end<=state.bytes.len() {output.copy_from_slice(&state.bytes[start..end]);return Ok(());}
        match &state.terminal {
            Some(Err(error))=>Err(io::Error::other(error.clone())),
            Some(Ok(()))=>Err(io::Error::new(io::ErrorKind::UnexpectedEof,"completed media is truncated")),
            None=>Err(io::Error::new(io::ErrorKind::WouldBlock,"media bytes have not arrived")),
        }
    }
}
impl Reader {
    /// Require a small encoded-data cushion to avoid cycling audio barriers for
    /// every network packet; end-of-file reduces the cushion naturally.
    pub(crate) fn ready_through(&self,end:u64)->io::Result<bool> {
        let end=usize::try_from(end).map_err(|_|io::Error::new(io::ErrorKind::InvalidData,"media offset overflow"))?;
        if end>self.0.length {return Err(io::Error::new(io::ErrorKind::UnexpectedEof,"media packet outside declared length"));}
        let state=match self.0.state.try_lock() {
            Ok(state)=>state,
            Err(TryLockError::WouldBlock)=>return Ok(false),
            Err(TryLockError::Poisoned(_))=>return Err(io::Error::other("progressive media buffer poisoned")),
        };
        if let Some(Err(error))=&state.terminal {return Err(io::Error::other(error.clone()));}
        Ok(state.bytes.len()>=end.saturating_add(256*1024).min(self.0.length))
    }
}

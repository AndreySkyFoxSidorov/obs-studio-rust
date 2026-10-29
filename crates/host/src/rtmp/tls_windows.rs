//! Windows RTMPS: client TLS 1.2 using the OS Schannel provider.
//!
//! No C/C++ wrapper, bundled SSL library, trust-store mutation or insecure mode.
//! Credentials and context live on the existing bounded RTMP owner, including
//! cancellation/retirement while a system certificate lookup is still pending.
//! See doc/migration/native-rtmps.md for the exact admitted scope and references.
use super::{Error, io_error, transport::Wire};
use std::{
    ffi::c_void,
    io::{self, Read, Write},
    net::TcpStream,
    ops::Range,
    sync::{Arc, atomic::{AtomicBool, Ordering}},
    time::{Duration, Instant},
};
use windows::{
    core::{HRESULT, PCWSTR},
    Win32::{
        Foundation::{SEC_E_OK, SEC_E_INCOMPLETE_MESSAGE, SEC_I_CONTINUE_NEEDED,
            SEC_I_CONTEXT_EXPIRED, SEC_I_INCOMPLETE_CREDENTIALS, SEC_I_RENEGOTIATE},
        Security::{Credentials::SecHandle, Authentication::Identity::*},
    },
};

const MAX_TOKEN: usize = 1024 * 1024;
const MAX_CIPHER: usize = 256 * 1024;
const MAX_PLAINTEXT: usize = 16 * 1024;
const MAX_RECORD: usize = 64 * 1024;
const REQUEST: ISC_REQ_FLAGS = ISC_REQ_FLAGS(
    ISC_REQ_REPLAY_DETECT.0 | ISC_REQ_SEQUENCE_DETECT.0 | ISC_REQ_CONFIDENTIALITY.0
        | ISC_REQ_EXTENDED_ERROR.0 | ISC_REQ_STREAM.0,
);
fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
fn security(operation: &'static str, status: HRESULT) -> io::Error {
    io::Error::new(io::ErrorKind::Other, format!("{operation}: Schannel {:#010x}", status.0 as u32))
}
fn transient(error: &io::Error) -> bool {
    matches!(error.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::Interrupted)
}
fn pending() -> io::Error {
    io::Error::new(io::ErrorKind::WouldBlock, "TLS needs another bounded socket poll")
}
fn descriptor(buffers: &mut [SecBuffer]) -> SecBufferDesc {
    SecBufferDesc { ulVersion: SECBUFFER_VERSION, cBuffers: buffers.len() as u32,
        pBuffers: buffers.as_mut_ptr() }
}
fn buffer(kind: u32, bytes: &mut [u8]) -> SecBuffer {
    SecBuffer { cbBuffer: bytes.len() as u32, BufferType: kind, pvBuffer: bytes.as_mut_ptr().cast() }
}
fn empty_handle() -> SecHandle {
    SecHandle { dwLower: usize::MAX, dwUpper: usize::MAX }
}
fn valid_handle(handle: SecHandle) -> bool {
    handle.dwLower != usize::MAX && handle.dwUpper != usize::MAX
}
struct Credentials(SecHandle);
impl Drop for Credentials {
    fn drop(&mut self) {
        if valid_handle(self.0) { unsafe { let _ = FreeCredentialsHandle(&self.0); } }
    }
}
struct Context(SecHandle);
impl Drop for Context {
    fn drop(&mut self) {
        if valid_handle(self.0) { unsafe { let _ = DeleteSecurityContext(&self.0); } }
    }
}

pub(super) struct TlsStream {
    socket: TcpStream,
    // Drop the context before its credentials, even on partial negotiation.
    context: Context,
    credentials: Credentials,
    server_name: Vec<u16>,
    token: Vec<u8>,
    encrypted: Vec<u8>,
    plain: Vec<u8>,
    plain_at: usize,
    outgoing: Vec<u8>,
    outgoing_at: usize,
    sizes: SecPkgContext_StreamSizes,
    cancel: Arc<AtomicBool>,
    closed: bool,
    finishing: bool,
    wire_sent: u64,
    wire_received: u64,
}
impl TlsStream {
    pub(super) fn connect(
        socket: TcpStream, host: &str, deadline: Instant, cancel: Arc<AtomicBool>,
    ) -> Result<Self, Error> {
        if host.is_empty() || host.len() > 253 || host.contains('\0') || !host.is_ascii() {
            return Err(Error::Invalid("TLS requires the authored ASCII server name"));
        }
        let timeout = super::transport::budget(deadline, &cancel)?;
        socket.set_read_timeout(Some(timeout)).map_err(|e| io_error("TLS read timeout", e))?;
        socket.set_write_timeout(Some(timeout)).map_err(|e| io_error("TLS write timeout", e))?;
        let package = unsafe { QuerySecurityPackageInfoW(UNISP_NAME_W) }
            .map_err(|e| io_error("query Schannel package", security("QuerySecurityPackageInfoW", e.code())))?;
        if package.is_null() { return Err(Error::Protocol("Schannel returned no package information")); }
        let token_bytes = unsafe { (*package).cbMaxToken as usize };
        unsafe { let _ = FreeContextBuffer(package.cast()); }
        if token_bytes == 0 || token_bytes > MAX_TOKEN {
            return Err(Error::Limit("Schannel token exceeds 1 MiB"));
        }
        let mut auth = SCHANNEL_CRED {
            dwVersion: SCHANNEL_CRED_VERSION,
            grbitEnabledProtocols: SP_PROT_TLS1_2_CLIENT,
            dwMinimumCipherStrength: 128,
            dwFlags: SCH_CRED_AUTO_CRED_VALIDATION | SCH_CRED_NO_DEFAULT_CREDS
                | SCH_USE_STRONG_CRYPTO | SCH_CRED_REVOCATION_CHECK_CHAIN_EXCLUDE_ROOT,
            ..Default::default()
        };
        let mut credentials = Credentials(empty_handle());
        unsafe { AcquireCredentialsHandleW(
            PCWSTR::null(), UNISP_NAME_W, SECPKG_CRED_OUTBOUND, None,
            Some((&mut auth as *mut SCHANNEL_CRED).cast::<c_void>()), None, None,
            &mut credentials.0, None,
        ) }.map_err(|e| io_error("acquire TLS credentials", security("AcquireCredentialsHandleW", e.code())))?;
        let mut stream = Self {
            socket, context: Context(empty_handle()), credentials,
            server_name: host.encode_utf16().chain(Some(0)).collect(),
            token: vec![0; token_bytes], encrypted: Vec::new(), plain: Vec::new(), plain_at: 0,
            outgoing: Vec::new(), outgoing_at: 0, sizes: Default::default(), cancel,
            closed: false, finishing: false, wire_sent: 0, wire_received: 0,
        };
        stream.handshake(deadline)?;
        unsafe { QueryContextAttributesW(&stream.context.0, SECPKG_ATTR_STREAM_SIZES,
            (&mut stream.sizes as *mut SecPkgContext_StreamSizes).cast()) }
            .map_err(|e| io_error("query TLS record sizes", security("QueryContextAttributesW", e.code())))?;
        let size = stream.sizes;
        if size.cbMaximumMessage == 0 || size.cbMaximumMessage as usize > MAX_PLAINTEXT
            || size.cbHeader as usize + size.cbMaximumMessage as usize + size.cbTrailer as usize > MAX_RECORD
            || size.cBuffers != 4
        { return Err(Error::Limit("Schannel record layout exceeds bounded stream profile")); }
        // Negotiation storage is not retained throughout a broadcast.
        stream.token = Vec::new();
        if stream.encrypted.len() > MAX_CIPHER {
            return Err(Error::Limit("post-handshake TLS ciphertext exceeds 256 KiB"));
        }
        let remainder = std::mem::take(&mut stream.encrypted);
        stream.encrypted = remainder.into_boxed_slice().into_vec();
        Ok(stream)
    }
    fn cancelled(&self) -> io::Result<()> {
        if self.cancel.load(Ordering::Acquire) {
            Err(io::Error::new(io::ErrorKind::ConnectionAborted, "TLS operation cancelled"))
        } else { Ok(()) }
    }
    fn handshake(&mut self, deadline: Instant) -> Result<(), Error> {
        let mut first = true;
        let mut need_read = false;
        let mut received = 0usize;
        // Work and storage bounds are independent. Repeated tiny peer tokens
        // cannot keep this owner indefinitely after the absolute deadline.
        for _ in 0..4096 {
            let timeout = super::transport::budget(deadline, &self.cancel)?;
            self.set_timeout(timeout).map_err(|e| io_error("set TLS handshake timeout", e))?;
            if need_read {
                match self.read_cipher(MAX_TOKEN) {
                    Ok(n) => { received += n; if received > 4 * MAX_TOKEN {
                        return Err(Error::Limit("TLS handshake input exceeds 4 MiB"));
                    } }
                    Err(error) if transient(&error) => continue,
                    Err(error) => return Err(io_error("read TLS handshake", error)),
                }
            }
            let mut input = [buffer(SECBUFFER_TOKEN, &mut self.encrypted), SecBuffer::default()];
            let input_desc = descriptor(&mut input);
            let mut output = [buffer(SECBUFFER_TOKEN, &mut self.token)];
            let mut output_desc = descriptor(&mut output);
            let mut attributes = 0;
            let existing = (!first).then_some(&self.context.0 as *const SecHandle);
            let status = unsafe { InitializeSecurityContextW(
                Some(&self.credentials.0), existing, Some(self.server_name.as_ptr()), REQUEST,
                0, SECURITY_NATIVE_DREP, (!first).then_some(&input_desc as *const SecBufferDesc), 0,
                Some(&mut self.context.0), Some(&mut output_desc), &mut attributes, None,
            ) };
            if status == SEC_E_INCOMPLETE_MESSAGE {
                if first { return Err(Error::Protocol("incomplete initial TLS context")); }
                // Input remains unconsumed on INCOMPLETE_MESSAGE.
                need_read = true;
                continue;
            }
            if status != SEC_E_OK && status != SEC_I_CONTINUE_NEEDED {
                if status == SEC_I_INCOMPLETE_CREDENTIALS {
                    return Err(Error::Unsupported("RTMPS client certificate authentication is not configured"));
                }
                return Err(io_error("negotiate TLS", security("InitializeSecurityContextW", status)));
            }
            if output[0].cbBuffer as usize > self.token.len()
                || (output[0].cbBuffer != 0 && output[0].pvBuffer != self.token.as_mut_ptr().cast())
            { return Err(Error::Protocol("Schannel output token does not fit its owned buffer")); }
            let token_len = output[0].cbBuffer as usize;
            let mut sent = 0;
            while sent < token_len {
                self.set_timeout(super::transport::budget(deadline, &self.cancel)?)
                    .map_err(|e| io_error("set TLS token timeout", e))?;
                match self.socket.write(&self.token[sent..token_len]) {
                    Ok(0) => return Err(Error::ConnectionLost{operation:"TLS handshake write",detail:"peer disconnected during TLS negotiation".into()}),
                    Ok(n) => {
                        sent += n;
                        self.wire_sent = self.wire_sent.checked_add(n as u64)
                            .ok_or(Error::Limit("TLS sent byte counter"))?;
                    }
                    Err(error) if transient(&error) => continue,
                    Err(error) => return Err(io_error("write TLS handshake", error)),
                }
            }
            if !first {
                let extra = input.iter().find(|b| b.BufferType == SECBUFFER_EXTRA).map_or(0, |b| b.cbBuffer as usize);
                if extra > self.encrypted.len() { return Err(Error::Protocol("invalid TLS handshake remainder")); }
                let consumed = self.encrypted.len() - extra;
                if status == SEC_I_CONTINUE_NEEDED && consumed == 0 && extra != 0 {
                    return Err(Error::Protocol("TLS handshake made no input progress"));
                }
                self.encrypted.copy_within(consumed.., 0);
                self.encrypted.truncate(extra);
            }
            first = false;
            if status == SEC_E_OK {
                let required = ISC_RET_CONFIDENTIALITY | ISC_RET_STREAM;
                if attributes & required != required {
                    return Err(Error::Protocol("TLS context lacks confidentiality or stream protection"));
                }
                return Ok(());
            }
            need_read = self.encrypted.is_empty();
        }
        Err(Error::Limit("TLS handshake iteration bound"))
    }
    fn read_cipher(&mut self, maximum: usize) -> io::Result<usize> {
        self.cancelled()?;
        let available = maximum.checked_sub(self.encrypted.len()).filter(|n| *n != 0)
            .ok_or_else(|| invalid("TLS encrypted input exceeds its bound"))?;
        let mut scratch = [0u8; 16 * 1024];
        let count = self.socket.read(&mut scratch[..available.min(16 * 1024)])?;
        if count == 0 { return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "TLS peer closed without close_notify")); }
        self.wire_received = self.wire_received.checked_add(count as u64)
            .ok_or_else(|| invalid("TLS received byte counter overflow"))?;
        self.encrypted.try_reserve_exact(count)
            .map_err(|_| io::Error::new(io::ErrorKind::OutOfMemory, "TLS ciphertext allocation failed"))?;
        self.encrypted.extend_from_slice(&scratch[..count]);
        Ok(count)
    }
    fn range(&self, native: &SecBuffer) -> io::Result<Range<usize>> {
        let start = (native.pvBuffer as usize).checked_sub(self.encrypted.as_ptr() as usize)
            .ok_or_else(|| invalid("TLS output pointer precedes owned ciphertext"))?;
        let end = start.checked_add(native.cbBuffer as usize)
            .filter(|end| *end <= self.encrypted.len())
            .ok_or_else(|| invalid("TLS output lies outside owned ciphertext"))?;
        Ok(start..end)
    }
    fn fill_plain(&mut self) -> io::Result<bool> {
        self.cancelled()?;
        if self.plain_at < self.plain.len() { return Ok(true); }
        self.plain.clear(); self.plain_at = 0;
        if self.closed { return Ok(false); }
        let mut read_once = false;
        for _ in 0..8 {
            if self.encrypted.is_empty() {
                if read_once { return Err(pending()); }
                self.read_cipher(MAX_CIPHER)?; read_once = true;
            }
            let mut buffers = [buffer(SECBUFFER_DATA, &mut self.encrypted),
                SecBuffer::default(), SecBuffer::default(), SecBuffer::default()];
            let desc = descriptor(&mut buffers);
            let status = unsafe { DecryptMessage(&self.context.0, &desc, 0, None) };
            if status == SEC_E_INCOMPLETE_MESSAGE {
                if read_once { return Err(pending()); }
                self.read_cipher(MAX_CIPHER)?; read_once = true; continue;
            }
            if status == SEC_I_RENEGOTIATE {
                // TLS1.2 only: no post-handshake TLS1.3 tickets are negotiated.
                // Server-requested renegotiation is terminal, never plaintext.
                return Err(io::Error::new(io::ErrorKind::Unsupported, "RTMPS TLS1.2 renegotiation is not supported"));
            }
            if status != SEC_E_OK && status != SEC_I_CONTEXT_EXPIRED {
                return Err(security("DecryptMessage", status));
            }
            let mut plain = None;
            let mut extra = None;
            for entry in &buffers {
                if entry.cbBuffer == 0 { continue; }
                match entry.BufferType {
                    SECBUFFER_DATA => {
                        if plain.is_some() { return Err(invalid("multiple TLS application-data buffers")); }
                        let range = self.range(entry)?;
                        if range.len() > MAX_PLAINTEXT { return Err(invalid("TLS plaintext exceeds 16 KiB")); }
                        plain = Some(range);
                    }
                    SECBUFFER_EXTRA => {
                        if extra.is_some() { return Err(invalid("multiple TLS ciphertext remainders")); }
                        // SSPI defines EXTRA by its byte count; unlike DATA,
                        // its pointer need not identify the unconsumed suffix.
                        let start = self.encrypted.len().checked_sub(entry.cbBuffer as usize)
                            .ok_or_else(|| invalid("TLS remainder exceeds owned ciphertext"))?;
                        extra = Some(start..self.encrypted.len());
                    }
                    _ => {}
                }
            }
            if let Some(range) = plain { self.plain.extend_from_slice(&self.encrypted[range]); }
            if let Some(range) = extra {
                if range.start == 0 { return Err(invalid("TLS record made no input progress")); }
                let remaining = range.len();
                self.encrypted.copy_within(range, 0); self.encrypted.truncate(remaining);
            } else { self.encrypted.clear(); }
            if status == SEC_I_CONTEXT_EXPIRED { self.closed = true; }
            if !self.plain.is_empty() { return Ok(true); }
            if self.closed { return Ok(false); }
        }
        Err(pending())
    }
    fn queue_close_notify(&mut self) -> io::Result<()> {
        let mut shutdown = SCHANNEL_SHUTDOWN.to_ne_bytes();
        let mut input = [buffer(SECBUFFER_TOKEN, &mut shutdown)];
        let input_desc = descriptor(&mut input);
        unsafe { ApplyControlToken(&self.context.0, &input_desc) }
            .map_err(|e| security("ApplyControlToken", e.code()))?;
        // Use a small bounded token for a TLS1.2 close_notify (no new handshake).
        let mut token = [0u8; MAX_RECORD];
        let mut output = [buffer(SECBUFFER_TOKEN, &mut token)];
        let mut output_desc = descriptor(&mut output);
        let mut attributes = 0;
        let existing = self.context.0;
        let status = unsafe { InitializeSecurityContextW(
            Some(&self.credentials.0), Some(&existing), Some(self.server_name.as_ptr()), REQUEST,
            0, SECURITY_NATIVE_DREP, None, 0, Some(&mut self.context.0), Some(&mut output_desc),
            &mut attributes, None,
        ) };
        if status != SEC_E_OK && status != SEC_I_CONTEXT_EXPIRED && status != SEC_I_CONTINUE_NEEDED {
            return Err(security("generate TLS close_notify", status));
        }
        let count = output[0].cbBuffer as usize;
        if count > token.len() || (count != 0 && output[0].pvBuffer != token.as_mut_ptr().cast()) {
            return Err(invalid("TLS close_notify does not fit its owned buffer"));
        }
        self.outgoing.extend_from_slice(&token[..count]); self.outgoing_at = 0;
        self.finishing = true;
        Ok(())
    }
}
impl Read for TlsStream {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        if bytes.is_empty() { return Ok(0); }
        if !self.fill_plain()? { return Ok(0); }
        let count = bytes.len().min(self.plain.len() - self.plain_at);
        bytes[..count].copy_from_slice(&self.plain[self.plain_at..self.plain_at + count]);
        self.plain_at += count;
        Ok(count)
    }
}
impl Write for TlsStream {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.cancelled()?;
        if bytes.is_empty() { return Ok(0); }
        if self.closed || self.finishing { return Err(io::Error::new(io::ErrorKind::BrokenPipe, "TLS stream is closing")); }
        // Flush ONLY previously accepted bytes before accepting another record.
        // A timeout after acceptance must not make RTMP resend the plaintext.
        self.flush()?;
        let count = bytes.len().min(self.sizes.cbMaximumMessage as usize);
        let header = self.sizes.cbHeader as usize;
        let trailer = self.sizes.cbTrailer as usize;
        self.outgoing.try_reserve_exact(header + count + trailer)
            .map_err(|_| io::Error::new(io::ErrorKind::OutOfMemory, "TLS record allocation failed"))?;
        self.outgoing.resize(header + count + trailer, 0);
        self.outgoing[header..header + count].copy_from_slice(&bytes[..count]);
        let pointer = self.outgoing.as_mut_ptr();
        let mut buffers = [
            SecBuffer { cbBuffer: header as u32, BufferType: SECBUFFER_STREAM_HEADER, pvBuffer: pointer.cast() },
            SecBuffer { cbBuffer: count as u32, BufferType: SECBUFFER_DATA, pvBuffer: unsafe { pointer.add(header) }.cast() },
            SecBuffer { cbBuffer: trailer as u32, BufferType: SECBUFFER_STREAM_TRAILER, pvBuffer: unsafe { pointer.add(header + count) }.cast() },
            SecBuffer::default(),
        ];
        let desc = descriptor(&mut buffers);
        let status = unsafe { EncryptMessage(&self.context.0, 0, &desc, 0) };
        if status != SEC_E_OK { self.outgoing.clear(); return Err(security("EncryptMessage", status)); }
        // Schannel may shorten its trailer. Compact each returned region; do
        // not transmit unused capacity or assume a fixed authenticated length.
        let original_len = self.outgoing.len();
        let mut end = 0usize;
        for entry in &buffers[..3] {
            let start = (entry.pvBuffer as usize).checked_sub(pointer as usize)
                .ok_or_else(|| invalid("TLS encrypted output precedes owned record"))?;
            let stop = start.checked_add(entry.cbBuffer as usize).filter(|n| *n <= original_len)
                .ok_or_else(|| invalid("TLS encrypted output exceeds owned record"))?;
            if start < end { return Err(invalid("TLS encrypted output regions overlap")); }
            self.outgoing.copy_within(start..stop, end); end += stop - start;
        }
        self.outgoing.truncate(end); self.outgoing_at = 0;
        Ok(count)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.cancelled()?;
        if self.outgoing_at < self.outgoing.len() {
            self.cancelled()?;
            match self.socket.write(&self.outgoing[self.outgoing_at..]) {
                Ok(0) => return Err(io::Error::new(io::ErrorKind::WriteZero, "TLS socket write made no progress")),
                Ok(n) => {
                    self.outgoing_at += n;
                    self.wire_sent = self.wire_sent.checked_add(n as u64)
                        .ok_or_else(|| invalid("TLS sent byte counter overflow"))?;
                }
                Err(error) => return Err(error),
            }
        }
        // Return control even after a partial successful socket write. The
        // publisher must recheck its absolute deadline before the next one;
        // a peer accepting one byte per poll cannot extend the operation.
        if self.outgoing_at < self.outgoing.len() { return Err(pending()); }
        self.outgoing.clear(); self.outgoing_at = 0;
        Ok(())
    }
}
impl Wire for TlsStream {
    fn set_timeout(&mut self, value: Duration) -> io::Result<()> {
        self.socket.set_read_timeout(Some(value))?;
        self.socket.set_write_timeout(Some(value))
    }
    fn ready(&mut self) -> io::Result<bool> {
        match self.fill_plain() {
            Ok(true) => Ok(true),
            Ok(false) => Err(io::Error::new(io::ErrorKind::UnexpectedEof, "TLS peer sent close_notify")),
            Err(error) if transient(&error) => Ok(false),
            Err(error) => Err(error),
        }
    }
    fn finish(&mut self) -> io::Result<()> {
        self.flush()?;
        if !self.finishing { self.queue_close_notify()?; }
        self.flush()
    }
    fn physical_counts(&self) -> Option<(u64, u64)> {
        Some((self.wire_sent, self.wire_received))
    }
}

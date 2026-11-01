//! Bounded, cancellable HTTP acquisition for a finite media object.
//!
//! This transport supports bounded finite HTTP(S) media. It does not implement
//! live streams. Callers must not substitute the
//! resulting bytes for the authored URL in the document.
use std::{io::{self, Read, Write}, net::{IpAddr, SocketAddr, TcpStream, ToSocketAddrs}, sync::{Arc, atomic::{AtomicBool, AtomicUsize, Ordering}, mpsc::{self, Receiver, TryRecvError}}, thread, time::{Duration, Instant}};

const MAX_HEADER: usize = 16 * 1024;
const MAX_BODY: usize = 32 * 1024 * 1024;
const IO_SLICE: Duration = Duration::from_millis(250);
const TOTAL_TIME: Duration = Duration::from_secs(30);
const MAX_WORKERS: usize = 4;
static ACTIVE: AtomicUsize = AtomicUsize::new(0);
static DNS_ACTIVE: AtomicUsize = AtomicUsize::new(0);
struct Permit;
impl Drop for Permit { fn drop(&mut self) { ACTIVE.fetch_sub(1, Ordering::Release); } }
struct DnsPermit;
impl Drop for DnsPermit { fn drop(&mut self) { DNS_ACTIVE.fetch_sub(1, Ordering::Release); } }

/// Request target and Host header are independent of the resolved socket.
pub struct Request {
    address: Option<SocketAddr>, hostname: String, port: u16,
    host: String, target: String, secure: bool, authorization: Option<String>,
}
// Decode URI user-info to bytes before constructing the HTTP Basic token.
// Bounds are independent of the overall URL and malformed escapes are rejected.
fn userinfo_bytes(input: &str) -> io::Result<Vec<u8>> {
    if input.len() > 8192 { return Err(invalid("HTTP user-info exceeds its limit")); }
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let digit = |b: u8| match b { b'0'..=b'9' => Some(b-b'0'), b'a'..=b'f' => Some(b-b'a'+10), b'A'..=b'F' => Some(b-b'A'+10), _ => None };
            let hi = bytes.get(i+1).and_then(|b| digit(*b)).ok_or_else(|| invalid("invalid HTTP user-info escape"))?;
            let lo = bytes.get(i+2).and_then(|b| digit(*b)).ok_or_else(|| invalid("invalid HTTP user-info escape"))?;
            out.push((hi<<4)|lo); i+=3;
        } else { out.push(bytes[i]); i+=1; }
    }
    Ok(out)
}
fn basic_authorization(user_info: &str) -> io::Result<String> {
    let (user, password) = user_info.split_once(':').unwrap_or((user_info, ""));
    let mut credentials = userinfo_bytes(user)?;
    if credentials.contains(&b':') { return Err(invalid("HTTP Basic username cannot contain a colon")); }
    credentials.push(b':');
    credentials.extend_from_slice(&userinfo_bytes(password)?);
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut value = String::with_capacity(6 + credentials.len().div_ceil(3)*4);
    value.push_str("Basic ");
    for chunk in credentials.chunks(3) {
        let a=chunk[0]; let b=chunk.get(1).copied().unwrap_or(0); let c=chunk.get(2).copied().unwrap_or(0);
        value.push(TABLE[(a>>2) as usize] as char);
        value.push(TABLE[(((a&3)<<4)|(b>>4)) as usize] as char);
        value.push(if chunk.len()>1 {TABLE[(((b&15)<<2)|(c>>6)) as usize] as char} else {'='});
        value.push(if chunk.len()>2 {TABLE[(c&63) as usize] as char} else {'='});
    }
    Ok(value)
}
pub fn is_url(value: &str) -> bool {
    value.get(..7).is_some_and(|prefix| prefix.eq_ignore_ascii_case("http://"))
        || value.get(..8).is_some_and(|prefix| prefix.eq_ignore_ascii_case("https://"))
}
/// Names accepted by FFmpeg's MOV-family demuxer. An empty value asks the
/// finite reader to detect the container. This only admits the name: actual
/// files must still pass SkyLive's ISO BMFF parser and codec restrictions.
pub fn supports_input_format(value: &str) -> bool {
    value.is_empty() || ["mov", "mp4", "m4a", "3gp", "3g2", "mj2"]
        .iter().any(|name|value.eq_ignore_ascii_case(name))
}
impl Request {
    pub fn http_url(url: &str) -> io::Result<Self> {
        let (secure,rest) = if url.get(..7).is_some_and(|prefix| prefix.eq_ignore_ascii_case("http://")) { (false,&url[7..]) }
            else if url.get(..8).is_some_and(|prefix| prefix.eq_ignore_ascii_case("https://")) { (true,&url[8..]) }
            else { return Err(invalid("only finite HTTP(S) URLs are supported")); };
        let default_port = if secure {443} else {80};
        // URI fragments are client-side only and must never be sent as part
        // of an HTTP request target. Keep the authored URL unchanged elsewhere.
        let rest = rest.split_once('#').map_or(rest, |(resource, _)| resource);
        let split = rest.find(|c| c == '/' || c == '?').unwrap_or(rest.len());
        let authority = &rest[..split];
        let (authorization,authority) = match authority.rsplit_once('@') {
            Some((user_info,authority)) => {
                if user_info.contains('@') { return Err(invalid("invalid HTTP user-info")); }
                (Some(basic_authorization(user_info)?),authority)
            },
            None => (None,authority),
        };
        let tail = &rest[split..];
        let raw_target = if tail.is_empty() { "/".to_owned() } else if tail.starts_with('?') { format!("/{tail}") } else { tail.to_owned() };
        // Preserve valid escapes without permitting malformed percent sequences
        // to reach the server or to be interpreted differently after redirects.
        let bytes = raw_target.as_bytes();
        for (index, byte) in bytes.iter().enumerate() {
            if *byte == b'%' && !(bytes.get(index + 1).is_some_and(u8::is_ascii_hexdigit)
                && bytes.get(index + 2).is_some_and(u8::is_ascii_hexdigit)) {
                return Err(invalid("invalid HTTP percent escape"));
            }
        }
        // HTTP request targets are ASCII. Preserve existing escapes and URI
        // delimiters while encoding UTF-8 path/query bytes and spaces, so a
        // copied international URL is usable without changing its meaning.
        let mut target = String::with_capacity(raw_target.len());
        for byte in raw_target.bytes() {
            // Raw control bytes cannot appear in an HTTP request target. In
            // particular, CR/LF would terminate the request line and inject
            // headers. Percent-encoded controls remain ordinary URI bytes.
            if byte.is_ascii_control() { return Err(invalid("invalid HTTP request target")); }
            // These ASCII characters are not allowed in an HTTP URI. Encode
            // them rather than sending an invalid origin-form request target.
            if byte == b' ' || !byte.is_ascii() || matches!(byte, b'"' | b'<' | b'>' | b'\\' | b'^' | b'`' | b'{' | b'|' | b'}') {
                use std::fmt::Write as _;
                write!(&mut target, "%{byte:02X}").map_err(|_| invalid("invalid HTTP request target"))?;
            } else {
                target.push(char::from(byte));
            }
            if target.len() > 8192 { return Err(invalid("invalid HTTP request target")); }
        }
        // HTTP URI authorities require brackets around IPv6 literals. Without
        // them the Host header is ambiguous and cannot be used as a redirect
        // base, even though Rust's IpAddr parser accepts the bare literal.
        if authority.starts_with('[') && !authority.contains(']') {
            return Err(invalid("invalid bracketed HTTP authority"));
        }
        if authority.starts_with('[') && authority.split_once(']').is_some_and(|(literal, _)| literal[1..].parse::<std::net::Ipv6Addr>().is_err()) {
            return Err(invalid("bracketed HTTP authority requires IPv6"));
        }
        if !authority.starts_with('[') && authority.parse::<std::net::Ipv6Addr>().is_ok() {
            return Err(invalid("IPv6 HTTP authorities require brackets"));
        }
        let (hostname,port,address) = if let Ok(socket) = authority.parse::<SocketAddr>() {
            (socket.ip().to_string(), socket.port(), Some(socket))
        } else if let Ok(ip) = authority.strip_prefix('[').and_then(|v| v.strip_suffix(']')).unwrap_or(authority).parse::<IpAddr>() {
            (ip.to_string(), default_port, Some(SocketAddr::new(ip, default_port)))
        } else {
            let (name, port) = if let Some((name, port)) = authority.rsplit_once(':') {
                if port.is_empty() || !port.bytes().all(|byte| byte.is_ascii_digit()) {
                    return Err(invalid("invalid HTTP port"));
                }
                (name, port.parse::<u16>().map_err(|_| invalid("invalid HTTP port"))?)
            } else { (authority,default_port) };
            let name = match url::Host::parse(name) {
                Ok(url::Host::Domain(domain)) => domain,
                _ => return Err(invalid("invalid HTTP hostname")),
            };
            if name.is_empty() || name.len() > 253 || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.') {
                return Err(invalid("invalid HTTP hostname"));
            }
            (name,port,None)
        };
        if port == 0 { return Err(invalid("invalid HTTP port")); }
        // DNS, TLS SNI and Host must use the same IDNA-normalized name.
        // Preserve explicit ports and numeric addresses exactly as authored.
        let host = if address.is_none() {
            if authority.rsplit_once(':').is_some() { format!("{hostname}:{port}") } else { hostname.clone() }
        } else { authority.to_owned() };
        let request = Self { address, hostname, port, host, target, secure, authorization };
        validate(&request)?;
        Ok(request)
    }
}

pub struct Owner {
    cancelled: Arc<AtomicBool>,
    result: Receiver<io::Result<Vec<u8>>>,
    progressive: Option<Arc<std::sync::Mutex<Option<super::network_progress::Reader>>>>,
}

impl Owner {
    /// Start one bounded request. Dropping the owner cancels it; socket reads
    /// and writes are interrupted by their bounded timeouts.
    pub fn start(request: Request) -> io::Result<Self> {
        Self::start_mode(request,false)
    }
    /// Start a finite transfer with an optional growing snapshot when the
    /// response uses Content-Length. Chunked bodies continue to completion.
    pub(crate) fn start_progressive(request: Request) -> io::Result<Self> {
        Self::start_mode(request,true)
    }
    fn start_mode(request: Request, progressive: bool) -> io::Result<Self> {
        validate(&request)?;
        ACTIVE.fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| (count < MAX_WORKERS).then_some(count + 1))
            .map_err(|_| io::Error::new(io::ErrorKind::WouldBlock, "media HTTP worker is busy"))?;
        let worker_permit = Permit;
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancelled = Arc::clone(&cancelled);
        let progressive = progressive.then(||Arc::new(std::sync::Mutex::new(None)));
        let worker_progressive = progressive.clone();
        let (sender, result) = mpsc::sync_channel(1);
        thread::Builder::new().name("media-http".into()).spawn(move || {
            let outcome = acquire(request, &worker_cancelled, worker_progressive.as_ref());
            // A retired owner drops the receiver and the bounded result.
            let _ = sender.send(outcome);
            drop(worker_permit);
        })?;
        Ok(Self { cancelled, result, progressive })
    }
    pub(crate) fn progressive(&self) -> Option<super::network_progress::Reader> {
        self.progressive.as_ref()?.try_lock().ok()?.clone()
    }
    pub fn cancel(&self) { self.cancelled.store(true, Ordering::Release); }
    /// Returns None while the worker is running, or its single terminal result.
    pub fn poll(&self) -> Option<io::Result<Vec<u8>>> {
        match self.result.try_recv() {
            Ok(value) => Some(value),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Err(io::Error::new(io::ErrorKind::BrokenPipe, "media HTTP worker stopped"))),
        }
    }
}
impl Drop for Owner { fn drop(&mut self) { self.cancel(); } }

fn invalid(message: &'static str) -> io::Error { io::Error::new(io::ErrorKind::InvalidData, message) }
fn validate(request: &Request) -> io::Result<()> {
    if request.host.is_empty() || request.host.len() > 1024 || !request.host.bytes().all(|c| c.is_ascii_graphic() && c != b'/' && c != b'\\') {
        return Err(invalid("invalid HTTP authority"));
    }
    if !request.target.starts_with('/') || request.target.len() > 8192 || !request.target.bytes().all(|c| c.is_ascii_graphic()) {
        return Err(invalid("invalid HTTP request target"));
    }
    Ok(())
}
fn check(cancel: &AtomicBool, deadline: Instant) -> io::Result<()> {
    if cancel.load(Ordering::Acquire) { return Err(io::Error::new(io::ErrorKind::Interrupted, "media HTTP request cancelled")); }
    if Instant::now() >= deadline { return Err(io::Error::new(io::ErrorKind::TimedOut, "media HTTP request timed out")); }
    Ok(())
}
fn retryable(error: &io::Error) -> bool { matches!(error.kind(), io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted) }
trait NetworkStream: Read + Write {}
impl<T: Read + Write> NetworkStream for T {}

#[cfg(any(target_os="windows",target_os="macos"))]
fn tls_stream(socket: TcpStream, hostname: &str, cancel: &AtomicBool, deadline: Instant) -> io::Result<Box<dyn NetworkStream>> {
    // The platform connector validates both the server certificate and name;
    // never set danger_accept_invalid_certs or disable hostname verification.
    let connector = native_tls::TlsConnector::new().map_err(io::Error::other)?;
    socket.set_nonblocking(true)?;
    let mut handshake = connector.connect(hostname, socket);
    let stream = loop {
        check(cancel,deadline)?;
        match handshake {
            Ok(stream) => break stream,
            Err(native_tls::HandshakeError::Failure(error)) => return Err(io::Error::other(error)),
            Err(native_tls::HandshakeError::WouldBlock(pending)) => {
                thread::sleep(Duration::from_millis(10));
                handshake = pending.handshake();
            }
        }
    };
    stream.get_ref().set_nonblocking(false)?;
    Ok(Box::new(stream))
}
#[cfg(not(any(target_os="windows",target_os="macos")))]
fn tls_stream(_socket: TcpStream, _hostname: &str, _cancel: &AtomicBool, _deadline: Instant) -> io::Result<Box<dyn NetworkStream>> {
    Err(io::Error::new(io::ErrorKind::Unsupported,"HTTPS media is not supported on this platform"))
}

fn resolve(request: &Request, cancel: &AtomicBool, deadline: Instant) -> io::Result<Vec<SocketAddr>> {
    if let Some(address) = request.address { return Ok(vec![address]); }
    DNS_ACTIVE.fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| (count < MAX_WORKERS).then_some(count + 1))
        .map_err(|_| io::Error::new(io::ErrorKind::WouldBlock,"media DNS worker is busy"))?;
    let permit = DnsPermit;
    let host = request.hostname.clone();
    let port = request.port;
    let (sender,receiver) = mpsc::sync_channel(1);
    thread::Builder::new().name("media-dns".into()).spawn(move || {
        let result = (host.as_str(),port).to_socket_addrs().map(|addresses| addresses.take(16).collect::<Vec<_>>());
        let _ = sender.send(result);
        drop(permit);
    })?;
    let dns_deadline = Instant::now().checked_add(Duration::from_secs(3)).unwrap_or(deadline).min(deadline);
    loop {
        check(cancel,dns_deadline)?;
        match receiver.recv_timeout(IO_SLICE) {
            Ok(result) => { let addresses = result?; return if addresses.is_empty() { Err(invalid("HTTP hostname has no addresses")) } else { Ok(addresses) }; }
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => return Err(io::Error::new(io::ErrorKind::BrokenPipe,"media DNS worker stopped")),
        }
    }
}

enum Response { Redirect(String), Complete(Vec<u8>) }
#[derive(Clone,Copy)]
enum BodyMode { Fixed(usize), Chunked, UntilClose }
enum Head { Redirect(String), Body(BodyMode) }

fn acquire(mut request: Request, cancel: &AtomicBool, progressive: Option<&Arc<std::sync::Mutex<Option<super::network_progress::Reader>>>>) -> io::Result<Vec<u8>> {
    let deadline = Instant::now() + TOTAL_TIME;
    for hop in 0..=5 {
        match acquire_once(&request,cancel,deadline,progressive)? {
            Response::Complete(body) => return Ok(body),
            Response::Redirect(location) => {
                if hop == 5 { return Err(invalid("HTTP redirect limit exceeded")); }
                let scheme = if request.secure {"https"} else {"http"};
                let base = url::Url::parse(&format!("{scheme}://{}{}",request.host,request.target))
                    .map_err(|_| invalid("invalid HTTP redirect base"))?;
                let next_url = base.join(&location).map_err(|_| invalid("invalid HTTP redirect location"))?;
                let mut next = Request::http_url(next_url.as_str())?;
                if request.secure && !next.secure { return Err(invalid("HTTPS to HTTP redirect is unsupported")); }
                // A server redirect cannot introduce new credentials. Existing
                // credentials survive only on the same scheme/host/port.
                if next.authorization.is_some() { return Err(invalid("credentials in HTTP redirects are unsupported")); }
                if next.secure == request.secure && next.hostname == request.hostname && next.port == request.port {
                    next.authorization = request.authorization.clone();
                }
                request = next;
            }
        }
    }
    Err(invalid("HTTP redirect limit exceeded"))
}

fn acquire_once(request: &Request, cancel: &AtomicBool, deadline: Instant, progressive: Option<&Arc<std::sync::Mutex<Option<super::network_progress::Reader>>>>) -> io::Result<Response> {
    check(cancel, deadline)?;
    let addresses = resolve(request,cancel,deadline)?;
    let mut last_error = None;
    let mut stream = None;
    for address in addresses.into_iter().take(4) {
        check(cancel,deadline)?;
        // Redirects and DNS consume the same request deadline.  Do not let
        // a late connection attempt extend the bounded transfer by another
        // three seconds after that deadline has expired.
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() { return Err(io::Error::new(io::ErrorKind::TimedOut, "media HTTP request timed out")); }
        match TcpStream::connect_timeout(&address, remaining.min(Duration::from_secs(3))) {
            Ok(socket) => { stream = Some(socket); break; },
            Err(error) => last_error = Some(error),
        }
    }
    let socket = stream.ok_or_else(|| last_error.unwrap_or_else(|| invalid("HTTP host has no connectable addresses")))?;
    socket.set_read_timeout(Some(IO_SLICE))?;
    socket.set_write_timeout(Some(IO_SLICE))?;
    let mut stream: Box<dyn NetworkStream> = if request.secure { tls_stream(socket,&request.hostname,cancel,deadline)? } else { Box::new(socket) };
    let authorization = request.authorization.as_ref().map_or_else(String::new, |value| format!("Authorization: {value}\r\n"));
    let request_bytes = format!("GET {} HTTP/1.1\r\nHost: {}\r\n{}Accept-Encoding: identity\r\nConnection: close\r\n\r\n", request.target, request.host, authorization);
    let mut written = 0;
    while written < request_bytes.len() {
        check(cancel, deadline)?;
        match stream.write(&request_bytes.as_bytes()[written..]) {
            Ok(0) => return Err(io::Error::new(io::ErrorKind::WriteZero, "HTTP request write failed")),
            Ok(n) => written += n,
            Err(error) if retryable(&error) => continue,
            Err(error) => return Err(error),
        }
    }
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 8192];
    let mut informational = 0usize;
    let header_end = loop {
        check(cancel, deadline)?;
        if let Some(index) = buffer.windows(4).position(|part| part == b"\r\n\r\n") {
            let end = index + 4;
            if end > MAX_HEADER { return Err(invalid("HTTP headers too large")); }
            let line_end = buffer.windows(2).position(|part| part == b"\r\n").ok_or_else(||invalid("invalid HTTP status"))?;
            // Reason phrases may contain opaque obs-text octets; only the
            // protocol version and status code need to be ASCII.
            let status: String = buffer[..line_end].iter().map(|&byte| char::from(byte)).collect();
            // A bare LF or another control byte inside the status line must
            // not be interpreted as part of the optional reason phrase.
            if buffer[..line_end].iter().any(|&b| (b < b' ' && b != b'\t') || b == 0x7f) {
                return Err(invalid("invalid HTTP status line"));
            }
            // HTTP status-line separators are SP, not arbitrary Unicode whitespace.
            let mut fields = status.split(' ');
            let version = fields.next();
            let code = fields.next();
            if matches!(version,Some("HTTP/1.0"|"HTTP/1.1")) && code.is_some_and(|code| code.len()==3 && code.starts_with('1') && code.bytes().all(|b|b.is_ascii_digit())) {
                if code == Some("101") { return Err(invalid("HTTP protocol upgrade is unsupported")); }
                // Informational responses have the same field syntax as the
                // final response; do not silently discard malformed fields.
                let info_fields = if line_end < index { &buffer[line_end+2..index] } else { &[] };
                let mut info_lines = info_fields.split(|&b| b == b'\n').peekable();
                while let Some(line) = info_lines.next() {
                    if line.is_empty() {
                        if info_fields.is_empty() { break; }
                        return Err(invalid("malformed informational HTTP header line ending"));
                    }
                    let line = if info_lines.peek().is_some() {
                        line.strip_suffix(b"\r").ok_or_else(||invalid("malformed informational HTTP header line ending"))?
                    } else { line };
                    let colon = line.iter().position(|&b| b == b':').ok_or_else(||invalid("malformed informational HTTP header"))?;
                    let name = &line[..colon];
                    if name.is_empty() || !name.iter().all(|&b| b.is_ascii_alphanumeric() || matches!(b, b'!'|b'#'|b'$'|b'%'|b'&'|b'\''|b'*'|b'+'|b'-'|b'.'|b'^'|b'_'|b'`'|b'|'|b'~')) || line[colon+1..].iter().any(|&b| (b < b' ' && b != b'\t') || b == 0x7f) {
                        return Err(invalid("invalid informational HTTP header"));
                    }
                }
                informational += 1;
                if informational > 8 { return Err(invalid("too many informational HTTP responses")); }
                buffer.drain(..end);
                continue;
            }
            break end;
        }
        match stream.read(&mut chunk) {
            Ok(0) => return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "incomplete HTTP headers")),
            Ok(n) => {
                buffer.extend_from_slice(&chunk[..n]);
                if buffer.len() > MAX_HEADER && !buffer.windows(4).any(|part|part==b"\r\n\r\n") { return Err(invalid("HTTP headers too large")); }
            }
            Err(error) if retryable(&error) => continue,
            Err(error) => return Err(error),
        }
    };
    let mode = match parse_head(&buffer[..header_end])? {
        Head::Redirect(location) => return Ok(Response::Redirect(location)),
        Head::Body(mode) => mode,
    };
    let prefix = buffer[header_end..].to_vec();
    let mut input = Input { stream: &mut *stream, prefix, offset: 0, cancel, deadline };
    if matches!(mode,BodyMode::Chunked) { return Ok(Response::Complete(chunked_body(&mut input)?)); }
    if matches!(mode,BodyMode::UntilClose) { return Ok(Response::Complete(close_delimited_body(&mut input)?)); }
    let BodyMode::Fixed(length) = mode else { unreachable!() };
    if input.prefix.len() > length { return Err(invalid("HTTP body exceeds declared length")); }
    let mut writer = if let Some(slot)=progressive {
        let (writer,reader)=super::network_progress::create(length)?;
        *slot.lock().map_err(|_|io::Error::other("progressive media publication poisoned"))?=Some(reader);
        Some(writer)
    } else {None};
    let result: io::Result<Vec<u8>> = (|| {
        let mut body = Vec::new();
        body.try_reserve_exact(length).map_err(|_| invalid("HTTP body allocation failed"))?;
        let mut remaining=length;
        while remaining>0 {
            let n=remaining.min(chunk.len());input.exact(&mut chunk[..n])?;
            if let Some(writer)=&mut writer {writer.append(&chunk[..n])?;}
            body.extend_from_slice(&chunk[..n]);remaining-=n;
        }
        check(cancel, deadline)?;
        Ok(body)
    })();
    match result {
        Ok(body) => { if let Some(writer)=writer {writer.finish()?;} Ok(Response::Complete(body)) }
        Err(error) => { if let Some(writer)=writer {writer.fail(error.to_string());} Err(error) }
    }
}

fn parse_head(header: &[u8]) -> io::Result<Head> {
    // HTTP field values may contain opaque bytes in the obs-text range. Map
    // bytes one-to-one so an unrelated non-UTF-8 header does not reject an
    // otherwise valid response; framing and redirect location remain ASCII.
    let text: String = header.iter().map(|&byte| char::from(byte)).collect();
    let mut lines = text.split("\r\n");
    let status = lines.next().ok_or_else(|| invalid("missing HTTP status"))?;
    let mut fields = status.split(' ');
    if !matches!(fields.next(), Some("HTTP/1.0" | "HTTP/1.1")) { return Err(invalid("invalid HTTP version")); }
    let status = fields.next().ok_or_else(|| invalid("missing HTTP status"))?;
    if status != "200" && !matches!(status,"301"|"302"|"303"|"307"|"308") { return Err(invalid("HTTP server did not return a supported status")); }
    let mut length = None;
    let mut chunked = false;
    let mut location = None;
    for line in lines.filter(|line| !line.is_empty()) {
        let (name, value) = line.split_once(':').ok_or_else(|| invalid("malformed HTTP header"))?;
        // Field values may contain horizontal tabs and obs-text, but other
        // control bytes cannot appear in an HTTP field value.
        if value.chars().any(|c| (c < ' ' && c != '\t') || c == '\u{7f}') {
            return Err(invalid("invalid HTTP header value"));
        }
        // Field names are HTTP tokens: accepting whitespace before ':' could
        // hide framing headers from the checks below.
        if name.is_empty() || !name.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'!' | b'#' | b'$' | b'%' | b'&' | b'\'' | b'*' | b'+' | b'-' | b'.' | b'^' | b'_' | b'`' | b'|' | b'~')) {
            return Err(invalid("invalid HTTP header name"));
        }
        // Only SP and HTAB are HTTP optional whitespace. Unicode-aware trim
        // would also discard obs-text such as 0x85 and can turn an invalid
        // framing value into a valid Content-Length or Transfer-Encoding.
        let value = value.trim_matches(|c| c == ' ' || c == '\t');
        // Redirect bodies are not consumed. Their encoding and declared size
        // do not describe the media at the destination and must not prevent
        // following a valid Location.
        if status != "200" {
            if name.eq_ignore_ascii_case("location") {
                if location.is_some() || value.is_empty() || value.len()>8192 || !value.is_ascii() { return Err(invalid("invalid HTTP redirect location")); }
                location=Some(value.to_owned());
            }
            continue;
        }
        if name.eq_ignore_ascii_case("content-encoding") && !value.eq_ignore_ascii_case("identity") {
            return Err(invalid("encoded HTTP response is unsupported"));
        }
        if name.eq_ignore_ascii_case("transfer-encoding") {
            if chunked || !value.eq_ignore_ascii_case("chunked") { return Err(invalid("unsupported HTTP transfer encoding")); }
            chunked=true;
        }
        if name.eq_ignore_ascii_case("content-length") {
            // Intermediaries can combine identical Content-Length fields into
            // a comma-separated list. Accept only identical decimal values;
            // different lengths remain ambiguous and must not be used.
            for item in value.split(',') {
                let item = item.trim_matches(|c| c == ' ' || c == '\t');
                if item.is_empty() || !item.bytes().all(|c| c.is_ascii_digit()) { return Err(invalid("invalid HTTP content length")); }
                let size = item.parse::<usize>().map_err(|_| invalid("invalid HTTP content length"))?;
                if size > MAX_BODY || size == 0 { return Err(invalid("HTTP media body exceeds budget")); }
                if length.is_some_and(|previous| previous != size) { return Err(invalid("conflicting HTTP content lengths")); }
                length = Some(size);
            }
        }
    }
    if chunked && length.is_some() { return Err(invalid("ambiguous HTTP body framing")); }
    if status != "200" { return location.map(Head::Redirect).ok_or_else(|| invalid("HTTP redirect location required")); }
    if chunked { return Ok(Head::Body(BodyMode::Chunked)); }
    Ok(Head::Body(length.map_or(BodyMode::UntilClose,BodyMode::Fixed)))
}

struct Input<'a> { stream: &'a mut dyn NetworkStream, prefix: Vec<u8>, offset: usize, cancel: &'a AtomicBool, deadline: Instant }
// HTTP/1.x responses without explicit framing end when the server closes the
// connection. Keep the same body and deadline limits as other finite media;
// this path cannot publish a progressive reader because the size is unknown.
fn close_delimited_body(input: &mut Input<'_>) -> io::Result<Vec<u8>> {
    let mut body=Vec::new();
    let mut chunk=[0u8;8192];
    loop {
        check(input.cancel,input.deadline)?;
        let n=if input.offset<input.prefix.len() {
            let count=chunk.len().min(input.prefix.len()-input.offset);
            chunk[..count].copy_from_slice(&input.prefix[input.offset..input.offset+count]);
            input.offset+=count;count
        } else {
            match input.stream.read(&mut chunk) {
                Ok(0)=>break,
                Ok(count)=>count,
                Err(error) if retryable(&error)=>continue,
                Err(error)=>return Err(error),
            }
        };
        let end=body.len().checked_add(n).filter(|size|*size<=MAX_BODY).ok_or_else(||invalid("HTTP media body exceeds budget"))?;
        body.try_reserve(end-body.len()).map_err(|_|invalid("HTTP body allocation failed"))?;
        body.extend_from_slice(&chunk[..n]);
    }
    if body.is_empty(){return Err(invalid("empty HTTP media body"));}
    Ok(body)
}
impl Input<'_> {
    fn exact(&mut self, mut output: &mut [u8]) -> io::Result<()> {
        while !output.is_empty() {
            check(self.cancel,self.deadline)?;
            if self.offset < self.prefix.len() {
                let n=output.len().min(self.prefix.len()-self.offset);
                output[..n].copy_from_slice(&self.prefix[self.offset..self.offset+n]);
                self.offset+=n;output=&mut output[n..];continue;
            }
            match self.stream.read(output) {
                Ok(0)=>return Err(io::Error::new(io::ErrorKind::UnexpectedEof,"HTTP body ended early")),
                Ok(n)=>output=&mut output[n..],
                Err(error) if retryable(&error)=>continue,
                Err(error)=>return Err(error),
            }
        }
        Ok(())
    }
    fn line(&mut self, budget: &mut usize) -> io::Result<Vec<u8>> {
        let mut line=Vec::new();
        loop {
            if *budget==0 || line.len()>=1024 { return Err(invalid("HTTP chunk metadata exceeds limit")); }
            *budget-=1;
            let mut byte=[0]; self.exact(&mut byte)?;
            if byte[0]==b'\n' {
                if line.pop()!=Some(b'\r') { return Err(invalid("invalid HTTP chunk line")); }
                return Ok(line);
            }
            // Trailer field values may contain opaque obs-text bytes. The
            // chunk-size and trailer parsers validate their own ASCII syntax
            // and reject control bytes after the complete line is read.
            line.push(byte[0]);
        }
    }
}
fn chunked_body(input: &mut Input<'_>) -> io::Result<Vec<u8>> {
    let mut body=Vec::new();let mut metadata=MAX_HEADER;let mut chunks=0usize;
    loop {
        chunks+=1;if chunks>65_536 {return Err(invalid("HTTP chunk count exceeds limit"));}
        let line=input.line(&mut metadata)?;
        // Chunk extensions are metadata, not media bytes. Reject embedded
        // control characters even when the hexadecimal size itself is valid.
        if line.iter().any(|byte| (*byte < 0x20 && *byte != b'\t') || *byte == 0x7f) {
            return Err(invalid("invalid HTTP chunk metadata"));
        }
        // RFC chunk extensions permit optional whitespace before the first
        // semicolon. It belongs to the extension, not the hexadecimal size.
        let mut size=line.split(|byte|*byte==b';').next().unwrap_or_default();
        if line.contains(&b';') {
            while matches!(size.last(),Some(b' '|b'\t')) { size=&size[..size.len()-1]; }
        }
        if size.is_empty()||size.len()>16||!size.iter().all(u8::is_ascii_hexdigit) {return Err(invalid("invalid HTTP chunk size"));}
        let size=usize::from_str_radix(std::str::from_utf8(size).map_err(|_|invalid("invalid HTTP chunk size"))?,16).map_err(|_|invalid("HTTP chunk size overflow"))?;
        if size==0 {
            loop {
                let trailer=input.line(&mut metadata)?;
                if trailer.is_empty(){break;}
                let Some(colon)=trailer.iter().position(|byte|*byte==b':') else{return Err(invalid("invalid HTTP trailer"));};
                let name=&trailer[..colon];
                if name.is_empty() || !name.iter().all(|b|b.is_ascii_alphanumeric() || matches!(*b,b'!'|b'#'|b'$'|b'%'|b'&'|b'\''|b'*'|b'+'|b'-'|b'.'|b'^'|b'_'|b'`'|b'|'|b'~'))
                    || trailer[colon+1..].iter().any(|b|(*b<0x20 && *b!=b'\t')||*b==0x7f) {
                    return Err(invalid("invalid HTTP trailer"));
                }
            }
            if body.is_empty(){return Err(invalid("empty HTTP media body"));}return Ok(body);
        }
        let end=body.len().checked_add(size).filter(|n|*n<=MAX_BODY).ok_or_else(||invalid("HTTP media body exceeds budget"))?;
        if body.capacity()<end {
            let desired=end.max(body.capacity().saturating_mul(2)).max(8192).min(MAX_BODY);
            body.try_reserve_exact(desired-body.len()).map_err(|_|invalid("HTTP body allocation failed"))?;
        }
        let start=body.len();body.resize(end,0);input.exact(&mut body[start..])?;
        let mut terminator=[0;2];input.exact(&mut terminator)?;
        if terminator!=*b"\r\n" {return Err(invalid("invalid HTTP chunk terminator"));}
    }
}

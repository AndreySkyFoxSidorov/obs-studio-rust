//! Bounded, cancellable HTTP acquisition for a finite media object.
//!
//! This transport supports bounded finite HTTP(S) media. It does not implement
//! live streams. Callers must not substitute the
//! resulting bytes for the authored URL in the document.
use std::{io::{self, Read, Write}, net::{IpAddr, SocketAddr, TcpStream, ToSocketAddrs}, sync::{Arc, atomic::{AtomicBool, Ordering}, mpsc::{self, Receiver, TryRecvError}}, thread, time::{Duration, Instant}};

const MAX_HEADER: usize = 16 * 1024;
const MAX_BODY: usize = 32 * 1024 * 1024;
const IO_SLICE: Duration = Duration::from_millis(250);
const TOTAL_TIME: Duration = Duration::from_secs(30);
static ACTIVE: AtomicBool = AtomicBool::new(false);
static DNS_ACTIVE: AtomicBool = AtomicBool::new(false);
struct Permit;
impl Drop for Permit { fn drop(&mut self) { ACTIVE.store(false, Ordering::Release); } }
struct DnsPermit;
impl Drop for DnsPermit { fn drop(&mut self) { DNS_ACTIVE.store(false, Ordering::Release); } }

/// Request target and Host header are independent of the resolved socket.
pub struct Request {
    address: Option<SocketAddr>, hostname: String, port: u16,
    host: String, target: String, secure: bool,
}
pub fn is_url(value: &str) -> bool { value.starts_with("http://") || value.starts_with("https://") }
impl Request {
    pub fn http_url(url: &str) -> io::Result<Self> {
        let (secure,rest) = if let Some(rest) = url.strip_prefix("http://") { (false,rest) }
            else if let Some(rest) = url.strip_prefix("https://") { (true,rest) }
            else { return Err(invalid("only finite HTTP(S) URLs are supported")); };
        let default_port = if secure {443} else {80};
        if rest.contains('#') || rest.contains('@') { return Err(invalid("HTTP fragments and credentials are unsupported")); }
        let split = rest.find(|c| c == '/' || c == '?').unwrap_or(rest.len());
        let authority = &rest[..split];
        let tail = &rest[split..];
        let target = if tail.is_empty() { "/".to_owned() } else if tail.starts_with('?') { format!("/{tail}") } else { tail.to_owned() };
        let (hostname,port,address) = if let Ok(socket) = authority.parse::<SocketAddr>() {
            (socket.ip().to_string(), socket.port(), Some(socket))
        } else if let Ok(ip) = authority.strip_prefix('[').and_then(|v| v.strip_suffix(']')).unwrap_or(authority).parse::<IpAddr>() {
            (ip.to_string(), default_port, Some(SocketAddr::new(ip, default_port)))
        } else {
            let (name, port) = if let Some((name, port)) = authority.rsplit_once(':') {
                (name, port.parse::<u16>().map_err(|_| invalid("invalid HTTP port"))?)
            } else { (authority,default_port) };
            if name.is_empty() || name.len() > 253 || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.') {
                return Err(invalid("invalid HTTP hostname"));
            }
            (name.to_owned(),port,None)
        };
        if port == 0 { return Err(invalid("invalid HTTP port")); }
        let request = Self { address, hostname, port, host: authority.to_owned(), target, secure };
        validate(&request)?;
        Ok(request)
    }
}

pub struct Owner {
    cancelled: Arc<AtomicBool>,
    result: Receiver<io::Result<Vec<u8>>>,
    _permit: Arc<Permit>,
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
        ACTIVE.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| io::Error::new(io::ErrorKind::WouldBlock, "media HTTP worker is busy"))?;
        let permit = Arc::new(Permit);
        let worker_permit = Arc::clone(&permit);
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
        Ok(Self { cancelled, result, _permit: permit, progressive })
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
    DNS_ACTIVE.compare_exchange(false,true,Ordering::AcqRel,Ordering::Acquire)
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
enum BodyMode { Fixed(usize), Chunked }
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
                let next = Request::http_url(next_url.as_str())?;
                if request.secure && !next.secure { return Err(invalid("HTTPS to HTTP redirect is unsupported")); }
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
        match TcpStream::connect_timeout(&address, Duration::from_secs(3)) {
            Ok(socket) => { stream = Some(socket); break; },
            Err(error) => last_error = Some(error),
        }
    }
    let socket = stream.ok_or_else(|| last_error.unwrap_or_else(|| invalid("HTTP host has no connectable addresses")))?;
    socket.set_read_timeout(Some(IO_SLICE))?;
    socket.set_write_timeout(Some(IO_SLICE))?;
    let mut stream: Box<dyn NetworkStream> = if request.secure { tls_stream(socket,&request.hostname,cancel,deadline)? } else { Box::new(socket) };
    let request_bytes = format!("GET {} HTTP/1.1\r\nHost: {}\r\nAccept-Encoding: identity\r\nConnection: close\r\n\r\n", request.target, request.host);
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
    let header_end = loop {
        check(cancel, deadline)?;
        match stream.read(&mut chunk) {
            Ok(0) => return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "incomplete HTTP headers")),
            Ok(n) => {
                buffer.extend_from_slice(&chunk[..n]);
                if let Some(index) = buffer.windows(4).position(|part| part == b"\r\n\r\n") {
                    if index + 4 > MAX_HEADER { return Err(invalid("HTTP headers too large")); }
                    break index + 4;
                }
                if buffer.len() > MAX_HEADER { return Err(invalid("HTTP headers too large")); }
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
    let text = std::str::from_utf8(header).map_err(|_| invalid("non-ASCII HTTP headers"))?;
    if !text.is_ascii() { return Err(invalid("non-ASCII HTTP headers")); }
    let mut lines = text.split("\r\n");
    let status = lines.next().ok_or_else(|| invalid("missing HTTP status"))?;
    let mut fields = status.split_whitespace();
    if !matches!(fields.next(), Some("HTTP/1.0" | "HTTP/1.1")) { return Err(invalid("invalid HTTP version")); }
    let status = fields.next().ok_or_else(|| invalid("missing HTTP status"))?;
    if status != "200" && !matches!(status,"301"|"302"|"303"|"307"|"308") { return Err(invalid("HTTP server did not return a supported status")); }
    let mut length = None;
    let mut chunked = false;
    let mut location = None;
    for line in lines.filter(|line| !line.is_empty()) {
        let (name, value) = line.split_once(':').ok_or_else(|| invalid("malformed HTTP header"))?;
        if name.eq_ignore_ascii_case("content-encoding") && !value.trim().eq_ignore_ascii_case("identity") {
            return Err(invalid("encoded HTTP response is unsupported"));
        }
        if name.eq_ignore_ascii_case("transfer-encoding") {
            if chunked || !value.trim().eq_ignore_ascii_case("chunked") { return Err(invalid("unsupported HTTP transfer encoding")); }
            chunked=true;
        }
        if name.eq_ignore_ascii_case("location") {
            if location.is_some() || value.trim().is_empty() || value.trim().len()>8192 { return Err(invalid("invalid HTTP redirect location")); }
            location=Some(value.trim().to_owned());
        }
        if name.eq_ignore_ascii_case("content-length") {
            if length.is_some() { return Err(invalid("duplicate HTTP content length")); }
            let value = value.trim();
            if value.is_empty() || !value.bytes().all(|c| c.is_ascii_digit()) { return Err(invalid("invalid HTTP content length")); }
            let size = value.parse::<usize>().map_err(|_| invalid("invalid HTTP content length"))?;
            if size > MAX_BODY || (size == 0 && status == "200") { return Err(invalid("HTTP media body exceeds budget")); }
            length = Some(size);
        }
    }
    if chunked && length.is_some() { return Err(invalid("ambiguous HTTP body framing")); }
    if status != "200" { return location.map(Head::Redirect).ok_or_else(|| invalid("HTTP redirect location required")); }
    if chunked { return Ok(Head::Body(BodyMode::Chunked)); }
    length.map(|n|Head::Body(BodyMode::Fixed(n))).ok_or_else(|| invalid("finite HTTP content length required"))
}

struct Input<'a> { stream: &'a mut dyn NetworkStream, prefix: Vec<u8>, offset: usize, cancel: &'a AtomicBool, deadline: Instant }
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
            if !byte[0].is_ascii() { return Err(invalid("invalid HTTP chunk metadata")); }
            line.push(byte[0]);
        }
    }
}
fn chunked_body(input: &mut Input<'_>) -> io::Result<Vec<u8>> {
    let mut body=Vec::new();let mut metadata=MAX_HEADER;let mut chunks=0usize;
    loop {
        chunks+=1;if chunks>65_536 {return Err(invalid("HTTP chunk count exceeds limit"));}
        let line=input.line(&mut metadata)?;
        let size=line.split(|byte|*byte==b';').next().unwrap_or_default();
        if size.is_empty()||size.len()>16||!size.iter().all(u8::is_ascii_hexdigit) {return Err(invalid("invalid HTTP chunk size"));}
        let size=usize::from_str_radix(std::str::from_utf8(size).map_err(|_|invalid("invalid HTTP chunk size"))?,16).map_err(|_|invalid("HTTP chunk size overflow"))?;
        if size==0 {
            loop {let trailer=input.line(&mut metadata)?;if trailer.is_empty(){break;}if !trailer.contains(&b':'){return Err(invalid("invalid HTTP trailer"));}}
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

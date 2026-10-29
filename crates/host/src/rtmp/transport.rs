use super::{
    Error, ResolvedTarget,
    amf::{self, Value},
    chunk::{self, Decoder, Message},
    flv::{self, AudioConfig, EncodedPacket, Generation, VideoConfig},
    io_error,
};
use std::{
    io::{self, Read, Write},
    net::TcpStream,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub max_streams: usize,
    pub max_inbound_message: usize,
    pub max_assembly_bytes: usize,
    pub queue_messages: usize,
    pub queue_bytes: usize,
    pub outbound_chunk_size: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_streams: 64,
            max_inbound_message: 1024 * 1024,
            max_assembly_bytes: 4 * 1024 * 1024,
            queue_messages: 32,
            queue_bytes: 8 * 1024 * 1024,
            outbound_chunk_size: 4096,
        }
    }
}
impl Limits {
    fn validate(self) -> Result<(), Error> {
        if self.max_streams == 0
            || self.max_streams > 64
            || self.max_inbound_message == 0
            || self.max_inbound_message > 1024 * 1024
            || self.max_assembly_bytes < self.max_inbound_message
            || self.max_assembly_bytes > 4 * 1024 * 1024
            || self.queue_messages == 0
            || self.queue_messages > 32
            || self.queue_bytes == 0
            || self.queue_bytes > 8 * 1024 * 1024
            || !(128..=65536).contains(&self.outbound_chunk_size)
        {
            Err(Error::Invalid("RTMP limits outside supported profile"))
        } else {
            Ok(())
        }
    }
}
#[derive(Clone, Copy, Debug)]
pub struct Timeouts {
    pub connect: Duration,
    pub handshake: Duration,
    pub command: Duration,
    pub packet: Duration,
    pub stop: Duration,
}
impl Default for Timeouts {
    fn default() -> Self {
        Self {
            connect: Duration::from_secs(5),
            handshake: Duration::from_secs(5),
            command: Duration::from_secs(5),
            packet: Duration::from_secs(2),
            stop: Duration::from_secs(2),
        }
    }
}
#[derive(Clone, Debug)]
pub struct ConnectOptions {
    pub connection: u64,
    pub target: ResolvedTarget,
    pub video_generation: Generation,
    pub video: VideoConfig,
    pub audio: Option<(Generation, AudioConfig)>,
    pub timestamp_origin_ns: u64,
    pub limits: Limits,
    pub timeouts: Timeouts,
}
impl ConnectOptions {
    pub fn validate(&self) -> Result<(), Error> {
        if self.connection == 0 {
            return Err(Error::Invalid("connection identity must be nonzero"));
        }
        self.limits.validate()?;
        self.video_generation.validate()?;
        if let Some((generation, _)) = &self.audio {
            generation.validate()?;
            if generation.session != self.video_generation.session
                || generation.output != self.video_generation.output
            {
                return Err(Error::StaleGeneration);
            }
        }
        for value in [
            self.timeouts.connect,
            self.timeouts.handshake,
            self.timeouts.command,
            self.timeouts.packet,
            self.timeouts.stop,
        ] {
            if value.is_zero() || value > Duration::from_secs(30) {
                return Err(Error::Invalid(
                    "timeout must be positive and at most30 seconds",
                ));
            }
        }
        Ok(())
    }
    pub(crate) fn accepts(&self, packet: &EncodedPacket) -> bool {
        match packet {
            EncodedPacket::Video(_) => packet.generation() == self.video_generation,
            EncodedPacket::Audio(_) => self
                .audio
                .as_ref()
                .is_some_and(|(generation, _)| packet.generation() == *generation),
        }
    }
}
pub(crate) trait Wire: Read + Write {
    fn set_timeout(&mut self, value: Duration) -> io::Result<()>;
    fn ready(&mut self) -> io::Result<bool>;
    fn finish(&mut self) -> io::Result<()> { self.flush() }
    fn physical_counts(&self) -> Option<(u64, u64)> { None }
}
impl Wire for TcpStream {
    fn set_timeout(&mut self, value: Duration) -> io::Result<()> {
        self.set_read_timeout(Some(value))?;
        self.set_write_timeout(Some(value))
    }
    fn ready(&mut self) -> io::Result<bool> {
        let mut byte = [0];
        match self.peek(&mut byte) {
            Ok(0) => Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "RTMP peer disconnected",
            )),
            Ok(_) => Ok(true),
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                ) =>
            {
                Ok(false)
            }
            Err(e) => Err(e),
        }
    }
}
pub(super) fn budget(deadline: Instant, cancel: &AtomicBool) -> Result<Duration, Error> {
    if cancel.load(Ordering::Acquire) {
        return Err(Error::Cancelled);
    }
    let remaining = deadline
        .checked_duration_since(Instant::now())
        .ok_or(Error::Deadline("operation"))?;
    if remaining.is_zero() {
        return Err(Error::Deadline("operation"));
    }
    Ok(remaining.min(Duration::from_millis(100)))
}
fn read_exact<W: Wire>(
    io: &mut W,
    bytes: &mut [u8],
    deadline: Instant,
    cancel: &AtomicBool,
    received: &mut u64,
) -> Result<(), Error> {
    let mut at = 0;
    while at < bytes.len() {
        io.set_timeout(budget(deadline, cancel)?)
            .map_err(|e| io_error("set socket timeout", e))?;
        match io.read(&mut bytes[at..]) {
            Ok(0) => return Err(Error::ConnectionLost{operation:"RTMP read",detail:"peer disconnected during RTMP read".into()}),
            Ok(n) => {
                at += n;
                *received = received
                    .checked_add(n as u64)
                    .ok_or(Error::Limit("received byte counter"))?;
            }
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::TimedOut
                        | io::ErrorKind::WouldBlock
                        | io::ErrorKind::Interrupted
                ) =>
            {
                std::thread::yield_now()
            }
            Err(e) => return Err(io_error("read RTMP", e)),
        }
    }
    Ok(())
}
pub(crate) struct Client<W: Wire> {
    io: W,
    decoder: Decoder,
    options: ConnectOptions,
    cancel: Arc<AtomicBool>,
    received: u64,
    sent: u64,
    ack_at: u64,
    ack_window: u32,
    peer_ack: u64,
    peer_window: Option<u32>,
    peer_limit: u8,
    stream: u32,
    last_dts: [Option<u64>; 2],
    last_video_ms: u32,
    sent_video: bool,
}
impl<W: Wire> Client<W> {
    pub fn new(io: W, options: ConnectOptions, cancel: Arc<AtomicBool>) -> Result<Self, Error> {
        options.validate()?;
        let l = options.limits;
        Ok(Self {
            io,
            decoder: Decoder::new(l.max_streams, l.max_inbound_message, l.max_assembly_bytes),
            options,
            cancel,
            received: 0,
            sent: 0,
            ack_at: 0,
            ack_window: 0,
            peer_ack: 0,
            peer_window: None,
            peer_limit: 255,
            stream: 0,
            last_dts: [None; 2],
            last_video_ms: 0,
            sent_video: false,
        })
    }
    fn deadline(&self, value: Duration) -> Result<Instant, Error> {
        Instant::now()
            .checked_add(value)
            .ok_or(Error::Limit("deadline overflow"))
    }
    fn write_raw(&mut self, bytes: &[u8], deadline: Instant, flow: bool) -> Result<(), Error> {
        let mut at = 0;
        while at < bytes.len() {
            let timeout = budget(deadline, &self.cancel)?;
            if flow
                && self
                    .peer_window
                    .is_some_and(|window| self.sent - self.peer_ack >= u64::from(window))
            {
                let message = self.message(deadline)?;
                self.control_or_status(message, deadline)?;
                continue;
            }
            let count = if flow {
                self.peer_window
                    .map_or(bytes.len() - at, |window| {
                        (u64::from(window) - (self.sent - self.peer_ack)) as usize
                    })
                    .min(bytes.len() - at)
            } else {
                bytes.len() - at
            };
            self.io
                .set_timeout(timeout)
                .map_err(|e| io_error("set socket timeout", e))?;
            match self.io.write(&bytes[at..at + count]) {
                Ok(0) => return Err(Error::ConnectionLost{operation:"RTMP write",detail:"peer disconnected during RTMP write".into()}),
                Ok(n) => {
                    at += n;
                    self.sent = self
                        .sent
                        .checked_add(n as u64)
                        .ok_or(Error::Limit("sent byte counter"))?;
                    // TLS accepts one plaintext record before its ciphertext
                    // has necessarily reached the socket. Finish that owned
                    // record before waiting for an RTMP acknowledgement.
                    self.flush_wire(deadline, false)?;
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::TimedOut
                            | io::ErrorKind::WouldBlock
                            | io::ErrorKind::Interrupted
                    ) =>
                {
                    std::thread::yield_now()
                }
                Err(e) => return Err(io_error("write RTMP", e)),
            }
        }
        Ok(())
    }
    fn flush_wire(&mut self, deadline: Instant, finishing: bool) -> Result<(), Error> {
        loop {
            self.io.set_timeout(budget(deadline, &self.cancel)?)
                .map_err(|error| io_error("set transport flush timeout", error))?;
            let result = if finishing { self.io.finish() } else { self.io.flush() };
            match result {
                Ok(()) => return Ok(()),
                Err(error) if matches!(error.kind(), io::ErrorKind::WouldBlock
                    | io::ErrorKind::TimedOut | io::ErrorKind::Interrupted) => std::thread::yield_now(),
                Err(error) => return Err(io_error("flush RTMP transport", error)),
            }
        }
    }
    fn send(
        &mut self,
        message: &Message,
        csid: u32,
        deadline: Instant,
        flow: bool,
    ) -> Result<(), Error> {
        let size = self.options.limits.outbound_chunk_size;
        chunk::write(message, csid, size, &mut |bytes| {
            self.write_raw(bytes, deadline, flow)
        })
    }
    fn command(&mut self, values: &[Value], stream: u32, deadline: Instant) -> Result<(), Error> {
        let message = Message {
            timestamp: 0,
            type_id: 20,
            stream,
            data: amf::encode(values)?,
        };
        self.send(&message, 3, deadline, false)
    }
    fn control(&mut self, kind: u8, data: Vec<u8>, deadline: Instant) -> Result<(), Error> {
        self.send(
            &Message {
                timestamp: 0,
                type_id: kind,
                stream: 0,
                data,
            },
            2,
            deadline,
            false,
        )
    }
    pub fn connect(&mut self) -> Result<(), Error> {
        let deadline = self.deadline(self.options.timeouts.handshake)?;
        budget(deadline, &self.cancel)?;
        let hello = super::handshake::Hello::new()?;
        self.write_raw(&[3], deadline, false)?;
        self.write_raw(&hello.packet, deadline, false)?;
        let mut s0 = [0];
        read_exact(
            &mut self.io,
            &mut s0,
            deadline,
            &self.cancel,
            &mut self.received,
        )?;
        if s0[0] != 3 {
            return Err(Error::Unsupported("RTMP handshake version"));
        }
        let mut s1 = [0; 1536];
        read_exact(
            &mut self.io,
            &mut s1,
            deadline,
            &self.cancel,
            &mut self.received,
        )?;
        let reply = hello.reply(&s1)?;
        self.write_raw(&reply.packet, deadline, false)?;
        let mut s2 = [0; 1536];
        read_exact(
            &mut self.io,
            &mut s2,
            deadline,
            &self.cancel,
            &mut self.received,
        )?;
        hello.verify(&reply, &s2)?;
        let deadline = self.deadline(self.options.timeouts.command)?;
        // Peer starts at128 until this exact control has been sent; encode it at
        //128 as well (four bytes fits), then subsequent messages use our size.
        self.control(
            1,
            (self.options.limits.outbound_chunk_size as u32)
                .to_be_bytes()
                .to_vec(),
            deadline,
        )?;
        let t = &self.options.target;
        let mut fields = vec![
            ("app".into(), Value::String(t.app.clone())),
            ("type".into(), Value::String("nonprivate".into())),
            ("tcUrl".into(), Value::String(t.tc_url.clone())),
            (
                "flashVer".into(),
                Value::String("FMLE/3.0 (compatible; SkyLive Rust)".into()),
            ),
            ("fpad".into(), Value::Boolean(false)),
            ("capabilities".into(), Value::Number(15.0)),
            ("audioCodecs".into(), Value::Number(0x0400 as f64)),
            ("videoCodecs".into(), Value::Number(0x0080 as f64)),
            ("videoFunction".into(), Value::Number(1.0)),
            ("objectEncoding".into(), Value::Number(0.0)),
        ];
        if let (Some(username), Some(password)) = (&t.username, &t.password) {
            fields.push(("user".into(), Value::String(username.clone())));
            fields.push(("password".into(), Value::String(password.clone())));
        }
        self.command(
            &[
                Value::String("connect".into()),
                Value::Number(1.0),
                Value::Object(fields),
            ],
            0,
            deadline,
        )?;
        let values = self.wait_result(1.0, deadline)?;
        if amf::property(&values, "code")
            != Some(&Value::String("NetConnection.Connect.Success".into()))
        {
            return Err(rejected("connect response", &values));
        }
        let deadline = self.deadline(self.options.timeouts.command)?;
        self.command(
            &[
                Value::String("createStream".into()),
                Value::Number(2.0),
                Value::Null,
            ],
            0,
            deadline,
        )?;
        let values = self.wait_result(2.0, deadline)?;
        let stream = match values.get(3) {
            Some(Value::Number(n))
                if *n >= 1.0 && *n <= f64::from(u32::MAX) && n.fract() == 0.0 =>
            {
                *n as u32
            }
            _ => return Err(Error::Protocol("invalid createStream stream ID")),
        };
        self.stream = stream;
        let name = self.options.target.stream_name.clone();
        self.command(
            &[
                Value::String("publish".into()),
                Value::Number(0.0),
                Value::Null,
                Value::String(name),
                Value::String("live".into()),
            ],
            stream,
            deadline,
        )?;
        self.wait_publish(deadline)?;
        self.send(
            &flv::video_header(&self.options.video, stream),
            6,
            deadline,
            true,
        )?;
        if let Some((_, config)) = &self.options.audio {
            let message = flv::audio_sequence(config, stream);
            self.send(&message, 4, deadline, true)?;
        }
        Ok(())
    }
    fn message(&mut self, deadline: Instant) -> Result<Message, Error> {
        let initial = self.received;
        loop {
            if self.received - initial > 8 * 1024 * 1024 {
                return Err(Error::Limit("per-operation inbound control work"));
            }
            let io = &mut self.io;
            let cancel = &self.cancel;
            let received = &mut self.received;
            let message = self
                .decoder
                .read(&mut |bytes| read_exact(io, bytes, deadline, cancel, received))?;
            if self.ack_window > 0 && self.received - self.ack_at >= u64::from(self.ack_window) {
                let count = (self.received as u32).to_be_bytes().to_vec();
                self.control(3, count, deadline)?;
                self.ack_at = self.received;
            }
            if let Some(message) = message {
                return Ok(message);
            }
        }
    }
    fn control_or_status(
        &mut self,
        message: Message,
        deadline: Instant,
    ) -> Result<Option<Vec<Value>>, Error> {
        if (1..=6).contains(&message.type_id) && message.stream != 0 {
            return Err(Error::Protocol("control message stream ID must be0"));
        }
        let data = &message.data;
        match message.type_id {
            1 => {
                if data.len() != 4 || data[0] & 128 != 0 {
                    return Err(Error::Protocol("SetChunkSize payload"));
                }
                self.decoder
                    .set_size(u32::from_be_bytes(data[..4].try_into().unwrap()))?;
            }
            2 => {
                if data.len() != 4 {
                    return Err(Error::Protocol("Abort payload"));
                }
                self.decoder
                    .abort(u32::from_be_bytes(data[..4].try_into().unwrap()));
            }
            3 => {
                if data.len() != 4 {
                    return Err(Error::Protocol("Acknowledgement payload"));
                }
                let seq = u32::from_be_bytes(data[..4].try_into().unwrap());
                let mut next = (self.peer_ack & !0xffff_ffff) | u64::from(seq);
                if next < self.peer_ack {
                    next = next
                        .checked_add(1u64 << 32)
                        .ok_or(Error::Limit("acknowledgement wrap"))?;
                }
                if next > self.sent {
                    return Err(Error::Protocol("peer acknowledged bytes not sent"));
                }
                self.peer_ack = next;
            }
            4 => {
                if data.len() < 2 {
                    return Err(Error::Protocol("UserControl payload"));
                }
                let event = u16::from_be_bytes(data[..2].try_into().unwrap());
                match event {
                    6 => {
                        if data.len() != 6 {
                            return Err(Error::Protocol("PingRequest payload"));
                        }
                        let mut pong = 7u16.to_be_bytes().to_vec();
                        pong.extend_from_slice(&data[2..]);
                        self.control(4, pong, deadline)?;
                    }
                    0 | 1 | 2 | 4 | 7 => {
                        if data.len() != 6 {
                            return Err(Error::Protocol("stream control payload"));
                        }
                    }
                    3 => {
                        if data.len() != 10 {
                            return Err(Error::Protocol("SetBufferLength payload"));
                        }
                    }
                    _ => return Err(Error::Unsupported("user control event")),
                }
            }
            5 => {
                if data.len() != 4 {
                    return Err(Error::Protocol("WindowAcknowledgementSize payload"));
                }
                let window = u32::from_be_bytes(data[..4].try_into().unwrap());
                if window == 0 {
                    return Err(Error::Protocol("zero acknowledgement window"));
                }
                self.ack_window = window;
            }
            6 => {
                if data.len() != 5 || data[4] > 2 {
                    return Err(Error::Protocol("SetPeerBandwidth payload"));
                }
                let value = u32::from_be_bytes(data[..4].try_into().unwrap());
                if value == 0 {
                    return Err(Error::Protocol("zero peer bandwidth"));
                }
                match data[4] {
                    0 => {
                        self.peer_window = Some(value);
                        self.peer_limit = 0;
                    }
                    1 => {
                        self.peer_window =
                            Some(self.peer_window.map_or(value, |old| old.min(value)));
                        self.peer_limit = 1;
                    }
                    2 => {
                        if self.peer_limit == 0 {
                            self.peer_window = Some(value);
                        }
                    }
                    _ => unreachable!(),
                };
                self.control(5, value.to_be_bytes().to_vec(), deadline)?;
            }
            20 => {
                let values = amf::decode(data)?;
                if values.first() == Some(&Value::String("_error".into()))
                    || amf::property(&values, "level") == Some(&Value::String("error".into()))
                {
                    return Err(rejected("server command", &values));
                }
                return Ok(Some(values));
            }
            17 => return Err(Error::Unsupported("AMF3 command")),
            _ => {
                return Err(Error::Unsupported(
                    "server message outside publisher control profile",
                ));
            }
        }
        Ok(None)
    }
    fn wait_result(&mut self, transaction: f64, deadline: Instant) -> Result<Vec<Value>, Error> {
        let initial = self.received;
        for _ in 0..256 {
            if self.received - initial > 8 * 1024 * 1024 {
                return Err(Error::Limit("command inbound byte work"));
            }
            let message = self.message(deadline)?;
            if let Some(values) = self.control_or_status(message, deadline)? {
                if values.first() == Some(&Value::String("_result".into()))
                    && values.get(1) == Some(&Value::Number(transaction))
                {
                    return Ok(values);
                }
            }
        }
        Err(Error::Limit("command response count"))
    }
    fn wait_publish(&mut self, deadline: Instant) -> Result<(), Error> {
        let initial = self.received;
        for _ in 0..256 {
            if self.received - initial > 8 * 1024 * 1024 {
                return Err(Error::Limit("publish inbound byte work"));
            }
            let message = self.message(deadline)?;
            let message_stream = message.stream;
            if let Some(values) = self.control_or_status(message, deadline)? {
                if values.first() == Some(&Value::String("onStatus".into())) {
                    let code = amf::property(&values, "code");
                    if code == Some(&Value::String("NetStream.Publish.Start".into())) {
                        if message_stream != self.stream {
                            return Err(Error::Protocol("publish status for wrong stream"));
                        }
                        return Ok(());
                    }
                    if publish_status_is_failure(&values) {
                        return Err(rejected("publish status", &values));
                    }
                    // OBS/librtmp accepts other onStatus notifications while
                    // awaiting Publish.Start. A status-level informational
                    // message is not evidence that the publish was rejected.
                }
            }
        }
        Err(Error::Limit("publish response count"))
    }
    pub fn packet(&mut self, packet: &EncodedPacket) -> Result<(), Error> {
        if !self.options.accepts(packet) {
            return Err(Error::StaleGeneration);
        }
        // CSID6 video and CSID4 audio have independent timestamp histories.
        // OBS librtmp RTMP_SendPacket uses m_vecChannelsOut[channel]. Upper
        // output mux interleaving is a distinct policy, not this transport gate.
        let track = usize::from(matches!(packet, EncodedPacket::Audio(_)));
        if self.last_dts[track].is_some_and(|old| packet.dts_ns() < old) {
            return Err(Error::Invalid(
                "packet DTS moved backwards on the same track",
            ));
        }
        if matches!(packet, EncodedPacket::Video(_))
            && !self.sent_video
            && !matches!(packet,EncodedPacket::Video(p)if p.keyframe())
        {
            return Err(Error::Invalid("first video packet must be an IDR"));
        }
        let message = packet.message(
            self.options.timestamp_origin_ns,
            self.stream,
            self.options.audio.as_ref().map(|(_, config)| config),
        )?;
        let deadline = self.deadline(self.options.timeouts.packet)?;
        let video = matches!(packet, EncodedPacket::Video(_));
        self.send(&message, if video { 6 } else { 4 }, deadline, true)?;
        self.last_dts[track] = Some(packet.dts_ns());
        if video {
            self.sent_video = true;
            self.last_video_ms = message.timestamp;
        }
        Ok(())
    }
    pub fn poll_control(&mut self) -> Result<(), Error> {
        self.io
            .set_timeout(Duration::from_millis(1))
            .map_err(|e| io_error("poll socket timeout", e))?;
        if self.io.ready().map_err(|e| io_error("poll RTMP", e))? {
            let deadline = self.deadline(self.options.timeouts.packet)?;
            let message = self.message(deadline)?;
            if let Some(values) = self.control_or_status(message, deadline)? {
                if values.first() == Some(&Value::String("onStatus".into()))
                    && publish_status_is_failure(&values) {
                    return Err(rejected("active publisher status", &values));
                }
            }
        }
        Ok(())
    }
    #[cfg(test)]
    pub fn into_wire(self) -> W {
        self.io
    }
    pub fn wire_counts(&self) -> (u64, u64) {
        // RTMP acknowledgement windows still use plaintext counters internally.
        // Statistics use encrypted socket bytes when the TLS owner has them.
        self.io.physical_counts().unwrap_or((self.sent, self.received))
    }
    pub fn stop(&mut self) -> Result<(), Error> {
        let deadline = self.deadline(self.options.timeouts.stop)?;
        if self.sent_video {
            self.send(
                &flv::video_end(self.stream, self.last_video_ms),
                6,
                deadline,
                true,
            )?;
        }
        let name = self.options.target.stream_name.clone();
        self.command(
            &[
                Value::String("FCUnpublish".into()),
                Value::Number(0.0),
                Value::Null,
                Value::String(name),
            ],
            0,
            deadline,
        )?;
        self.command(
            &[
                Value::String("deleteStream".into()),
                Value::Number(0.0),
                Value::Null,
                Value::Number(f64::from(self.stream)),
            ],
            0,
            deadline,
        )?;
        self.flush_wire(deadline, true)
    }
}

// OBS librtmp treats these explicit failure statuses as failures even when a
// server incorrectly labels them as a non-error level. Other onStatus values
// may be informational and must not tear down an otherwise live publisher.
fn publish_status_is_failure(values: &[Value]) -> bool {
    matches!(amf::property(values, "code"), Some(Value::String(code)) if matches!(code.as_str(),
        "NetStream.Failed" | "NetStream.Play.Failed" | "NetStream.Play.StreamNotFound"
        | "NetConnection.Connect.InvalidApp" | "NetStream.Publish.Rejected"
        | "NetStream.Publish.Denied" | "NetStream.Publish.BadName"
        | "NetStream.Publish.Stop" | "NetStream.Unpublish.Success"))
}

fn rejected(phase: &str, values: &[Value]) -> Error {
    Error::Rejected(format!(
        "{phase}: code={:?}, description={:?}",
        amf::property(values, "code"),
        amf::property(values, "description")
    ))
}

#[cfg(test)]
mod control_tests {
    use super::*;
    impl Wire for std::io::Cursor<Vec<u8>> {
        fn set_timeout(&mut self, _: Duration) -> io::Result<()> {
            Ok(())
        }
        fn ready(&mut self) -> io::Result<bool> {
            Ok(false)
        }
    }
    #[test]
    fn hard_soft_dynamic_bandwidth_and_acknowledgements_follow_literal_rules() {
        let mut client = Client::new(
            std::io::Cursor::new(Vec::new()),
            super::super::tests::options("127.0.0.1:1935".parse().unwrap()),
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        for (value, kind, expected) in [
            (100, 2, None),
            (100, 0, Some(100)),
            (200, 2, Some(200)),
            (300, 2, Some(300)),
            (150, 1, Some(150)),
            (999, 2, Some(150)),
            (500, 1, Some(150)),
        ] {
            let mut data = (value as u32).to_be_bytes().to_vec();
            data.push(kind);
            client
                .control_or_status(
                    Message {
                        timestamp: 0,
                        type_id: 6,
                        stream: 0,
                        data,
                    },
                    deadline,
                )
                .unwrap();
            assert_eq!(client.peer_window, expected);
        }
        let ack = client.sent as u32;
        client
            .control_or_status(
                Message {
                    timestamp: 0,
                    type_id: 3,
                    stream: 0,
                    data: ack.to_be_bytes().to_vec(),
                },
                deadline,
            )
            .unwrap();
        assert_eq!(client.peer_ack, client.sent);
        assert!(
            client
                .control_or_status(
                    Message {
                        timestamp: 0,
                        type_id: 3,
                        stream: 0,
                        data: (ack + 1).to_be_bytes().to_vec()
                    },
                    deadline
                )
                .is_err()
        );
        assert!(
            client
                .control_or_status(
                    Message {
                        timestamp: 0,
                        type_id: 1,
                        stream: 8,
                        data: 128u32.to_be_bytes().to_vec()
                    },
                    deadline
                )
                .is_err()
        );
    }
    #[test]
    fn cancellation_and_expired_deadline_precede_any_socket_io() {
        let cancel = AtomicBool::new(false);
        assert!(matches!(
            budget(Instant::now(), &cancel),
            Err(Error::Deadline(_))
        ));
        cancel.store(true, Ordering::Release);
        assert_eq!(
            budget(Instant::now() + Duration::from_secs(1), &cancel),
            Err(Error::Cancelled)
        );
    }
}

//! TEST ONLY. Install as rtmp::streaming_test_support, so the bounded existing
//! AMF/chunk implementation is reused; never a production listener.
use super::{
    amf::{self, Value},
    chunk::{self, Decoder, Message},
    io_error,
};
use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    thread,
    time::{Duration, Instant},
};
#[derive(Debug)]
pub(crate) struct Tag {
    pub timestamp: u32,
    pub type_id: u8,
    pub data: Vec<u8>,
}
/// TEST ONLY: defaults preserve the original native unit-test admission.
#[derive(Clone, Debug)]
pub(crate) struct LoopbackConfig {
    pub expected_key: String,
    pub max_tags: usize,
    pub max_payload_bytes: usize,
    pub max_messages: usize,
    pub overall: Duration,
    pub io_timeout: Duration,
}
impl Default for LoopbackConfig {
    fn default() -> Self {
        Self {
            expected_key: "literal/key?token=abc".into(),
            max_tags: 128,
            max_payload_bytes: 16 * 1024 * 1024,
            max_messages: 256,
            overall: Duration::from_secs(25),
            io_timeout: Duration::from_secs(5),
        }
    }
}
impl LoopbackConfig {
    fn validate(&self) {
        assert!(!self.expected_key.is_empty() && self.expected_key.len() <= 4096);
        assert!((1..=2048).contains(&self.max_tags));
        assert!((1..=32 * 1024 * 1024).contains(&self.max_payload_bytes));
        assert!((1..=8192).contains(&self.max_messages));
        assert!(!self.overall.is_zero() && self.overall <= Duration::from_secs(90));
        assert!(!self.io_timeout.is_zero() && self.io_timeout <= Duration::from_secs(5));
    }
}
pub(crate) struct Received {
    pub tags: Vec<Tag>,
    pub unpublished: bool,
    pub deleted: bool,
}
pub(crate) struct Loopback {
    pub address: SocketAddr,
    handle: thread::JoinHandle<Received>,
}
fn io_deadline(deadline: Instant, timeout: Duration) -> std::io::Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|remaining| !remaining.is_zero())
        .map(|remaining| remaining.min(timeout))
        .ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::TimedOut, "loopback overall deadline")
        })
}
fn read_until(
    s: &mut TcpStream,
    mut bytes: &mut [u8],
    deadline: Instant,
    timeout: Duration,
) -> std::io::Result<()> {
    while !bytes.is_empty() {
        s.set_read_timeout(Some(io_deadline(deadline, timeout)?))?;
        match s.read(bytes) {
            Ok(0) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "loopback input ended",
                ));
            }
            Ok(n) => {
                bytes = &mut bytes[n..];
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}
fn write_until(
    s: &mut TcpStream,
    mut bytes: &[u8],
    deadline: Instant,
    timeout: Duration,
) -> std::io::Result<()> {
    while !bytes.is_empty() {
        s.set_write_timeout(Some(io_deadline(deadline, timeout)?))?;
        match s.write(bytes) {
            Ok(0) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::WriteZero,
                    "loopback output ended",
                ));
            }
            Ok(n) => {
                bytes = &bytes[n..];
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}
fn receive(s: &mut TcpStream, d: &mut Decoder, deadline: Instant, timeout: Duration) -> Message {
    loop {
        if let Some(m) = d
            .read(&mut |b| {
                read_until(s, b, deadline, timeout).map_err(|e| io_error("loopback read", e))
            })
            .unwrap()
        {
            return m;
        }
    }
}
fn reply(s: &mut TcpStream, stream: u32, values: &[Value], deadline: Instant, timeout: Duration) {
    let m = Message {
        timestamp: 0,
        type_id: 20,
        stream,
        data: amf::encode(values).unwrap(),
    };
    chunk::write(&m, 3, 128, &mut |b| {
        write_until(s, b, deadline, timeout).map_err(|e| io_error("loopback reply", e))
    })
    .unwrap();
}
fn success(transaction: f64, value: Value) -> Vec<Value> {
    vec![
        Value::String("_result".into()),
        Value::Number(transaction),
        Value::Null,
        value,
    ]
}
impl Loopback {
    pub(crate) fn bind() -> Self {
        Self::bind_with(LoopbackConfig::default())
    }
    pub(crate) fn bind_with(config: LoopbackConfig) -> Self {
        config.validate();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let handle = thread::spawn(move || {
            let deadline = Instant::now() + config.overall;
            let (mut socket, peer) = loop {
                match listener.accept() {
                    Ok(pair) => break pair,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline, "loopback accept deadline");
                        thread::sleep(Duration::from_millis(1));
                    }
                    Err(e) => panic!("{e}"),
                }
            };
            assert!(peer.ip().is_loopback());
            socket.set_nonblocking(false).unwrap();
            socket.set_read_timeout(Some(config.io_timeout)).unwrap();
            socket.set_write_timeout(Some(config.io_timeout)).unwrap();
            let mut c0c1 = [0; 1537];
            read_until(&mut socket, &mut c0c1, deadline, config.io_timeout).unwrap();
            assert_eq!(c0c1[0], 3);
            let mut s1 = [0; 1536];
            s1[8..].fill(71);
            write_until(&mut socket, &[3], deadline, config.io_timeout).unwrap();
            write_until(&mut socket, &s1, deadline, config.io_timeout).unwrap();
            let mut c2 = [0; 1536];
            read_until(&mut socket, &mut c2, deadline, config.io_timeout).unwrap();
            assert_eq!(c2, s1);
            write_until(&mut socket, &c0c1[1..], deadline, config.io_timeout).unwrap();
            let mut decoder = Decoder::new(64, 2 * 1024 * 1024 + 5, 4 * 1024 * 1024);
            let mut tags = vec![];
            let mut bytes = 0usize;
            let mut publishing = false;
            let mut deleted = false;
            let mut unpublished = false;
            for _ in 0..config.max_messages {
                assert!(Instant::now() < deadline, "loopback overall deadline");
                let message = receive(&mut socket, &mut decoder, deadline, config.io_timeout);
                match message.type_id {
                    1 => decoder
                        .set_size(u32::from_be_bytes(message.data[..4].try_into().unwrap()))
                        .unwrap(),
                    20 => {
                        let v = amf::decode(&message.data).unwrap();
                        match v.first() {
                            Some(Value::String(name)) if name == "connect" => reply(
                                &mut socket,
                                0,
                                &success(
                                    1.,
                                    Value::Object(vec![(
                                        "code".into(),
                                        Value::String("NetConnection.Connect.Success".into()),
                                    )]),
                                ),
                                deadline,
                                config.io_timeout,
                            ),
                            Some(Value::String(name)) if name == "createStream" => reply(
                                &mut socket,
                                0,
                                &success(2., Value::Number(7.)),
                                deadline,
                                config.io_timeout,
                            ),
                            Some(Value::String(name)) if name == "publish" => {
                                assert_eq!(message.stream, 7);
                                assert_eq!(
                                    v.get(3),
                                    Some(&Value::String(config.expected_key.clone()))
                                );
                                publishing = true;
                                reply(
                                    &mut socket,
                                    7,
                                    &[
                                        Value::String("onStatus".into()),
                                        Value::Number(0.),
                                        Value::Null,
                                        Value::Object(vec![
                                            ("level".into(), Value::String("status".into())),
                                            (
                                                "code".into(),
                                                Value::String("NetStream.Publish.Start".into()),
                                            ),
                                        ]),
                                    ],
                                    deadline,
                                    config.io_timeout,
                                );
                            }
                            Some(Value::String(name)) if name == "FCUnpublish" => {
                                assert!(publishing);
                                assert_eq!(
                                    v.get(3),
                                    Some(&Value::String(config.expected_key.clone()))
                                );
                                unpublished = true;
                            }
                            Some(Value::String(name)) if name == "deleteStream" => {
                                deleted = true;
                                break;
                            }
                            _ => panic!("unexpected loopback command: {v:?}"),
                        }
                    }
                    8 | 9 => {
                        assert!(publishing);
                        assert_eq!(message.stream, 7);
                        assert!(tags.len() < config.max_tags);
                        bytes = bytes.checked_add(message.data.len()).unwrap();
                        assert!(bytes <= config.max_payload_bytes);
                        tags.push(Tag {
                            timestamp: message.timestamp,
                            type_id: message.type_id,
                            data: message.data,
                        });
                    }
                    3..=6 => {}
                    _ => panic!("unexpected loopback RTMP type {}", message.type_id),
                }
            }
            assert!(deleted, "actual EOS/delete acknowledgement before finish");
            Received {
                tags,
                unpublished,
                deleted,
            }
        });
        Self { address, handle }
    }
    pub(crate) fn finish(self) -> Vec<Tag> {
        let received = self.finish_report();
        assert!(
            received.unpublished && received.deleted,
            "actual unpublish and delete before successful finish"
        );
        received.tags
    }
    pub(crate) fn finish_report(self) -> Received {
        self.handle
            .join()
            .unwrap_or_else(|payload| std::panic::resume_unwind(payload))
    }
}

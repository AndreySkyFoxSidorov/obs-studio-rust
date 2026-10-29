use super::*;
use super::{
    amf::Value,
    chunk::{Decoder, Message},
    transport::{Client, Wire},
};
use std::{
    io::{self, Read, Write},
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};
pub(super) fn generation() -> Generation {
    Generation {
        session: 1,
        output: 2,
        encoder: 3,
    }
}
pub(super) fn video() -> VideoConfig {
    VideoConfig::new(Arc::from([
        1, 66, 0, 30, 255, 225, 0, 4, 0x67, 66, 0, 30, 1, 0, 2, 0x68, 0xce,
    ]))
    .unwrap()
}
pub(super) fn audio() -> AudioConfig {
    AudioConfig::new(Arc::from([0x11, 0x90])).unwrap()
}
pub(super) fn options(address: std::net::SocketAddr) -> ConnectOptions {
    ConnectOptions {
        connection: 1,
        target: ResolvedTarget::new(
            address,
            "live".into(),
            format!("rtmp://{address}/live"),
            "literal/key?token=abc".into(),
            None,
            None,
        )
        .unwrap(),
        video_generation: generation(),
        video: video(),
        audio: Some((
            Generation {
                encoder: 4,
                ..generation()
            },
            audio(),
        )),
        timestamp_origin_ns: 1_000_000_000,
        limits: Default::default(),
        timeouts: Default::default(),
    }
}
fn wire(message: &Message, size: usize) -> Vec<u8> {
    let mut out = Vec::new();
    chunk::write(message, 3, size, &mut |b| {
        out.extend_from_slice(b);
        Ok(())
    })
    .unwrap();
    out
}
fn command(values: &[Value], stream: u32) -> Vec<u8> {
    wire(
        &Message {
            timestamp: 0,
            type_id: 20,
            stream,
            data: amf::encode(values).unwrap(),
        },
        128,
    )
}
pub(super) fn success(transaction: f64, value: Value) -> Vec<Value> {
    vec![
        Value::String("_result".into()),
        Value::Number(transaction),
        Value::Null,
        value,
    ]
}
pub(super) fn publish_success() -> Vec<Value> {
    vec![
        Value::String("onStatus".into()),
        Value::Number(0.0),
        Value::Null,
        Value::Object(vec![
            ("level".into(), Value::String("status".into())),
            (
                "code".into(),
                Value::String("NetStream.Publish.Start".into()),
            ),
        ]),
    ]
}
fn handshake() -> Vec<u8> {
    let mut out = vec![3];
    let mut s1 = [0; 1536];
    s1[..4].copy_from_slice(&42u32.to_be_bytes());
    s1[8..].fill(71);
    out.extend_from_slice(&s1);
    let mut s2 = [0; 1536];
    for (i, b) in s2[8..].iter_mut().enumerate() {
        *b = (i * 37 + 11) as u8;
    }
    out.extend_from_slice(&s2);
    out
}
fn responses() -> Vec<u8> {
    let mut out = handshake();
    out.extend(command(
        &success(
            1.0,
            Value::Object(vec![(
                "code".into(),
                Value::String("NetConnection.Connect.Success".into()),
            )]),
        ),
        0,
    ));
    out.extend(command(&success(2.0, Value::Number(7.0)), 0));
    out.extend(command(&publish_success(), 7));
    out
}
#[derive(Default)]
struct Fake {
    input: Vec<u8>,
    at: usize,
    output: Vec<u8>,
    fragment: usize,
}
impl Read for Fake {
    fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
        let count = b
            .len()
            .min(self.input.len() - self.at)
            .min(self.fragment.max(1));
        b[..count].copy_from_slice(&self.input[self.at..self.at + count]);
        self.at += count;
        Ok(count)
    }
}
impl Write for Fake {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        let count = b.len().min(self.fragment.max(1));
        self.output.extend_from_slice(&b[..count]);
        Ok(count)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
impl Wire for Fake {
    fn set_timeout(&mut self, _: Duration) -> io::Result<()> {
        Ok(())
    }
    fn ready(&mut self) -> io::Result<bool> {
        Ok(self.at < self.input.len())
    }
}
#[test]
fn amf_literals_bounds_duplicate_keys_and_number_endianness() {
    let literal = [2, 0, 1, b'x', 0, 0x3f, 0xf0, 0, 0, 0, 0, 0, 0, 5, 1, 1];
    let values = vec![
        Value::String("x".into()),
        Value::Number(1.0),
        Value::Null,
        Value::Boolean(true),
    ];
    assert_eq!(amf::decode(&literal).unwrap(), values);
    assert_eq!(amf::encode(&values).unwrap(), literal);
    assert!(amf::decode(&[3, 0, 1, b'a', 5, 0, 1, b'a', 5, 0, 0, 9]).is_err());
    assert!(amf::decode(&[2, 0xff, 0xff]).is_err());
    assert!(amf::decode(&[0, 0x7f, 0xf8, 0, 0, 0, 0, 0, 0]).is_err());
    assert!(amf::encode(&[Value::String("x".repeat(16385))]).is_err());
    let mut deep = Value::Null;
    for _ in 0..10 {
        deep = Value::Array(vec![deep]);
    }
    assert!(amf::encode(&[deep]).is_err());
}
#[test]
fn chunk_literal_fragmentation_interleave_extended_and_compressed_headers() {
    // csid3 type0, ts1, length3, type20, stream1 little-endian; one2-byte
    // first chunk, unrelated complete csid4 chunk, then csid3 continuation.
    let bytes = [
        3, 0, 0, 1, 0, 0, 3, 20, 1, 0, 0, 0, b'a', b'b', 4, 0, 0, 9, 0, 0, 1, 8, 2, 0, 0, 0, b'x',
        0xc3, b'c',
    ];
    let mut at = 0;
    let mut decoder = Decoder::new(64, 1024, 4096);
    decoder.set_size(2).unwrap();
    let mut read = |out: &mut [u8]| {
        let end = at + out.len();
        out.copy_from_slice(bytes.get(at..end).ok_or(Error::Protocol("test EOF"))?);
        at = end;
        Ok(())
    };
    assert!(decoder.read(&mut read).unwrap().is_none());
    let unrelated = decoder.read(&mut read).unwrap().unwrap();
    assert_eq!(
        (unrelated.stream, unrelated.timestamp, unrelated.data),
        (2, 9, vec![b'x'])
    );
    let original = decoder.read(&mut read).unwrap().unwrap();
    assert_eq!(original.data, b"abc");
    assert_eq!(original.timestamp, 1);
    let message = Message {
        timestamp: 0x01000003,
        type_id: 9,
        stream: 0x01020304,
        data: vec![1, 2, 3, 4, 5],
    };
    let mut encoded = Vec::new();
    chunk::write(&message, 65599, 2, &mut |b| {
        encoded.extend_from_slice(b);
        Ok(())
    })
    .unwrap();
    assert_eq!(&encoded[..3], &[1, 255, 255]);
    let mut input = Fake {
        input: encoded,
        fragment: 1,
        ..Default::default()
    };
    let mut decoder = Decoder::new(64, 1024, 4096);
    decoder.set_size(2).unwrap();
    let actual = loop {
        if let Some(m) = decoder
            .read(&mut |b| input.read_exact(b).map_err(|e| io_error("fake", e)))
            .unwrap()
        {
            break m;
        }
    };
    assert_eq!(
        (actual.timestamp, actual.stream, actual.data),
        (message.timestamp, message.stream, message.data)
    );
    // Type1delta2/len1/type8, then type2delta3, then type3newmessage reuse3.
    let bytes = [
        3, 0, 0, 10, 0, 0, 1, 8, 1, 0, 0, 0, 1, 0x43, 0, 0, 2, 0, 0, 1, 8, 2, 0x83, 0, 0, 3, 3,
        0xc3, 4,
    ];
    let mut cursor = io::Cursor::new(bytes);
    let mut decoder = Decoder::new(4, 64, 256);
    let mut times = Vec::new();
    for _ in 0..4 {
        let m = decoder
            .read(&mut |b| cursor.read_exact(b).map_err(|e| io_error("literal", e)))
            .unwrap()
            .unwrap();
        times.push(m.timestamp);
    }
    assert_eq!(times, [10, 12, 15, 18]);
}
#[test]
fn malformed_chunks_never_allocate_peer_declared_unbounded_messages() {
    let mut decoder = Decoder::new(1, 8, 8);
    let mut cursor = io::Cursor::new([3, 0, 0, 0, 0xff, 0xff, 0xff, 20, 0, 0, 0, 0]);
    assert!(matches!(
        decoder.read(&mut |b| cursor.read_exact(b).map_err(|e| io_error("test", e))),
        Err(Error::Limit(_))
    ));
    assert!(decoder.set_size(0).is_err());
    assert!(decoder.set_size(65537).is_err());
    let mut decoder = Decoder::new(1, 8, 8);
    let mut cursor = io::Cursor::new([0xc3]);
    assert!(
        decoder
            .read(&mut |b| cursor.read_exact(b).map_err(|e| io_error("test", e)))
            .is_err()
    );
}
#[test]
fn avc_aac_sequence_raw_tags_and_signed_composition_use_one_explicit_time_origin() {
    let config = audio();
    assert_eq!((config.sample_rate(), config.channels()), (48000, 2));
    let header = flv::audio_sequence(&config, 1);
    assert_eq!(header.data, [0xaf, 0, 0x11, 0x90]);
    let header = flv::video_header(&video(), 1);
    assert_eq!(&header.data[..5], &[0x17, 0, 0, 0, 0]);
    let p = EncodedPacket::Video(
        VideoPacket::new(
            generation(),
            1_040_999_999,
            1_000_000_000,
            Arc::from([0, 0, 0, 2, 0x65, 0x80]),
        )
        .unwrap(),
    );
    let m = p.message(1_000_000_000, 1, Some(&config)).unwrap();
    assert_eq!(m.timestamp, 40);
    assert_eq!(&m.data[..5], &[0x17, 1, 0xff, 0xff, 0xd8]);
    assert!(p.message(2_000_000_000, 1, Some(&config)).is_err());
    let negative = EncodedPacket::Video(
        VideoPacket::new(
            generation(),
            1_000_000_000,
            999_999_999,
            Arc::from([0, 0, 0, 1, 0x65]),
        )
        .unwrap(),
    );
    assert_eq!(
        &negative.message(1_000_000_000, 1, None).unwrap().data[2..5],
        &[255, 255, 255]
    );
    let a = EncodedPacket::Audio(
        AudioPacket::new(generation(), 1_023_999_999, Arc::from([0x21, 0x10])).unwrap(),
    );
    let m = a.message(1_000_000_000, 1, Some(&config)).unwrap();
    assert_eq!((m.timestamp, m.data), (23, vec![0xaf, 1, 0x21, 0x10]));
    assert!(a.message(0, 1, None).is_err());
    assert!(AudioConfig::new(Arc::from([0x2b, 0x92])).is_err());
    assert!(VideoPacket::new(generation(), 0, 0, Arc::from([0, 0, 0, 3, 0x65])).is_err());
}
#[test]
fn complete_fake_publish_preserves_credentials_and_requires_idr_generation_and_order() {
    let mut opts = options("127.0.0.1:1935".parse().unwrap());
    opts.target = ResolvedTarget::new(
        opts.target.address(),
        "live/exact".into(),
        "rtmp://127.0.0.1/live?token=raw".into(),
        "stream/key?x=exact".into(),
        Some("User exact".into()),
        Some("Password exact".into()),
    )
    .unwrap();
    let fake = Fake {
        input: responses(),
        fragment: 1,
        ..Default::default()
    };
    let mut client = Client::new(fake, opts, Arc::new(AtomicBool::new(false))).unwrap();
    client.connect().unwrap();
    let nonidr = EncodedPacket::Video(
        VideoPacket::new(
            generation(),
            1_000_000_000,
            1_000_000_000,
            Arc::from([0, 0, 0, 1, 0x41]),
        )
        .unwrap(),
    );
    assert!(client.packet(&nonidr).is_err());
    let idr = EncodedPacket::Video(
        VideoPacket::new(
            generation(),
            1_000_000_000,
            1_000_000_000,
            Arc::from([0, 0, 0, 1, 0x65]),
        )
        .unwrap(),
    );
    client.packet(&idr).unwrap();
    let wrong = EncodedPacket::Video(
        VideoPacket::new(
            Generation {
                encoder: 8,
                ..generation()
            },
            1_000_000_001,
            1_000_000_001,
            Arc::from([0, 0, 0, 1, 0x65]),
        )
        .unwrap(),
    );
    assert_eq!(client.packet(&wrong), Err(Error::StaleGeneration));
    client.stop().unwrap();
    let fake = client.into_wire();
    for exact in [
        "live/exact",
        "rtmp://127.0.0.1/live?token=raw",
        "stream/key?x=exact",
        "User exact",
        "Password exact",
    ] {
        assert!(
            fake.output
                .windows(exact.len())
                .any(|w| w == exact.as_bytes())
        );
    }
}
#[test]
fn failure_rejection_and_cancellation_do_not_report_publish_success() {
    let mut input = handshake();
    input.extend(command(
        &[
            Value::String("_error".into()),
            Value::Number(1.0),
            Value::Null,
            Value::Object(vec![(
                "description".into(),
                Value::String("Denied literal".into()),
            )]),
        ],
        0,
    ));
    let fake = Fake {
        input,
        fragment: 7,
        ..Default::default()
    };
    let mut client = Client::new(
        fake,
        options("127.0.0.1:1935".parse().unwrap()),
        Arc::new(AtomicBool::new(false)),
    )
    .unwrap();
    assert!(matches!(client.connect(),Err(Error::Rejected(s))if s.contains("Denied literal")));
    let fake = Fake {
        input: responses(),
        fragment: 3,
        ..Default::default()
    };
    let mut client = Client::new(
        fake,
        options("127.0.0.1:1935".parse().unwrap()),
        Arc::new(AtomicBool::new(true)),
    )
    .unwrap();
    assert_eq!(client.connect(), Err(Error::Cancelled));
    let t = ResolvedTarget::new(
        "127.0.0.1:1935".parse().unwrap(),
        "a".into(),
        "rtmpsx://127.0.0.1/a".into(),
        "key".into(),
        None,
        None,
    );
    assert!(matches!(t, Err(Error::Invalid(_))));
}

#[test]
fn incomplete_assemblies_stream_slots_abort_and_extended_timestamp_are_strictly_bounded() {
    let mut decoder = Decoder::new(2, 8, 8);
    decoder.set_size(2).unwrap();
    let mut cursor = io::Cursor::new([
        3, 0, 0, 0, 0, 0, 6, 20, 0, 0, 0, 0, b'a', b'b', 4, 0, 0, 0, 0, 0, 6, 20, 0, 0, 0, 0,
    ]);
    assert!(
        decoder
            .read(&mut |b| cursor.read_exact(b).map_err(|e| io_error("test", e)))
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        decoder.read(&mut |b| cursor.read_exact(b).map_err(|e| io_error("test", e))),
        Err(Error::Limit(_))
    ));
    decoder.abort(3);
    // A type0 header may replace an explicitly aborted assembly.
    let mut cursor = io::Cursor::new([3, 0, 0, 0, 0, 0, 1, 20, 0, 0, 0, 0, 42]);
    assert_eq!(
        decoder
            .read(&mut |b| cursor.read_exact(b).map_err(|e| io_error("test", e)))
            .unwrap()
            .unwrap()
            .data,
        [42]
    );
    let mut decoder = Decoder::new(1, 8, 8);
    let mut cursor = io::Cursor::new([3, 0, 0, 0, 0, 0, 0, 20, 0, 0, 0, 0, 4]);
    assert_eq!(
        decoder
            .read(&mut |b| cursor.read_exact(b).map_err(|e| io_error("test", e)))
            .unwrap()
            .unwrap()
            .data
            .len(),
        0
    );
    assert!(matches!(
        decoder.read(&mut |b| cursor.read_exact(b).map_err(|e| io_error("test", e))),
        Err(Error::Limit(_))
    ));
    let mut decoder = Decoder::new(1, 8, 8);
    decoder.set_size(1).unwrap();
    let mut cursor = io::Cursor::new([
        3, 255, 255, 255, 0, 0, 2, 9, 1, 0, 0, 0, 1, 0, 0, 0, 7, 0xc3, 1, 0, 0, 1,
    ]);
    assert!(
        decoder
            .read(&mut |b| cursor.read_exact(b).map_err(|e| io_error("test", e)))
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        decoder.read(&mut |b| cursor.read_exact(b).map_err(|e| io_error("test", e))),
        Err(Error::Protocol(_))
    ));
}

#[test]
fn distinct_track_timestamps_are_independent_but_each_track_rejects_rewind() {
    let fake = Fake {
        input: responses(),
        fragment: 1,
        ..Default::default()
    };
    let mut opts = options("127.0.0.1:1935".parse().unwrap());
    opts.timestamp_origin_ns = 0;
    let video_gen = opts.video_generation;
    let audio_gen = opts.audio.as_ref().unwrap().0;
    let mut client = Client::new(fake, opts, Arc::new(AtomicBool::new(false))).unwrap();
    client.connect().unwrap();
    let video = |ns| {
        EncodedPacket::Video(
            VideoPacket::new(
                video_gen,
                ns,
                i128::from(ns),
                Arc::from([0, 0, 0, 2, 0x65, 0x80]),
            )
            .unwrap(),
        )
    };
    let audio = |ns| {
        EncodedPacket::Audio(AudioPacket::new(audio_gen, ns, Arc::from([0x21, 0x10])).unwrap())
    };
    client.packet(&video(366_666_600)).unwrap();
    client.packet(&audio(20_800)).unwrap();
    client.packet(&video(400_000_000)).unwrap();
    client.packet(&audio(21_354_100)).unwrap();
    assert!(matches!(
        client.packet(&video(399_000_000)),
        Err(Error::Invalid(
            "packet DTS moved backwards on the same track"
        ))
    ));
    assert!(matches!(
        client.packet(&audio(21_000_000)),
        Err(Error::Invalid(
            "packet DTS moved backwards on the same track"
        ))
    ));
    client.stop().unwrap();
    let wire = client.into_wire();
    let mut input = std::io::Cursor::new(&wire.output[3073..]);
    let mut decoder = Decoder::new(64, 2 * 1024 * 1024 + 5, 4 * 1024 * 1024);
    let mut media = vec![];
    while (input.position() as usize) < wire.output.len() - 3073 {
        let Some(message) = decoder
            .read(&mut |b| {
                input
                    .read_exact(b)
                    .map_err(|e| io_error("independent-track fixture", e))
            })
            .unwrap()
        else {
            continue;
        };
        if message.type_id == 1 {
            decoder
                .set_size(u32::from_be_bytes(message.data[..4].try_into().unwrap()))
                .unwrap();
        } else if matches!(message.type_id, 8 | 9) && message.data.get(1) == Some(&1) {
            media.push((message.type_id, message.timestamp));
        }
    }
    assert_eq!(
        media,
        [(9, 366), (8, 0), (9, 400), (8, 21)],
        "exact observed DTS only declared millisecond wire quantization"
    );
}

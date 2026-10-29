//! Opt-in localhost only. No test or socket is executed by adding this source.
use super::*;
use super::{
    amf::Value,
    chunk::{Decoder, Message},
};
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::Arc,
    thread,
    time::{Duration, Instant},
};
fn receive(socket: &mut TcpStream, decoder: &mut Decoder) -> Message {
    loop {
        if let Some(m) = decoder
            .read(&mut |b| {
                socket
                    .read_exact(b)
                    .map_err(|e| io_error("loopback read", e))
            })
            .unwrap()
        {
            return m;
        }
    }
}
fn send(socket: &mut TcpStream, message: Message) {
    chunk::write(
        &message,
        if message.type_id <= 6 { 2 } else { 3 },
        128,
        &mut |bytes| {
            socket
                .write_all(bytes)
                .map_err(|e| io_error("loopback write", e))
        },
    )
    .unwrap();
}
fn reply(socket: &mut TcpStream, stream: u32, values: &[Value]) {
    send(
        socket,
        Message {
            timestamp: 0,
            type_id: 20,
            stream,
            data: amf::encode(values).unwrap(),
        },
    );
}
fn fixture(path: &std::path::Path, max: usize) -> Vec<u8> {
    let mut data = Vec::new();
    std::fs::File::open(path)
        .unwrap()
        .take((max + 1) as u64)
        .read_to_end(&mut data)
        .unwrap();
    assert!(data.len() <= max);
    data
}
#[test]
#[ignore = "explicit opt-in localhost RTMP socket/receipt acceptance; no external server"]
fn localhost_publish_roundtrip_and_optional_flv_receipt() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut socket, peer) = listener.accept().unwrap();
        assert!(peer.ip().is_loopback());
        socket
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        socket
            .set_write_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut c0c1 = [0u8; 1537];
        socket.read_exact(&mut c0c1).unwrap();
        assert_eq!(c0c1[0], 3);
        let mut s1 = [0; 1536];
        s1[8..].fill(71);
        socket.write_all(&[3]).unwrap();
        socket.write_all(&s1).unwrap();
        let mut c2 = [0; 1536];
        socket.read_exact(&mut c2).unwrap();
        assert_eq!(c2, s1);
        socket.write_all(&c0c1[1..]).unwrap();
        let mut decoder = Decoder::new(64, 2 * 1024 * 1024 + 5, 4 * 1024 * 1024);
        let mut tags = Vec::new();
        let mut published = false;
        let mut deleted = false;
        for _ in 0..128 {
            let message = receive(&mut socket, &mut decoder);
            match message.type_id {
                1 => decoder
                    .set_size(u32::from_be_bytes(message.data[..4].try_into().unwrap()))
                    .unwrap(),
                20 => {
                    let values = amf::decode(&message.data).unwrap();
                    match values.first() {
                        Some(Value::String(name)) if name == "connect" => {
                            reply(
                                &mut socket,
                                0,
                                &super::tests::success(
                                    1.0,
                                    Value::Object(vec![(
                                        "code".into(),
                                        Value::String("NetConnection.Connect.Success".into()),
                                    )]),
                                ),
                            );
                        }
                        Some(Value::String(name)) if name == "createStream" => reply(
                            &mut socket,
                            0,
                            &super::tests::success(2.0, Value::Number(7.0)),
                        ),
                        Some(Value::String(name)) if name == "publish" => {
                            assert_eq!(message.stream, 7);
                            assert_eq!(
                                values.get(3),
                                Some(&Value::String("literal/key?token=abc".into()))
                            );
                            published = true;
                            reply(&mut socket, 7, &super::tests::publish_success());
                        }
                        Some(Value::String(name)) if name == "FCUnpublish" => {}
                        Some(Value::String(name)) if name == "deleteStream" => {
                            deleted = true;
                            break;
                        }
                        _ => panic!("unexpected loopback command"),
                    }
                }
                8 | 9 => {
                    assert!(published);
                    assert_eq!(message.stream, 7);
                    assert!(tags.len() < 8);
                    tags.push(message);
                }
                3..=5 => {}
                _ => panic!("unexpected message type"),
            }
        }
        assert!(deleted, "EOS/unpublish/delete must precede local stop");
        tags
    });
    let mut options = super::tests::options(address);
    let mut video_au: Arc<[u8]> = Arc::from([0, 0, 0, 2, 0x65, 0x80]);
    let mut audio_au: Arc<[u8]> = Arc::from([0x21, 0x10]);
    let supplied = if let Some(dir) = std::env::var_os("SKYLIVE_RTMP_TEST_FIXTURES") {
        let dir = std::path::PathBuf::from(dir);
        options.video =
            VideoConfig::new(Arc::from(fixture(&dir.join("video.avcc"), 32768))).unwrap();
        options.audio = Some((
            Generation {
                encoder: 4,
                ..super::tests::generation()
            },
            AudioConfig::new(Arc::from(fixture(&dir.join("audio.asc"), 64))).unwrap(),
        ));
        video_au = Arc::from(fixture(&dir.join("video.avcc-au"), 2 * 1024 * 1024));
        audio_au = Arc::from(fixture(&dir.join("audio.raw-au"), 65536));
        true
    } else {
        false
    };
    let mut publisher = Publisher::start(options).unwrap();
    let key = publisher.key();
    let until = Instant::now() + Duration::from_secs(10);
    loop {
        match publisher.poll() {
            Some(Event::Publishing { key: actual }) => {
                assert_eq!(actual, key);
                break;
            }
            Some(Event::Failed { error, .. }) => panic!("{error}"),
            _ => {
                assert!(Instant::now() < until);
                thread::sleep(Duration::from_millis(1));
            }
        }
    }
    let video = EncodedPacket::Video(
        VideoPacket::new(
            super::tests::generation(),
            1_000_000_000,
            1_000_000_000,
            video_au,
        )
        .unwrap(),
    );
    let audio = EncodedPacket::Audio(
        AudioPacket::new(
            Generation {
                encoder: 4,
                ..super::tests::generation()
            },
            1_000_000_000,
            audio_au,
        )
        .unwrap(),
    );
    publisher.try_packet(&video).unwrap();
    publisher.try_packet(&audio).unwrap();
    publisher.stop();
    assert_eq!(publisher.try_packet(&video), Err(Error::Closed));
    loop {
        match publisher.poll() {
            Some(Event::Stopped { key: actual, stats }) => {
                assert_eq!(actual, key);
                assert_eq!(stats.packets, 2);
                break;
            }
            Some(Event::Failed { error, .. }) => panic!("{error}"),
            _ => {
                assert!(Instant::now() < until);
                thread::sleep(Duration::from_millis(1));
            }
        }
    }
    publisher.join().unwrap();
    let tags = server.join().unwrap();
    assert_eq!(tags.len(), 5);
    assert_eq!(&tags[0].data[..5], &[0x17, 0, 0, 0, 0]);
    assert_eq!(tags[1].data[1], 0);
    assert_eq!(tags[2].data[1], 1);
    assert_eq!(tags[3].data[1], 1);
    assert_eq!(tags[4].data, [0x17, 2, 0, 0, 0]);
    if let Some(path) = std::env::var_os("SKYLIVE_RTMP_RECEIPT") {
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(path)
            .unwrap();
        file.write_all(&[b'F', b'L', b'V', 1, 5, 0, 0, 0, 9, 0, 0, 0, 0])
            .unwrap();
        for tag in tags {
            let mut header = vec![tag.type_id];
            chunk::put24(&mut header, tag.data.len() as u32);
            chunk::put24(&mut header, tag.timestamp & 0xffffff);
            header.push((tag.timestamp >> 24) as u8);
            header.extend_from_slice(&[0; 3]);
            file.write_all(&header).unwrap();
            file.write_all(&tag.data).unwrap();
            file.write_all(&((tag.data.len() + 11) as u32).to_be_bytes())
                .unwrap();
        }
        file.sync_all().unwrap();
        eprintln!(
            "FLV receipt written; actual encoded fixtures supplied={supplied}; default tiny wire fixtures are NOT decoder conformance evidence"
        );
    }
}

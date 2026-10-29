//! Explicit Windows CLI/native/local-only composition. No authored input device,
//! microphone, sound playback, external RTMP endpoint, PCM or recording output.
use super::*;
#[path = "streaming_fixture/mod.rs"]
mod fixture;
use std::time::Duration;
fn request(cli: &mut LiveProtocol, value: Value) -> Value {
    let input = cli.input.as_mut().unwrap();
    writeln!(input, "{value}").unwrap();
    input.flush().unwrap();
    let value = cli
        .replies
        .recv_timeout(Duration::from_secs(30))
        .expect("bounded native CLI reply");
    assert_eq!(value["ok"], true, "{value}");
    value["result"].clone()
}
#[test]
#[ignore = "explicit opt-in native Windows CLI streaming and localhost RTMP; no physical inputs"]
fn cli_actual_start_fresh_audio_clock_frame_status_stop_close_and_eof() {
    assert_eq!(
        std::env::var("SKYLIVE_CLI_STREAMING_ACCEPTANCE").as_deref(),
        Ok("1")
    );
    let server = fixture::Loopback::bind();
    let mut cli = LiveProtocol::new();
    let temp = Temp::new();
    let saved = temp.0.join("synthetic-stream.json");
    request(
        &mut cli,
        json!({"request":"command","command":{"op":"set_video","width":64,"height":48,"fps_num":30,"fps_den":1}}),
    );
    request(
        &mut cli,
        json!({"request":"command","command":{"op":"add_color_source","scene":1,"name":"Synthetic red CLI Program","rgba":[255,0,0,255]}}),
    );
    let settings = json!({"server_url":format!("rtmp://{}/live",server.address),"stream_key":"literal/key?token=abc","video_bitrate_kbps":1000,"keyframe_interval_seconds":1,"audio_enabled":true,"audio_bitrate_kbps":128});
    request(
        &mut cli,
        json!({"request":"command","command":{"op":"profile","command":{"op":"set_streaming","id":1,"settings":settings}}}),
    );
    request(&mut cli, json!({"request":"save","path":saved}));
    let started = request(&mut cli, json!({"request":"start_streaming"}));
    assert_eq!(started["started"], true);
    assert_eq!(started["status"]["phase"], "publishing");
    let mut key = started["status"]["key"].clone();
    key["epoch"] = started["status"]["epoch"].clone();
    let origin = started["status"]["origin_ns"].as_u64().unwrap();
    assert!(
        origin > 0,
        "actual Program clock sampled after encoder/network readiness"
    );
    // The source has no audio device: the shared CPU Program legitimately emits
    // silence. PCM travels directly from that owner; CLI never reads PCM.
    for _ in 1..12 {
        std::thread::sleep(Duration::from_millis(35));
        let frame = request(&mut cli, json!({"request":"stream_frame","key":key}));
        assert_eq!(frame["accepted"], true);
    }
    let status = request(&mut cli, json!({"request":"streaming_status"}));
    assert_eq!(status["active"], true);
    assert_eq!(status["status"]["origin_ns"], origin);
    let stopped = request(&mut cli, json!({"request":"stop_streaming","key":key}));
    assert_eq!(stopped["actual_owners_closed"], true);
    assert_eq!(stopped["status"]["phase"], "closed");
    assert!(stopped["status"]["failure"].is_null());
    assert_eq!(stopped["status"]["video_frames"], 12);
    request(&mut cli, json!({"request":"close"}));
    assert!(cli.finish());
    let tags = server.finish();
    let configuration = tags
        .iter()
        .find(|t| t.type_id == 9 && t.data.get(1) == Some(&0))
        .unwrap();
    assert!(configuration.data.len() > 12);
    assert_eq!(configuration.data[5], 1);
    assert_eq!(configuration.data[6], 66, "actual Baseline avcC");
    let asc = tags
        .iter()
        .find(|t| t.type_id == 8 && t.data.get(1) == Some(&0))
        .unwrap();
    assert_eq!(&asc.data[2..], &[0x11, 0x90]);
    let mut video_count = 0usize;
    let mut audio_count = 0usize;
    let mut audio_last = None;
    let mut video_last = None;
    for tag in &tags {
        if tag.type_id == 9 && tag.data.get(1) == Some(&1) {
            assert!(video_last.is_none_or(|p| tag.timestamp >= p));
            video_last = Some(tag.timestamp);
            assert!(tag.data.len() > 5);
            video_count += 1;
        } else if tag.type_id == 8 && tag.data.get(1) == Some(&1) {
            assert!(audio_last.is_none_or(|p| tag.timestamp >= p));
            audio_last = Some(tag.timestamp);
            assert!(tag.data.len() > 2);
            audio_count += 1;
        }
    }
    assert_eq!(video_count, 12);
    assert!(audio_count > 8);
    assert_eq!(
        audio_count as u64,
        stopped["status"]["audio_packets"].as_u64().unwrap()
    );
    assert_eq!(
        audio_count as u64 * 1024,
        stopped["status"]["native_aac"]["submitted_frames"]
            .as_u64()
            .unwrap()
    );
    let receipt = json!({"success":true,"scope":"actual CLI/nativeWindows/localhost protocol only; codec decode is the separate host acceptance and external oracle","origin_ns":origin,"video_frames":video_count,"audio_packets":audio_count,"coded_audio_frames":audio_count*1024,"status":stopped["status"],"physical_input":false,"gui_pcm_polling":false,"actual_closed":true,"independent_decode_performed":false});
    if let Some(path) = std::env::var_os("SKYLIVE_CLI_STREAMING_EVIDENCE_DIR") {
        let directory = std::path::PathBuf::from(path);
        assert!(directory.is_absolute());
        assert!(directory.parent().unwrap().exists());
        std::fs::create_dir(&directory).unwrap();
        let mut flv = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(directory.join("stream.flv"))
            .unwrap();
        flv.write_all(&[b'F', b'L', b'V', 1, 5, 0, 0, 0, 9, 0, 0, 0, 0])
            .unwrap();
        let mut bytes = 13usize;
        for tag in &tags {
            let n = tag.data.len();
            bytes = bytes.checked_add(n + 15).unwrap();
            assert!(bytes <= 16 * 1024 * 1024);
            let ts = tag.timestamp;
            flv.write_all(&[
                tag.type_id,
                (n >> 16) as u8,
                (n >> 8) as u8,
                n as u8,
                (ts >> 16) as u8,
                (ts >> 8) as u8,
                ts as u8,
                (ts >> 24) as u8,
                0,
                0,
                0,
            ])
            .unwrap();
            flv.write_all(&tag.data).unwrap();
            flv.write_all(&((n + 11) as u32).to_be_bytes()).unwrap();
        }
        flv.sync_all().unwrap();
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(directory.join("protocol-acceptance.json"))
            .unwrap();
        serde_json::to_writer_pretty(&mut file, &receipt).unwrap();
        file.sync_all().unwrap();
    }
    println!("NATIVE_CLI_STREAMING_ACCEPTANCE {receipt}");
}
#[test]
#[ignore = "explicit opt-in active-EOF Windows native/localhost retirement; no physical inputs"]
fn cli_eof_actually_drains_video_only_owner_before_process_exit() {
    assert_eq!(
        std::env::var("SKYLIVE_CLI_STREAMING_ACCEPTANCE").as_deref(),
        Ok("1")
    );
    let server = fixture::Loopback::bind();
    let mut cli = LiveProtocol::new();
    let temp = Temp::new();
    let saved = temp.0.join("video-only-stream.json");
    request(
        &mut cli,
        json!({"request":"command","command":{"op":"set_video","width":64,"height":48,"fps_num":30,"fps_den":1}}),
    );
    let settings = json!({"server_url":format!("rtmp://{}/live",server.address),"stream_key":"literal/key?token=abc","video_bitrate_kbps":1000,"keyframe_interval_seconds":1,"audio_enabled":false,"audio_bitrate_kbps":128});
    request(
        &mut cli,
        json!({"request":"command","command":{"op":"profile","command":{"op":"set_streaming","id":1,"settings":settings}}}),
    );
    request(&mut cli, json!({"request":"save","path":saved}));
    let started = request(&mut cli, json!({"request":"start_streaming","time_ns":0}));
    let mut key = started["status"]["key"].clone();
    key["epoch"] = started["status"]["epoch"].clone();
    request(
        &mut cli,
        json!({"request":"stream_frame","key":key,"time_ns":100000000}),
    );
    assert!(cli.finish(), "active EOF must wait actual owner retirement");
    let tags = server.finish();
    let video = tags
        .iter()
        .filter(|t| t.type_id == 9 && t.data.get(1) == Some(&1))
        .count();
    assert_eq!(video, 2);
    assert!(
        tags.iter()
            .any(|t| t.type_id == 9 && t.data.get(1) == Some(&2))
    );
    assert!(!tags.iter().any(|t| t.type_id == 8));
}

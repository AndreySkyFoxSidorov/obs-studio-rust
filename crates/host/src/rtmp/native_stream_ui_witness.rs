//! Windows opt-in GUIв†’localhost witness. It records transport bytes, not decoder acceptance.
use super::streaming_test_support::{Loopback, LoopbackConfig};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Write,
    path::Path,
    time::{Duration, Instant},
};

const MAX_TAGS: usize = 2048;
const MAX_FLV_BYTES: usize = 32 * 1024 * 1024;
const STREAM_KEY: &str = "gui-witness";

fn create(path: &Path) -> fs::File {
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .unwrap()
}
fn json(path: &Path, value: &serde_json::Value) {
    let mut file = create(path);
    serde_json::to_writer_pretty(&mut file, value).unwrap();
    file.write_all(b"\n").unwrap();
    file.sync_all().unwrap();
}

#[test]
#[ignore = "explicit opt-in Windows GUI localhost RTMP witness; binds one loopback socket for up to90s"]
fn gui_localhost_rtmp_witness() {
    let destination = std::env::var_os("SKYLIVE_GUI_RTMP_WITNESS_DIR")
        .expect("set SKYLIVE_GUI_RTMP_WITNESS_DIR to a NEW absolute run directory");
    let directory = std::path::PathBuf::from(destination);
    assert!(
        directory.is_absolute(),
        "evidence directory must be absolute"
    );
    // Parent must already exist. Never reuse, clean, truncate, or replace an old run.
    fs::create_dir(&directory).expect("witness requires a new owned run directory");
    let started = Instant::now();
    let server = Loopback::bind_with(LoopbackConfig {
        expected_key: STREAM_KEY.into(),
        max_tags: MAX_TAGS,
        // Includes13-byte FLV header and every15-byte tag framing in the32MiB final cap.
        max_payload_bytes: MAX_FLV_BYTES - 13 - MAX_TAGS * 15,
        max_messages: 8192,
        overall: Duration::from_secs(90),
        io_timeout: Duration::from_secs(5),
    });
    assert!(server.address.ip().is_loopback());
    let address = server.address;
    json(
        &directory.join("ready.json"),
        &serde_json::json!({
            "schema":1, "ready":true, "address":address.to_string(),
            "host":"127.0.0.1", "port":address.port(),
            "server_url":format!("rtmp://{address}/live"), "stream_key":STREAM_KEY,
            "flv_path":directory.join("received.flv"), "report_path":directory.join("report.json"),
            "scope":"GUI Program publishes to this localhost TEST ONLY witness; no external destination",
            "max_tags":MAX_TAGS, "max_flv_bytes":MAX_FLV_BYTES,
            "overall_timeout_seconds":90, "read_timeout_seconds":5,
            "decoder_acceptance":"not_run"
        }),
    );
    println!(
        "GUI_RTMP_WITNESS_READY {}",
        directory.join("ready.json").display()
    );
    let received =
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| server.finish_report())) {
            Ok(value) => value,
            Err(payload) => {
                let reason = payload
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_owned()))
                    .unwrap_or_else(|| "test server panicked; see original test output".into());
                json(
                    &directory.join("report.json"),
                    &serde_json::json!({
                        "schema":1,"transport_complete":false,"failure":reason,
                        "elapsed_seconds":started.elapsed().as_secs_f64(),
                        "decoder_acceptance":"not_run","external_connection":false,
                        "flv_created":false
                    }),
                );
                std::panic::resume_unwind(payload);
            }
        };
    let tags = &received.tags;
    let audio = tags.iter().any(|t| t.type_id == 8);
    let video = tags.iter().any(|t| t.type_id == 9);
    let mut file = create(&directory.join("received.flv"));
    let mut digest = Sha256::new();
    let mut written = 0usize;
    let mut write = |bytes: &[u8]| {
        written = written.checked_add(bytes.len()).unwrap();
        assert!(written <= MAX_FLV_BYTES);
        file.write_all(bytes).unwrap();
        digest.update(bytes);
    };
    write(&[
        b'F',
        b'L',
        b'V',
        1,
        (u8::from(audio) << 2) | u8::from(video),
        0,
        0,
        0,
        9,
        0,
        0,
        0,
        0,
    ]);
    for tag in tags {
        let n = tag.data.len();
        assert!(n <= 0x00ff_ffff);
        let t = tag.timestamp;
        write(&[
            tag.type_id,
            (n >> 16) as u8,
            (n >> 8) as u8,
            n as u8,
            (t >> 16) as u8,
            (t >> 8) as u8,
            t as u8,
            (t >> 24) as u8,
            0,
            0,
            0,
        ]);
        write(&tag.data);
        write(&((n + 11) as u32).to_be_bytes());
    }
    file.sync_all().unwrap();
    let video_header = tags
        .iter()
        .filter(|t| {
            t.type_id == 9
                && t.data.first().is_some_and(|v| v & 15 == 7)
                && t.data.get(1) == Some(&0)
        })
        .count();
    let audio_header = tags
        .iter()
        .filter(|t| {
            t.type_id == 8
                && t.data.first().is_some_and(|v| v >> 4 == 10)
                && t.data.get(1) == Some(&0)
        })
        .count();
    let video_packets = tags
        .iter()
        .filter(|t| t.type_id == 9 && t.data.get(1) == Some(&1))
        .count();
    let audio_packets = tags
        .iter()
        .filter(|t| t.type_id == 8 && t.data.get(1) == Some(&1))
        .count();
    let complete = received.unpublished
        && received.deleted
        && video_header > 0
        && audio_header > 0
        && video_packets > 0
        && audio_packets > 0;
    let report = serde_json::json!({
        "schema":1, "transport_complete":complete, "peer_enforced_loopback":true,
        "fc_unpublish_received":received.unpublished,"delete_stream_received":received.deleted,
        "address":address.to_string(), "elapsed_seconds":started.elapsed().as_secs_f64(),
        "flv_path":directory.join("received.flv"), "flv_bytes":written,
        "flv_sha256":format!("{:x}",digest.finalize()),
        "audio_header_tags":audio_header,"video_header_tags":video_header,
        "audio_packet_tags":audio_packets,"video_packet_tags":video_packets,
        "tags":tags.iter().map(|tag|serde_json::json!({"type":tag.type_id,"timestamp_ms":tag.timestamp,"bytes":tag.data.len()})).collect::<Vec<_>>(),
        "decoder_acceptance":"not_run", "native_encoder_provenance":"GUI process; independently verify built executable receipt",
        "external_connection":false,"physical_device_activation_by_witness":false
    });
    json(&directory.join("report.json"), &report);
    assert!(
        complete,
        "expected GUI audio/video header+packet tags and both real unpublish/delete commands; inspect report.json"
    );
    println!(
        "GUI_RTMP_WITNESS_FINISHED {}",
        directory.join("report.json").display()
    );
}

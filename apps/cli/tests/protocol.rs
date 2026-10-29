use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
};
static SERIAL: AtomicU64 = AtomicU64::new(1);

#[test]
fn obs_filter_ids_forward_pixels_and_session_uuid_policy_cross_the_actual_cli() {
    let temp = Temp::new();
    let baseline = temp.0.join("baseline.json");
    let native = temp.0.join("filters.json");
    let obs = temp.0.join("same-session.json");
    let generated = temp.0.join("native-reopen.json");
    let white = temp.0.join("white.ppm");
    let black = temp.0.join("black.ppm");
    let fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../crates/host/tests/fixtures/obs-color-correction-v2.json");
    let mut cli = LiveProtocol::new();
    cli.good(json!({"request":"command","command":{"op":"set_video","width":16,"height":16,"fps_num":30,"fps_den":1}}));
    cli.good(json!({"request":"save","path":baseline}));
    cli.good(json!({"request":"import_obs","path":fixture,"width":16,"height":16,"fps_num":30,"fps_den":1}));
    let snapshot = cli.good(json!({"request":"snapshot"}));
    let source = snapshot["collection"]["sources"][0]["id"].clone();
    let original = snapshot["collection"]["sources"][0]["video_filters"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(original[0]["name"], "Черный");
    assert!(original.iter().all(|f| f["id"].as_u64().is_some()));
    cli.good(json!({"request":"render_program","path":white,"width":16,"height":16,"time_ns":0}));
    let mut next = original.clone();
    next.swap(0, 1);
    next[1]["name"] = json!("Темний колір");
    let wire_gamma = f64::from(-1.93_f32);
    next[1]["effect"]["gamma"] = json!(wire_gamma);
    cli.good(json!({"request":"command","command":{"op":"set_source_video_filters","source":source,"filters":next}}));
    cli.good(json!({"request":"render_program","path":black,"width":16,"height":16,"time_ns":0}));
    cli.good(json!({"request":"export_obs","path":obs}));
    cli.good(json!({"request":"save","path":native}));
    cli.good(json!({"request":"load","path":native}));
    let reopened = cli.good(json!({"request":"snapshot"}));
    assert_eq!(
        reopened["collection"]["sources"][0]["video_filters"][1]["effect"]["gamma"]
            .as_f64()
            .unwrap()
            .to_bits(),
        wire_gamma.to_bits()
    );
    assert_eq!(
        reopened["collection"]["sources"][0]["video_filters"][0]["id"],
        original[1]["id"]
    );
    cli.good(json!({"request":"export_obs","path":generated}));
    cli.good(json!({"request":"close"}));
    assert!(cli.finish());
    assert_ppm(&white, 16, 16, &vec![[255; 3]; 256]);
    assert_ppm(&black, 16, 16, &vec![[0; 3]; 256]);
    let saved: Value = serde_json::from_slice(&fs::read(obs).unwrap()).unwrap();
    let reloaded: Value = serde_json::from_slice(&fs::read(generated).unwrap()).unwrap();
    assert_eq!(
        saved["sources"][0]["filters"][1]["settings"]["gamma"]
            .as_f64()
            .unwrap()
            .to_bits(),
        wire_gamma.to_bits()
    );
    assert_eq!(
        saved["sources"][0]["filters"][0]["uuid"],
        "5457ed57-09de-4c8e-aad8-3708dd393b9b"
    );
    assert_eq!(saved["sources"][0]["filters"][1]["name"], "Темний колір");
    assert_ne!(
        saved["sources"][0]["filters"][0]["uuid"],
        reloaded["sources"][0]["filters"][0]["uuid"]
    );
    assert_eq!(
        saved["sources"][0]["filters"][1]["name"],
        reloaded["sources"][0]["filters"][1]["name"]
    );
}

#[test]
fn obs_color_correction_builtin_uses_shared_command_pixels_history_and_native_reopen() {
    let temp = Temp::new();
    let native = temp.0.join("obs-color.json");
    let filtered = temp.0.join("filtered.ppm");
    let clear = temp.0.join("clear.ppm");
    let reopened = temp.0.join("reopened.ppm");
    let effect = json!({"kind":"color_correction","gamma":0.0,"contrast":0.0,"brightness":0.0,"saturation":0.0,
        "hue_shift":120.0,"opacity":1.0,"color_multiply":[255,255,255],"color_add":[0,0,0]});
    let (ok, replies) = protocol(&[
        json!({"request":"command","command":{"op":"set_video","width":16,"height":16,"fps_num":30,"fps_den":1}}),
        json!({"request":"command","command":{"op":"add_color_source","scene":1,"name":"Red","rgba":[255,0,0,255]}}),
        json!({"request":"command","command":{"op":"set_source_video_filters","source":2,"filters":[{"enabled":true,"effect":effect}]}}),
        json!({"request":"command","command":{"op":"set_plugin_enabled","plugin":"skylive/craft-video-effects","enabled":false}}),
        json!({"request":"render_program","path":filtered,"width":16,"height":16,"time_ns":0}),
        json!({"request":"save","path":native}),
        json!({"request":"command","command":{"op":"undo"}}),
        json!({"request":"render_program","path":clear,"width":16,"height":16,"time_ns":0}),
        json!({"request":"command","command":{"op":"redo"}}),
        json!({"request":"save","path":native}),
        json!({"request":"load","path":native}),
        json!({"request":"render_program","path":reopened,"width":16,"height":16,"time_ns":0}),
        json!({"request":"close"}),
    ]);
    assert!(ok, "{replies:?}");
    assert_ppm(&filtered, 16, 16, &vec![[0, 0, 255]; 256]);
    assert_ppm(&clear, 16, 16, &vec![[255, 0, 0]; 256]);
    assert_eq!(fs::read(filtered).unwrap(), fs::read(reopened).unwrap());
    let data: Value = serde_json::from_slice(&fs::read(native).unwrap()).unwrap();
    assert_eq!(
        data["collection"]["sources"][0]["video_filters"][0]["effect"]["hue_shift"],
        120.0
    );
}
struct Temp(std::path::PathBuf);
#[test]
fn builtin_obs_audio_filters_have_real_pcm_and_native_roundtrip_without_plugin_activation() {
    let temp = Temp::new();
    let path = temp.0.join("obs-audio.json");
    let binding = microphone_binding();
    let mut cli = LiveProtocol::new();
    cli.good(json!({"request":"command","command":{"op":"add_audio_capture_source","scene":1,"name":"Offline input","binding":binding}}));
    let filters = json!([{"kind":"obs_gain","db":20.0,"enabled":true},{"kind":"obs_polarity_invert","enabled":true}]);
    cli.good(json!({"request":"command","command":{"op":"set_source_audio_filters","source":2,"filters":filters}}));
    cli.good(
        json!({"request":"command","command":{"op":"add_existing_source","scene":1,"source":2}}),
    );
    cli.good(json!({"request":"command","command":{"op":"set_audio","source":2,"gain":0.1,"muted":false}}));
    let identity = cli.good(json!({"request":"audio_program_reset"}))["identity"].clone();
    cli.good(program_packet(
        &identity,
        &binding,
        1,
        1,
        "0",
        48000,
        json!([0.25, -0.5]),
    ));
    cli.good(json!({"request":"audio_program_finish","identity":identity,"binding":binding,"stream_generation":1,"sequence":1}));
    let output = cli.good(json!({"request":"render_audio_program","start_frame":0,"frames":2}));
    assert_eq!(output["block"]["samples"], json!([-0.25, -0.25, 0.5, 0.5]));
    assert_eq!(output["block"]["gaps"], json!([]));
    assert_eq!(output["native_capture"], false);
    cli.good(json!({"request":"save","path":path}));
    cli.good(json!({"request":"load","path":path}));
    assert_eq!(
        cli.good(json!({"request":"snapshot"}))["collection"]["sources"][0]["audio_filters"],
        filters
    );
    cli.good(json!({"request":"close"}));
    assert!(cli.finish());
    let native: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    assert_eq!(native["schema"], skylive_host::storage::CURRENT_SCHEMA);
    assert_eq!(native["collection"]["sources"][0]["audio_filters"], filters);
}
impl Temp {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "skylive-cli-{}-{}",
            std::process::id(),
            SERIAL.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&p).unwrap();
        Self(p)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn protocol(requests: &[Value]) -> (bool, Vec<Value>) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_skylive-cli"))
        .arg("--stdio")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    {
        let mut input = child.stdin.take().unwrap();
        for request in requests {
            writeln!(input, "{request}").unwrap();
        }
    }
    let result = child.wait_with_output().unwrap();
    (
        result.status.success(),
        String::from_utf8(result.stdout)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect(),
    )
}
fn native_audio(document: &Value) -> Value {
    let selected = &document["profiles"]["selected"];
    let profile = document["profiles"]["profiles"]
        .as_array()
        .unwrap()
        .iter()
        .find(|profile| &profile["id"] == selected)
        .unwrap();
    json!({"sample_rate":profile["sample_rate"],"speakers":profile["speakers"],"buffering":document["preferences"]["audio_buffering"]})
}
fn legacy_v1(document: Value) -> Value {
    let audio = native_audio(&document);
    let selected = &document["profiles"]["selected"];
    let profile = document["profiles"]["profiles"]
        .as_array()
        .unwrap()
        .iter()
        .find(|profile| &profile["id"] == selected)
        .unwrap();
    let mut collection = document["collection"].clone();
    collection["audio"] = audio;
    collection["video"] = profile["video"].clone();
    collection["revision"] = document["revision"].clone();
    let mut legacy = json!({"schema":1,"collection":collection});
    if let Some(selection) = document.get("selection") {
        legacy["selection"] = selection.clone();
    }
    legacy
}
fn write_bmp(path: &std::path::Path, colors: [[u8; 3]; 4]) {
    let mut bytes = vec![0u8; 70];
    bytes[..2].copy_from_slice(b"BM");
    bytes[2..6].copy_from_slice(&70u32.to_le_bytes());
    bytes[10..14].copy_from_slice(&54u32.to_le_bytes());
    bytes[14..18].copy_from_slice(&40u32.to_le_bytes());
    bytes[18..22].copy_from_slice(&2i32.to_le_bytes());
    bytes[22..26].copy_from_slice(&2i32.to_le_bytes());
    bytes[26..28].copy_from_slice(&1u16.to_le_bytes());
    bytes[28..30].copy_from_slice(&24u16.to_le_bytes());
    for (index, rgb) in colors.into_iter().enumerate() {
        let x = index % 2;
        let y = index / 2;
        let offset = 54 + (1 - y) * 8 + x * 3;
        bytes[offset..offset + 3].copy_from_slice(&[rgb[2], rgb[1], rgb[0]]);
    }
    fs::write(path, bytes).unwrap();
}
fn write_two_frame_gif(path: &std::path::Path, colors: [[u8; 3]; 2]) {
    let mut bytes = b"GIF89a".to_vec();
    bytes.extend_from_slice(&[1, 0, 1, 0, 0x80, 0, 0]);
    for color in colors {
        bytes.extend_from_slice(&color);
    }
    bytes.extend_from_slice(b"\x21\xff\x0bNETSCAPE2.0\x03\x01\x00\x00\x00");
    for data in [0x44, 0x4c] {
        bytes.extend_from_slice(&[0x21, 0xf9, 4, 0, 10, 0, 0, 0]);
        bytes.extend_from_slice(&[0x2c, 0, 0, 0, 0, 1, 0, 1, 0, 0, 2, 2, data, 1, 0]);
    }
    bytes.push(0x3b);
    fs::write(path, bytes).unwrap();
}

fn color_scenes() -> Vec<Value> {
    // Stable issued identities: red source/item 2/3, blue scene4/source5/item6.
    vec![
        json!({"request":"command","command":{"op":"set_video","width":16,"height":16,"fps_num":30,"fps_den":1}}),
        json!({"request":"command","command":{"op":"add_color_source","scene":1,"name":"Red","rgba":[255,0,0,255]}}),
        json!({"request":"command","command":{"op":"add_scene","name":"Blue scene"}}),
        json!({"request":"command","command":{"op":"add_color_source","scene":4,"name":"Blue","rgba":[0,0,255,255]}}),
    ]
}

fn microphone_binding() -> Value {
    json!({"flow":"input","target":{"kind":"default"},"use_device_timing":false,"exclude_own_audio":false})
}

/// Request/response subprocess boundary with a bounded response wait. PCM
/// fixtures never invoke native enumeration, capture or audio output requests.
struct LiveProtocol {
    child: Option<std::process::Child>,
    input: Option<std::process::ChildStdin>,
    replies: std::sync::mpsc::Receiver<Value>,
    reader: Option<std::thread::JoinHandle<()>>,
}
impl LiveProtocol {
    fn new() -> Self {
        use std::io::BufRead;
        let mut child = Command::new(env!("CARGO_BIN_EXE_skylive-cli"))
            .arg("--stdio")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let input = child.stdin.take();
        let output = child.stdout.take().unwrap();
        let (send, replies) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            for line in std::io::BufReader::new(output).lines() {
                let Ok(line) = line else {
                    break;
                };
                let value = serde_json::from_str(&line).unwrap();
                if send.send(value).is_err() {
                    break;
                }
            }
        });
        Self {
            child: Some(child),
            input,
            replies,
            reader: Some(reader),
        }
    }
    fn ask(&mut self, request: Value) -> Value {
        let input = self.input.as_mut().unwrap();
        writeln!(input, "{request}").unwrap();
        input.flush().unwrap();
        self.replies
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("CLI must answer bounded CPU request")
    }
    fn good(&mut self, request: Value) -> Value {
        let response = self.ask(request);
        assert_eq!(response["ok"], true, "{response}");
        response["result"].clone()
    }
    fn finish(mut self) -> bool {
        self.input.take();
        let result = self.child.take().unwrap().wait_with_output().unwrap();
        self.reader.take().unwrap().join().unwrap();
        result.status.success()
    }
}
impl Drop for LiveProtocol {
    fn drop(&mut self) {
        self.input.take();
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}
fn endpoint_binding(id: &str) -> Value {
    json!({"flow":"input","target":{"kind":"windows_endpoint","id":id},"use_device_timing":true,"exclude_own_audio":false})
}
fn program_packet(
    identity: &Value,
    binding: &Value,
    generation: u64,
    sequence: u64,
    time: &str,
    rate: u32,
    samples: Value,
) -> Value {
    json!({"request":"audio_program_packet","identity":identity,"binding":binding,"stream_generation":generation,"sequence":sequence,"first_media_ns":time,"received_media_ns":time,"sample_rate":rate,"channels":1,"channel_mask":4,"samples":samples})
}

#[test]
fn timestamped_program_pcm_transport_finish_and_native_save_reopen_use_actual_runtime_identity() {
    let temp = Temp::new();
    let saved = temp.0.join("program.json");
    let binding = microphone_binding();
    let mut cli = LiveProtocol::new();
    cli.good(json!({"request":"command","command":{"op":"add_audio_capture_source","scene":1,"name":"Program microphone","binding":binding}}));
    cli.good(json!({"request":"command","command":{"op":"set_audio","source":2,"gain":0.5,"muted":false}}));
    let reset = cli.good(json!({"request":"audio_program_reset"}));
    let identity = reset["identity"].clone();
    assert!(identity["instance"].as_u64().unwrap() > 0);
    assert_eq!(reset["native_capture"], false);
    assert_eq!(reset["audio_output"], false);
    cli.good(program_packet(
        &identity,
        &binding,
        1,
        1,
        "0",
        48000,
        json!([-0.5, 0.25]),
    ));
    cli.good(program_packet(
        &identity,
        &binding,
        1,
        2,
        "41667",
        48000,
        json!([0.0, 0.75]),
    ));
    cli.good(json!({"request":"audio_program_finish","identity":identity,"binding":binding,"stream_generation":1,"sequence":2}));
    let rendered = cli.good(json!({"request":"render_audio_program","start_frame":0,"frames":4}));
    assert_eq!(
        rendered["block"]["samples"],
        json!([-0.25, -0.25, 0.125, 0.125, 0.0, 0.0, 0.375, 0.375])
    );
    assert_eq!(rendered["block"]["gaps"], json!([]));
    assert_eq!(rendered["native_capture"], false);
    assert_eq!(rendered["audio_output"], false);
    cli.good(json!({"request":"save","path":saved}));
    cli.good(json!({"request":"load","path":saved}));
    assert!(
        cli.good(json!({"request":"audio_program_status"}))["status"].is_null(),
        "native reload discards derived PCM runtime"
    );
    cli.good(json!({"request":"close"}));
    assert!(cli.finish());
    let native: Value = serde_json::from_slice(&fs::read(saved).unwrap()).unwrap();
    assert_eq!(
        native_audio(&native),
        json!({"sample_rate":48000,"speakers":"stereo","buffering":{"max_ms":0,"fixed":false}})
    );
    for forbidden in [
        "samples",
        "routing_events",
        "stream_generation",
        "data_generation",
    ] {
        assert!(
            !native.to_string().contains(forbidden),
            "runtime {forbidden} leaked into native model"
        );
    }
}

#[test]
fn timestamped_cli_cut_and_gain_preserve_old_route_inside_first_1024_frame_block() {
    let temp = Temp::new();
    let a = endpoint_binding("fixture-a");
    let b = endpoint_binding("fixture-b");
    let mut cli = LiveProtocol::new();
    cli.good(json!({"request":"command","command":{"op":"add_audio_capture_source","scene":1,"name":"A microphone","binding":a}}));
    cli.good(json!({"request":"command","command":{"op":"add_scene","name":"B scene"}}));
    cli.good(json!({"request":"command","command":{"op":"add_audio_capture_source","scene":4,"name":"B microphone","binding":b}}));
    cli.good(json!({"request":"command","command":{"op":"set_transition","kind":"cut","duration_ms":300}}));
    let identity = cli.good(json!({"request":"audio_program_reset"}))["identity"].clone();
    cli.good(json!({"request":"advance","time_ns":10_000_000}));
    cli.good(json!({"request":"select_preview","scene":4}));
    assert_eq!(
        cli.good(json!({"request":"audio_program_route"}))["changed"],
        true
    );
    cli.good(json!({"request":"advance","time_ns":15_000_000}));
    cli.good(json!({"request":"command","command":{"op":"set_audio","source":5,"gain":0.5,"muted":false}}));
    cli.good(json!({"request":"audio_program_route"}));
    cli.good(program_packet(
        &identity,
        &a,
        1,
        1,
        "0",
        48000,
        json!(vec![0.25; 1024]),
    ));
    cli.good(program_packet(
        &identity,
        &b,
        1,
        1,
        "0",
        48000,
        json!(vec![0.75; 1024]),
    ));
    let output = cli.good(json!({"request":"render_audio_program","start_frame":0,"frames":1024}));
    for (frame, samples) in output["block"]["samples"]
        .as_array()
        .unwrap()
        .chunks_exact(2)
        .enumerate()
    {
        let expected = if frame < 480 {
            0.25
        } else if frame < 720 {
            0.75
        } else {
            0.375
        };
        assert_eq!(samples, [json!(expected), json!(expected)], "frame {frame}");
    }
    assert_eq!(output["block"]["gaps"], json!([]));
    cli.good(json!({"request":"save","path":temp.0.join("cut.json")}));
    cli.good(json!({"request":"close"}));
    assert!(cli.finish());
}

#[test]
fn cli_packet_resampling_finishes_exactly_and_audio_settings_reopen_and_migrate_missing_defaults() {
    let temp = Temp::new();
    let saved = temp.0.join("resampled.json");
    let binding = microphone_binding();
    let mut cli = LiveProtocol::new();
    cli.good(json!({"request":"command","command":{"op":"set_audio_settings","settings":{"sample_rate":44100,"speakers":"mono"}}}));
    cli.good(json!({"request":"command","command":{"op":"add_audio_capture_source","scene":1,"name":"Resampled microphone","binding":binding}}));
    let identity = cli.good(json!({"request":"audio_program_reset"}))["identity"].clone();
    let mut produced = 0;
    for (sequence, time) in [(1, "0"), (2, "62500000")] {
        let report = cli.good(program_packet(
            &identity,
            &binding,
            1,
            sequence,
            time,
            16000,
            json!(vec![0.25; 1000]),
        ));
        produced += report["report"]["output_frames"].as_u64().unwrap();
    }
    produced+=cli.good(json!({"request":"audio_program_finish","identity":identity,"binding":binding,"stream_generation":1,"sequence":2}))["report"]["output_frames"].as_u64().unwrap();
    assert_eq!(produced, 5513);
    cli.good(json!({"request":"render_audio_program","start_frame":0,"frames":1024}));
    let interior =
        cli.good(json!({"request":"render_audio_program","start_frame":1024,"frames":1024}));
    assert!(
        interior["block"]["samples"]
            .as_array()
            .unwrap()
            .iter()
            .all(|sample| (sample.as_f64().unwrap() - 0.25).abs() < 0.0001)
    );
    assert_eq!(interior["block"]["gaps"], json!([]));
    cli.good(json!({"request":"save","path":saved}));
    cli.good(json!({"request":"load","path":saved}));
    let snapshot = cli.good(json!({"request":"snapshot"}));
    assert_eq!(
        snapshot["collection"]["audio"],
        json!({"sample_rate":44100,"speakers":"mono","buffering":{"max_ms":0,"fixed":false}})
    );
    assert!(cli.good(json!({"request":"audio_program_status"}))["status"].is_null());
    cli.good(json!({"request":"close"}));
    assert!(cli.finish());
    let mut legacy = legacy_v1(serde_json::from_slice(&fs::read(saved).unwrap()).unwrap());
    legacy["collection"]
        .as_object_mut()
        .unwrap()
        .remove("audio");
    let legacy_path = temp.0.join("missing-audio.json");
    fs::write(&legacy_path, serde_json::to_vec(&legacy).unwrap()).unwrap();
    let (ok, replies) = protocol(&[
        json!({"request":"load","path":legacy_path}),
        json!({"request":"snapshot"}),
    ]);
    assert!(ok, "{replies:?}");
    assert_eq!(
        replies[1]["result"]["collection"]["audio"],
        json!({"sample_rate":48000,"speakers":"stereo","buffering":{"max_ms":0,"fixed":false}})
    );
}

#[test]
fn typed_audio_buffering_settings_are_global_outside_scene_history_and_native_reopen_does_not_start_capture()
 {
    let temp = Temp::new();
    let low_path = temp.0.join("low-latency.json");
    let custom_path = temp.0.join("custom-adaptive.json");
    let mut cli = LiveProtocol::new();
    cli.good(json!({"request":"command","command":{"op":"add_audio_capture_source","scene":1,"name":"Metadata microphone","binding":microphone_binding()}}));
    let before = cli.good(json!({"request":"snapshot"}))["collection"].clone();
    let low =
        json!({"sample_rate":48000,"speakers":"stereo","buffering":{"max_ms":20,"fixed":true}});
    let custom =
        json!({"sample_rate":48000,"speakers":"stereo","buffering":{"max_ms":209,"fixed":false}});
    let applied =
        cli.good(json!({"request":"command","command":{"op":"set_audio_settings","settings":low}}));
    assert_eq!(applied["changed"], true);
    let low_revision = applied["revision"].clone();
    let noop =
        cli.good(json!({"request":"command","command":{"op":"set_audio_settings","settings":low}}));
    assert_eq!(noop["changed"], false);
    assert_eq!(noop["revision"], low_revision);
    cli.good(json!({"request":"command","command":{"op":"undo"}}));
    assert_eq!(
        cli.good(json!({"request":"snapshot"}))["collection"]["audio"],
        low,
        "scene undo must preserve user-global buffering"
    );
    cli.good(json!({"request":"command","command":{"op":"redo"}}));
    assert_eq!(
        cli.good(json!({"request":"snapshot"}))["collection"]["audio"],
        low
    );
    cli.good(json!({"request":"save","path":low_path}));
    cli.good(json!({"request":"command","command":{"op":"set_audio_settings","settings":custom}}));
    let current = cli.good(json!({"request":"snapshot"}))["collection"].clone();
    assert_eq!(current["audio"], custom);
    for key in ["video", "sources", "scenes", "global_audio"] {
        assert_eq!(
            current[key], before[key],
            "buffer policy must not alter {key}"
        );
    }
    cli.good(json!({"request":"command","command":{"op":"undo"}}));
    assert_eq!(
        cli.good(json!({"request":"snapshot"}))["collection"]["audio"],
        custom
    );
    cli.good(json!({"request":"command","command":{"op":"redo"}}));
    assert_eq!(
        cli.good(json!({"request":"snapshot"}))["collection"]["audio"],
        custom
    );
    let cpu = cli.good(json!({"request":"audio_program_reset"}));
    assert_eq!(cpu["status"]["settings"], custom);
    assert_eq!(cpu["status"]["continuous_native_transport"], false);
    assert_eq!(cpu["native_capture"], false);
    let capabilities = cli.good(json!({"request":"capabilities"}));
    assert_eq!(capabilities["audio"]["adaptive_buffering"], true);
    assert_eq!(capabilities["audio"]["native_capture_verified"], false);
    let inactive = cli.good(json!({"request":"read_native_audio_program","max_blocks":1}));
    assert_eq!(inactive["blocks"], json!([]));
    assert!(inactive["status"].is_null());
    assert_eq!(inactive["native_capture_requested"], false);
    cli.good(json!({"request":"save","path":custom_path}));
    cli.good(json!({"request":"load","path":custom_path}));
    assert_eq!(
        cli.good(json!({"request":"snapshot"}))["collection"]["audio"],
        custom
    );
    assert!(cli.good(json!({"request":"audio_program_status"}))["status"].is_null());
    let inactive = cli.good(json!({"request":"read_native_audio_program"}));
    assert_eq!(inactive["blocks"], json!([]));
    assert!(inactive["status"].is_null());
    cli.good(json!({"request":"load","path":low_path}));
    assert_eq!(
        cli.good(json!({"request":"snapshot"}))["collection"]["audio"],
        low
    );
    cli.good(json!({"request":"close"}));
    assert!(cli.finish());
    for (path, expected) in [(&low_path, &low), (&custom_path, &custom)] {
        let saved: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        assert_eq!(&native_audio(&saved), expected);
        assert!(saved["collection"].get("audio").is_none());
        assert!(saved["collection"].get("video").is_none());
        for runtime in [
            "buffer_blocks",
            "waiting_blocks",
            "suppressed_blocks",
            "wall_ticks",
            "pending_inputs",
            "input_packets",
            "samples",
        ] {
            assert!(
                !saved.to_string().contains(runtime),
                "runtime {runtime} must not enter native collection"
            );
        }
    }
    // Native AudioSettings from before the additive field retain their rate
    // and speakers and acquire the same default as legacy request payloads.
    let mut legacy = legacy_v1(serde_json::from_slice(&fs::read(custom_path).unwrap()).unwrap());
    legacy["collection"]["audio"]
        .as_object_mut()
        .unwrap()
        .remove("buffering");
    let legacy_path = temp.0.join("missing-buffering.json");
    fs::write(&legacy_path, serde_json::to_vec(&legacy).unwrap()).unwrap();
    let (ok, replies) = protocol(&[
        json!({"request":"load","path":legacy_path}),
        json!({"request":"snapshot"}),
    ]);
    assert!(ok, "{replies:?}");
    assert_eq!(
        replies[1]["result"]["collection"]["audio"],
        json!({"sample_rate":48000,"speakers":"stereo","buffering":{"max_ms":0,"fixed":false}})
    );
}

#[test]
fn invalid_audio_buffer_policy_range_and_types_reject_as_structured_requests() {
    for buffering in [
        json!({"max_ms":1001,"fixed":false}),
        json!({"max_ms":20,"fixed":"false"}),
    ] {
        let (ok, replies) = protocol(&[
            json!({"request":"snapshot"}),
            json!({"request":"command","command":{"op":"set_audio_settings","settings":{"sample_rate":48000,"speakers":"stereo","buffering":buffering}}}),
        ]);
        assert!(!ok);
        assert_eq!(replies.len(), 2);
        assert_eq!(replies[0]["ok"], true);
        assert_eq!(replies[1]["ok"], false);
        assert_eq!(replies[1]["error"]["code"], "invalid_request");
        assert!(replies[1]["error"].get("phase").is_none());
    }
}

#[test]
fn cli_program_rejects_stale_identity_sequence_eof_and_past_grid_without_fake_success() {
    // The protocol intentionally exits after the first structured error. Each
    // boundary case therefore gets its own fresh subprocess and real identity.
    for case in [
        "identity",
        "generation",
        "sequence",
        "eof",
        "past_grid",
        "past_route",
        "format",
        "decimal",
    ] {
        let binding = microphone_binding();
        let mut cli = LiveProtocol::new();
        cli.good(json!({"request":"command","command":{"op":"add_audio_capture_source","scene":1,"name":"Validated microphone","binding":binding}}));
        let old_identity = cli.good(json!({"request":"audio_program_reset"}))["identity"].clone();
        let identity = cli.good(json!({"request":"audio_program_reset"}))["identity"].clone();
        cli.good(program_packet(
            &identity,
            &binding,
            2,
            1,
            "0",
            48000,
            json!([0.25, 0.5]),
        ));
        let (request, code) = match case {
            "identity" => (
                program_packet(&old_identity, &binding, 2, 2, "41667", 48000, json!([0.75])),
                "program_audio_invalid",
            ),
            "generation" => (
                program_packet(&identity, &binding, 1, 2, "41667", 48000, json!([0.75])),
                "program_audio_invalid",
            ),
            "sequence" => (
                program_packet(&identity, &binding, 2, 1, "41667", 48000, json!([0.75])),
                "program_audio_invalid",
            ),
            "eof" => (
                json!({"request":"audio_program_finish","identity":identity,"binding":binding,"stream_generation":1,"sequence":1}),
                "program_audio_invalid",
            ),
            "decimal" => (
                program_packet(
                    &identity,
                    &binding,
                    2,
                    2,
                    "not-a-timestamp",
                    48000,
                    json!([0.75]),
                ),
                "program_audio_invalid",
            ),
            "format" => {
                cli.good(json!({"request":"command","command":{"op":"set_audio_settings","settings":{"sample_rate":44100,"speakers":"mono"}}}));
                (
                    json!({"request":"audio_program_route"}),
                    "program_audio_format_changed",
                )
            }
            "past_grid" | "past_route" => {
                let rendered =
                    cli.good(json!({"request":"render_audio_program","start_frame":0,"frames":2}));
                assert_eq!(rendered["block"]["samples"], json!([0.25, 0.25, 0.5, 0.5]));
                (
                    if case == "past_grid" {
                        json!({"request":"render_audio_program","start_frame":0,"frames":1})
                    } else {
                        json!({"request":"audio_program_route"})
                    },
                    "program_audio_clock_rewind",
                )
            }
            _ => unreachable!(),
        };
        let rejected = cli.ask(request);
        assert_eq!(rejected["ok"], false, "case {case}: {rejected}");
        assert_eq!(rejected["error"]["code"], code, "case {case}: {rejected}");
        assert!(rejected["error"].get("phase").is_none());
        assert!(
            !cli.finish(),
            "case {case}: structured failure must set subprocess failure status"
        );
    }
}
#[test]
fn audio_authoring_native_reopen_and_explicit_pcm_use_one_source_fader() {
    let temp = Temp::new();
    let native = temp.0.join("audio-native.json");
    let ppm = temp.0.join("audio-only.ppm");
    let (ok, replies) = protocol(&[
        json!({"request":"command","command":{"op":"add_audio_capture_source","scene":1,"name":"Microphone","binding":microphone_binding()}}),
        json!({"request":"command","command":{"op":"set_global_audio","slot":"mic1","source":2}}),
        json!({"request":"command","command":{"op":"set_audio","source":2,"gain":0.5,"muted":false}}),
        json!({"request":"audio_plan"}),
        json!({"request":"mix_program_audio","sample_rate":48000,"channels":1,"frames":2,"inputs":[{"kind":"samples","source":2,"sample_rate":48000,"channels":1,"samples":[-0.5,0.25]}]}),
        json!({"request":"render","path":ppm,"width":2,"height":2}),
        json!({"request":"save","path":native}),
        json!({"request":"load","path":native}),
        json!({"request":"mix_program_audio","sample_rate":48000,"channels":1,"frames":2,"inputs":[{"kind":"samples","source":2,"sample_rate":48000,"channels":1,"samples":[-0.5,0.25]}]}),
        json!({"request":"capabilities"}),
        json!({"request":"close"}),
    ]);
    assert!(ok, "{replies:?}");
    let plan = &replies[3]["result"]["plan"];
    assert_eq!(plan["sources"].as_array().unwrap().len(), 1);
    assert_eq!(plan["sources"][0]["source"], 2);
    assert_eq!(
        replies[4]["result"]["mixed"]["samples"],
        json!([-0.25, 0.125])
    );
    assert_eq!(
        replies[8]["result"]["mixed"]["samples"],
        replies[4]["result"]["mixed"]["samples"]
    );
    assert_eq!(replies[4]["result"]["native_capture"], false);
    assert_eq!(replies[4]["result"]["audio_output"], false);
    assert_eq!(
        replies[9]["result"]["audio"]["native_capture_verified"],
        false
    );
    assert_eq!(
        fs::read(ppm)
            .unwrap()
            .split(|byte| *byte == b'\n')
            .next_back()
            .unwrap(),
        &[0u8; 12]
    );
    let saved: Value = serde_json::from_slice(&fs::read(native).unwrap()).unwrap();
    assert!(!saved.to_string().contains("samples"));
}

#[test]
fn program_audio_plan_excludes_studio_preview_and_mute_is_shared() {
    let temp = Temp::new();
    let (ok, replies) = protocol(&[
        json!({"request":"command","command":{"op":"add_audio_capture_source","scene":1,"name":"Program mic","binding":microphone_binding()}}),
        json!({"request":"command","command":{"op":"add_scene","name":"Preview scene"}}),
        json!({"request":"command","command":{"op":"add_audio_capture_source","scene":4,"name":"Preview mic","binding":microphone_binding()}}),
        json!({"request":"studio_mode","enabled":true}),
        json!({"request":"select_preview","scene":4}),
        json!({"request":"audio_plan"}),
        json!({"request":"command","command":{"op":"set_audio","source":2,"gain":1.0,"muted":true}}),
        json!({"request":"mix_program_audio","sample_rate":48000,"channels":1,"frames":1,"inputs":[{"kind":"samples","source":2,"sample_rate":48000,"channels":1,"samples":[0.75]}]}),
        json!({"request":"command","command":{"op":"undo"}}),
        json!({"request":"mix_program_audio","sample_rate":48000,"channels":1,"frames":1,"inputs":[{"kind":"samples","source":2,"sample_rate":48000,"channels":1,"samples":[0.75]}]}),
        json!({"request":"save","path":temp.0.join("audio.json")}),
        json!({"request":"close"}),
    ]);
    assert!(ok, "{replies:?}");
    assert_eq!(
        replies[5]["result"]["plan"]["sources"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(replies[5]["result"]["plan"]["sources"][0]["source"], 2);
    assert_eq!(replies[7]["result"]["mixed"]["samples"], json!([0.0]));
    assert_eq!(replies[9]["result"]["mixed"]["samples"], json!([0.75]));
}

#[test]
fn pcm_missing_input_and_rate_conversion_are_explicit_errors() {
    for inputs in [
        json!([]),
        json!([{"kind":"samples","source":2,"sample_rate":44100,"channels":1,"samples":[0.5]}]),
    ] {
        let (ok, replies) = protocol(&[
            json!({"request":"command","command":{"op":"add_audio_capture_source","scene":1,"name":"Mic","binding":microphone_binding()}}),
            json!({"request":"mix_program_audio","sample_rate":48000,"channels":1,"frames":1,"inputs":inputs}),
        ]);
        assert!(!ok);
        assert_eq!(replies[1]["ok"], false);
        assert!(
            replies[1]["error"]["code"]
                .as_str()
                .unwrap()
                .starts_with("audio_")
        );
        assert!(replies[1]["error"].get("phase").is_none());
    }
}
fn assert_ppm(path: &std::path::Path, width: usize, height: usize, pixels: &[[u8; 3]]) {
    let mut expected = format!("P6\n{width} {height}\n255\n").into_bytes();
    for pixel in pixels {
        expected.extend_from_slice(pixel);
    }
    assert_eq!(pixels.len(), width * height);
    assert_eq!(fs::read(path).unwrap(), expected);
}

#[test]
fn display_source_crud_native_reopen_does_not_implicitly_grab_or_enumerate() {
    let temp = Temp::new();
    let native = temp.0.join("display-native.json");
    let binding = json!({"target":{"platform":"windows","device_id":"fixture-do-not-enumerate"},"capture_cursor":true,"width":2,"height":2});
    let rebound = json!({"target":{"platform":"windows","device_id":"fixture-new-do-not-enumerate"},"capture_cursor":false,"width":4,"height":2});
    let (ok, replies) = protocol(&[
        json!({"request":"command","command":{"op":"set_video","width":16,"height":16,"fps_num":30,"fps_den":1}}),
        json!({"request":"command","command":{"op":"add_display_capture_source","scene":1,"name":"Screen metadata","binding":binding}}),
        json!({"request":"command","command":{"op":"add_scene","name":"Shared screen"}}),
        json!({"request":"command","command":{"op":"add_existing_source","scene":4,"source":2}}),
        json!({"request":"command","command":{"op":"rebind_display_capture","source":2,"binding":rebound}}),
        json!({"request":"save","path":native}),
        json!({"request":"load","path":native}),
        json!({"request":"capabilities"}),
        json!({"request":"close"}),
    ]);
    assert!(ok, "{replies:?}");
    let state = &replies[6]["result"];
    assert_eq!(state["collection"]["sources"][0]["id"], 2);
    let mut expected_binding = rebound.clone();
    expected_binding["method"] = json!("gdi");
    assert_eq!(
        state["collection"]["sources"][0]["kind"],
        json!({"kind":"display_capture","binding":expected_binding})
    );
    assert_eq!(
        state["collection"]["scenes"][0]["items"][0]["transform"]["width"],
        4.0
    );
    assert_eq!(
        state["collection"]["scenes"][1]["items"][0]["transform"]["width"],
        4.0
    );
    assert_eq!(state["capture"]["frames"], json!([]));
    assert!(
        replies[7]["result"]["implemented"]
            .as_array()
            .unwrap()
            .contains(&json!("display_capture_source"))
    );
    let bytes = fs::read(native).unwrap();
    let saved: Value = serde_json::from_slice(&bytes).unwrap();
    assert!(saved["collection"].get("capture").is_none());
}

#[test]
fn uncaptured_display_export_is_strict_without_starting_native_capture() {
    for obs in [false, true] {
        let temp = Temp::new();
        let output = temp.0.join(if obs {
            "display-obs.json"
        } else {
            "display.ppm"
        });
        let render = if obs {
            json!({"request":"export_obs","path":output})
        } else {
            json!({"request":"render_program","path":output,"width":2,"height":2})
        };
        let (ok, replies) = protocol(&[
            json!({"request":"command","command":{"op":"add_display_capture_source","scene":1,"name":"Uncaptured","binding":{"target":{"platform":"windows","device_id":"fixture-no-native-call"},"capture_cursor":false,"width":2,"height":2}}}),
            render,
        ]);
        assert!(!ok);
        assert_eq!(
            replies[1]["error"]["code"],
            if obs {
                "obs_interchange_failure"
            } else {
                "capture_resources_unavailable"
            }
        );
        assert!(!output.exists());
    }
}

#[test]
fn fade_to_color_frames_native_obs_and_compound_removal_use_the_shared_registry() {
    let temp = Temp::new();
    let native = temp.0.join("color-transition.json");
    let exported = temp.0.join("color-transition-obs.json");
    let paths: Vec<_> = (0..10)
        .map(|index| temp.0.join(format!("color-{index}.ppm")))
        .collect();
    let mut requests = color_scenes();
    requests.extend([
        json!({"request":"command","command":{"op":"add_transition_preset","name":"White bridge","kind":{"fade_to_color":{"rgb":[255,255,255],"switch_point":50}}}}),
        json!({"request":"command","command":{"op":"add_transition_preset","name":"Green bridge","kind":{"fade_to_color":{"rgb":[0,255,0],"switch_point":25}}}}),
        json!({"request":"command","command":{"op":"select_transition","selection":{"reference":{"preset":1},"duration_ms":1000}}}),
        json!({"request":"command","command":{"op":"configure_scene_transition_selection","scene":4,"reference":{"preset":1},"duration_ms":1000}}),
        json!({"request":"command","command":{"op":"update_quick_transition_selection","id":2,"selection":{"reference":{"preset":1},"duration_ms":800},"fade_to_black":false}}),
        json!({"request":"command","command":{"op":"update_quick_transition_selection","id":3,"selection":{"reference":{"preset":1},"duration_ms":1000},"fade_to_black":true}}),
        json!({"request":"studio_mode","enabled":true}),json!({"request":"select_preview","scene":4}),json!({"request":"transition","time_ns":0}),
    ]);
    for (index, time) in [0, 250_000_000, 500_000_000, 750_000_000, 1_000_000_000]
        .into_iter()
        .enumerate()
    {
        requests.push(json!({"request":"render_program","path":paths[index],"width":2,"height":1,"time_ns":time}));
    }
    requests.extend([
        json!({"request":"save","path":native}),
        json!({"request":"load","path":native}),
        json!({"request":"quick_transition","id":3,"time_ns":1_000_000_000}),
    ]);
    for (index, time) in [
        1_250_000_000u64,
        1_500_000_000,
        1_750_000_000,
        2_000_000_000,
    ]
    .into_iter()
    .enumerate()
    {
        requests.push(json!({"request":"render_program","path":paths[index+5],"width":2,"height":1,"time_ns":time}));
    }
    requests.extend([
        json!({"request":"quick_transition","id":3}),
        json!({"request":"render_program","path":paths[9],"width":2,"height":1,"time_ns":3_000_000_000u64}),
    ]);
    let before = requests.len();
    requests.push(json!({"request":"snapshot"}));
    requests.push(json!({"request":"command","command":{"op":"remove_transition_preset_and_references","id":1}}));
    let removed = requests.len();
    requests.push(json!({"request":"snapshot"}));
    requests.push(json!({"request":"command","command":{"op":"undo"}}));
    let undone = requests.len();
    requests.push(json!({"request":"snapshot"}));
    requests.extend([
        json!({"request":"export_obs","path":exported}),
        json!({"request":"save","path":native}),
        json!({"request":"close"}),
    ]);
    let (ok, replies) = protocol(&requests);
    assert!(ok, "{replies:?}");
    for (path, pixel) in paths.iter().zip([
        [255, 0, 0],
        [255, 128, 128],
        [255, 255, 255],
        [128, 128, 255],
        [0, 0, 255],
        [128, 128, 255],
        [255, 255, 255],
        [128, 128, 128],
        [0, 0, 0],
        [255, 0, 0],
    ]) {
        assert_ppm(path, 2, 1, &[pixel; 2]);
    }
    let original = &replies[before]["result"]["collection"];
    let deleted = &replies[removed]["result"]["collection"];
    let restored = &replies[undone]["result"]["collection"];
    assert_eq!(deleted["transition_presets"].as_array().unwrap().len(), 1);
    assert_eq!(deleted["transition"]["reference"], json!({"preset":2}));
    assert_eq!(deleted["transition"]["duration_ms"], 1000);
    assert_eq!(deleted["quick_transitions"].as_array().unwrap().len(), 1);
    assert_eq!(deleted["scenes"][1]["transition_override"], Value::Null);
    assert_eq!(
        deleted["scenes"][1]["inactive_transition_duration_ms"],
        1000
    );
    for field in [
        "transition_presets",
        "transition",
        "quick_transitions",
        "scenes",
    ] {
        assert_eq!(restored[field], original[field]);
    }
    let obs: Value = serde_json::from_slice(&fs::read(exported).unwrap()).unwrap();
    assert_eq!(obs["current_transition"], "White bridge");
    assert_eq!(obs["transitions"][0]["id"], "fade_to_color_transition");
    assert_eq!(
        obs["transitions"][0]["settings"],
        json!({"color":4_294_967_295u32,"switch_point":50})
    );
    assert_eq!(obs["transitions"][1]["name"], "Green bridge");
    assert_eq!(obs["quick_transitions"][1]["name"], "White bridge");
}

#[test]
fn named_transition_presets_are_shared_by_manual_quick_scene_and_native_reopen() {
    let temp = Temp::new();
    let swipe = temp.0.join("shared-swipe.ppm");
    let slide = temp.0.join("shared-slide.ppm");
    let reopened = temp.0.join("reopened.ppm");
    let quick = temp.0.join("shared-quick.ppm");
    let saved = temp.0.join("named.json");
    let mut requests = color_scenes();
    requests.extend([
        json!({"request":"command","command":{"op":"add_transition_preset","name":"Shared swipe","kind":{"swipe":{"direction":"left","swipe_in":false}}}}),
        json!({"request":"command","command":{"op":"add_transition_preset","name":"Unused slide","kind":{"slide":{"direction":"down"}}}}),
        json!({"request":"command","command":{"op":"select_transition","selection":{"reference":{"preset":1},"duration_ms":400}}}),
        json!({"request":"command","command":{"op":"update_quick_transition_selection","id":2,"selection":{"reference":{"preset":1},"duration_ms":800},"fade_to_black":false}}),
        json!({"request":"command","command":{"op":"configure_scene_transition_selection","scene":4,"reference":{"preset":1},"duration_ms":1000}}),
        json!({"request":"studio_mode","enabled":true}),
        json!({"request":"select_preview","scene":4}),
        json!({"request":"tbar_position","value":512}),
        json!({"request":"render_program","path":swipe,"width":4,"height":4,"time_ns":10_000_000_000u64}),
        json!({"request":"tbar_release"}),
    ]);
    let held = requests.len();
    requests.push(json!({"request":"snapshot"}));
    requests.extend([
        json!({"request":"command","command":{"op":"configure_transition_preset","id":1,"name":"Shared vertical","kind":{"slide":{"direction":"up"}}}}),
        json!({"request":"render_program","path":slide,"width":4,"height":4}),
    ]);
    let edited = requests.len();
    requests.push(json!({"request":"snapshot"}));
    requests.extend([
        json!({"request":"save","path":saved}),
        json!({"request":"load","path":saved}),
        json!({"request":"render_program","path":reopened,"width":4,"height":4}),
    ]);
    let loaded = requests.len();
    requests.push(json!({"request":"snapshot"}));
    requests.extend([
        json!({"request":"studio_mode","enabled":true}),
        json!({"request":"select_preview","scene":1}),
        json!({"request":"quick_transition","id":2}),
        json!({"request":"render_program","path":quick,"width":4,"height":4,"time_ns":10_400_000_000u64}),
        json!({"request":"snapshot"}),
    ]);
    let (ok, replies) = protocol(&requests);
    assert!(ok, "{replies:?}");
    let horizontal: Vec<_> = (0..16)
        .map(|index| {
            if index % 4 < 2 {
                [255, 0, 0]
            } else {
                [0, 0, 255]
            }
        })
        .collect();
    let vertical: Vec<_> = (0..16)
        .map(|index| {
            if index / 4 < 2 {
                [255, 0, 0]
            } else {
                [0, 0, 255]
            }
        })
        .collect();
    assert_ppm(&swipe, 4, 4, &horizontal);
    assert_ppm(&slide, 4, 4, &vertical);
    assert_ppm(&reopened, 4, 4, &[[0, 0, 255]; 16]);
    let reverse: Vec<_> = vertical
        .iter()
        .map(|pixel| {
            if *pixel == [255, 0, 0] {
                [0, 0, 255]
            } else {
                [255, 0, 0]
            }
        })
        .collect();
    assert_ppm(&quick, 4, 4, &reverse);
    let before = &replies[held]["result"]["transition"];
    let after = &replies[edited]["result"]["transition"];
    assert_eq!(before["manual"]["current"], 0.5);
    assert_eq!(after["manual"]["current"], 0.5);
    assert_eq!(before["epoch"], after["epoch"]);
    assert_ne!(before["composition"], after["composition"]);
    assert_eq!(after["needs_tick"], false);
    let state = &replies[loaded]["result"];
    let collection = &state["collection"];
    assert_eq!(
        collection["transition_presets"].as_array().unwrap().len(),
        2
    );
    assert_eq!(
        collection["transition_presets"][0]["name"],
        "Shared vertical"
    );
    assert_eq!(
        collection["transition_presets"][0]["kind"],
        json!({"slide":{"direction":"up"}})
    );
    assert_eq!(collection["transition"]["reference"], json!({"preset":1}));
    assert_eq!(
        collection["quick_transitions"][1]["config"]["reference"],
        json!({"preset":1})
    );
    assert_eq!(
        collection["scenes"][1]["transition_override"]["reference"],
        json!({"preset":1})
    );
    assert_eq!(state["transition"]["active"], false);
    assert_eq!(state["transition"]["manual"], Value::Null);
    assert_eq!(
        replies.last().unwrap()["result"]["transition"]["active"],
        true
    );
}

#[test]
fn preset_cli_rejects_invalid_commands_and_atomic_properties_have_one_undo() {
    let temp = Temp::new();
    let mut requests = vec![
        json!({"request":"command","command":{"op":"add_transition_preset","name":"Shared","kind":{"swipe":{"direction":"left","swipe_in":false}}}}),
        json!({"request":"command","command":{"op":"add_transition_preset","name":"Unused","kind":{"slide":{"direction":"down"}}}}),
        json!({"request":"command","command":{"op":"select_transition","selection":{"reference":{"preset":1},"duration_ms":300}}}),
        json!({"request":"snapshot"}),
    ];
    let invalid = [
        (
            json!({"op":"remove_transition_preset","id":1}),
            "transition_preset_referenced",
        ),
        (
            json!({"op":"configure_transition_preset","id":1,"name":"Unused","kind":{"slide":{"direction":"up"}}}),
            "invalid_input",
        ),
        (
            json!({"op":"configure_transition_preset","id":1,"name":"Fade","kind":{"slide":{"direction":"up"}}}),
            "invalid_input",
        ),
        (
            json!({"op":"set_transition_preset_kind","id":1,"kind":"cut"}),
            "invalid_input",
        ),
        (
            json!({"op":"select_transition","selection":{"reference":{"preset":63},"duration_ms":300}}),
            "transition_preset_not_found",
        ),
        (
            json!({"op":"select_transition","selection":{"reference":{"preset":1},"duration_ms":0}}),
            "invalid_request",
        ),
        (
            json!({"op":"set_transition","kind":{"swipe":{"direction":"up","swipe_in":true}},"duration_ms":300}),
            "invalid_input",
        ),
        (
            json!({"op":"add_transition_preset","name":"Bad direction","kind":{"slide":{"direction":"sideways"}}}),
            "invalid_request",
        ),
        (
            json!({"op":"add_transition_preset","name":"Unknown property","kind":{"swipe":{"direction":"up","swipe_in":true,"extra":5}}}),
            "invalid_request",
        ),
    ];
    // This CLI deliberately terminates on the first rejected request. Exercise
    // each transport failure in its own process; core tests prove rollback.
    for (command, code) in &invalid {
        let mut rejected = requests.clone();
        rejected.push(json!({"request":"command","command":command}));
        let (ok, replies) = protocol(&rejected);
        assert!(!ok);
        assert_eq!(replies.len(), rejected.len());
        assert_eq!(
            replies.last().unwrap()["error"]["code"],
            *code,
            "{replies:?}"
        );
    }
    let configured = requests.len();
    requests.extend([
        json!({"request":"command","command":{"op":"configure_transition_preset","id":1,"name":"Renamed","kind":{"slide":{"direction":"up"}}}}),
        json!({"request":"snapshot"}),
        json!({"request":"command","command":{"op":"undo"}}),
        json!({"request":"snapshot"}),
        json!({"request":"command","command":{"op":"redo"}}),
        json!({"request":"snapshot"}),
        json!({"request":"command","command":{"op":"remove_transition_preset","id":2}}),
        json!({"request":"command","command":{"op":"undo"}}),
        json!({"request":"command","command":{"op":"add_transition_preset","name":"New identity","kind":{"slide":{"direction":"right"}}}}),
        json!({"request":"save","path":temp.0.join("presets.json")}),
    ]);
    let (ok, replies) = protocol(&requests);
    assert!(ok, "{replies:?}");
    let initial = &replies[3]["result"]["collection"];
    let edited = &replies[configured + 1]["result"]["collection"];
    let undone = &replies[configured + 3]["result"]["collection"];
    let redone = &replies[configured + 5]["result"]["collection"];
    assert_eq!(edited["transition_presets"][0]["name"], "Renamed");
    assert_eq!(
        edited["transition_presets"][0]["kind"],
        json!({"slide":{"direction":"up"}})
    );
    assert_eq!(undone["transition_presets"], initial["transition_presets"]);
    assert_eq!(redone["transition_presets"], edited["transition_presets"]);
    assert_eq!(
        replies[configured + 8]["result"]["created_transition_preset"],
        3
    );
}

#[test]
fn manual_tbar_renders_video_ticks_held_reverse_cancel_and_high_release_swap() {
    let temp = Temp::new();
    let start = temp.0.join("manual-start.ppm");
    let first = temp.0.join("manual-first.ppm");
    let half = temp.0.join("manual-half.ppm");
    let reverse = temp.0.join("manual-reverse.ppm");
    let cancel = temp.0.join("manual-cancel.ppm");
    let end = temp.0.join("manual-end.ppm");
    let saved = temp.0.join("manual-native.json");
    let mut requests = color_scenes();
    requests.extend([
        json!({"request":"studio_mode","enabled":true}),
        json!({"request":"select_preview","scene":4}),
        json!({"request":"tbar_position","value":512,"time_ns":0}),
        json!({"request":"render_program","path":start,"width":1,"height":1,"time_ns":33_333_332}),
        json!({"request":"render_program","path":first,"width":1,"height":1,"time_ns":33_333_333}),
        json!({"request":"render_program","path":half,"width":1,"height":1,"time_ns":10_000_000_000u64}),
        json!({"request":"tbar_release"}),
        json!({"request":"tbar_position","value":256}),
        json!({"request":"render_program","path":reverse,"width":1,"height":1,"time_ns":20_000_000_000u64}),
        json!({"request":"tbar_position","value":0}),
        json!({"request":"tbar_release"}),
        json!({"request":"render_program","path":cancel,"width":1,"height":1}),
        json!({"request":"tbar_position","value":1023}),
        json!({"request":"advance","time_ns":30_000_000_000u64}),
        json!({"request":"tbar_release"}),
        json!({"request":"render_program","path":end,"width":1,"height":1,"time_ns":40_000_000_000u64}),
        json!({"request":"snapshot"}),
        json!({"request":"save","path":saved}),
    ]);
    let (ok, replies) = protocol(&requests);
    assert!(ok, "{replies:?}");
    for (path, color) in [
        (&start, [255, 0, 0]),
        (&first, [221, 0, 34]),
        (&half, [128, 0, 128]),
        (&reverse, [191, 0, 64]),
        (&cancel, [255, 0, 0]),
        (&end, [0, 0, 255]),
    ] {
        assert_ppm(path, 1, 1, &[color]);
    }
    let held = &replies[10]["result"]["transition"];
    assert_eq!(
        held["manual"],
        json!({"value":512,"current":0.5,"target":0.5,"finishing":false})
    );
    assert_eq!(held["active"], true);
    assert_eq!(held["needs_tick"], false);
    assert_eq!(replies[14]["result"]["program"], 1);
    assert_eq!(replies[14]["result"]["transition"]["active"], false);
    assert_eq!(
        replies[18]["result"]["transition"]["manual"]["finishing"],
        true
    );
    let finished = &replies[20]["result"];
    assert_eq!(finished["program"], 4);
    assert_eq!(finished["preview"], 1);
    assert_eq!(finished["transition"]["manual"], Value::Null);
    assert_eq!(finished["collection"]["revision"], 4);
}

#[test]
fn quick_fade_to_black_uses_linear_gain_and_returns_prepared_b_with_logical_c() {
    let temp = Temp::new();
    let half = temp.0.join("black-half.ppm");
    let black = temp.0.join("black.ppm");
    let incoming = temp.0.join("black-return-half.ppm");
    let returned = temp.0.join("black-return.ppm");
    let mut requests = color_scenes();
    requests.extend([
        json!({"request":"command","command":{"op":"set_color","source":2,"rgba":[255,255,255,255]}}),
        json!({"request":"command","command":{"op":"add_scene","name":"Green scene"}}),
        json!({"request":"command","command":{"op":"add_color_source","scene":7,"name":"Green","rgba":[0,255,0,255]}}),
        json!({"request":"studio_mode","enabled":true}),json!({"request":"select_preview","scene":4}),
        json!({"request":"quick_transition","id":3}),
        json!({"request":"render_program","path":half,"width":1,"height":1,"time_ns":150_000_000}),
        json!({"request":"render_program","path":black,"width":1,"height":1,"time_ns":300_000_000}),
        json!({"request":"snapshot"}),json!({"request":"select_preview","scene":7}),
        json!({"request":"quick_transition","id":3}),
        json!({"request":"render_program","path":incoming,"width":1,"height":1,"time_ns":450_000_000}),
        json!({"request":"render_program","path":returned,"width":1,"height":1,"time_ns":600_000_000}),
        json!({"request":"snapshot"}),json!({"request":"save","path":temp.0.join("black.json")}),
    ]);
    let (ok, replies) = protocol(&requests);
    assert!(ok, "{replies:?}");
    assert_ppm(&half, 1, 1, &[[188, 188, 188]]);
    assert_ppm(&black, 1, 1, &[[0, 0, 0]]);
    assert_ppm(&incoming, 1, 1, &[[0, 0, 188]]);
    assert_ppm(&returned, 1, 1, &[[0, 0, 255]]);
    assert_eq!(replies[12]["result"]["program"], 4);
    assert_eq!(replies[12]["result"]["preview"], 4);
    assert_eq!(replies[17]["result"]["program"], 7);
    assert_eq!(replies[17]["result"]["preview"], 7);
    assert_eq!(replies[17]["result"]["transition"]["active"], false);
    let ordinary = temp.0.join("ordinary-black-half.ppm");
    let mut regular = color_scenes();
    regular.extend([
        json!({"request":"command","command":{"op":"set_color","source":2,"rgba":[255,255,255,255]}}),json!({"request":"command","command":{"op":"add_scene","name":"Empty black"}}),
        json!({"request":"studio_mode","enabled":true}),json!({"request":"select_preview","scene":7}),json!({"request":"transition"}),
        json!({"request":"render_program","path":ordinary,"width":1,"height":1,"time_ns":150_000_000}),json!({"request":"save","path":temp.0.join("ordinary.json")}),
    ]);
    let (ok, replies) = protocol(&regular);
    assert!(ok, "{replies:?}");
    assert_ppm(&ordinary, 1, 1, &[[128, 128, 128]]);
}

#[test]
fn quick_rows_crud_native_persistence_and_scene_override_do_not_change_global_config() {
    let temp = Temp::new();
    let native = temp.0.join("rows.json");
    let ordinary = temp.0.join("override-half.ppm");
    let quick = temp.0.join("quick-half.ppm");
    let mut requests = color_scenes();
    requests.extend([
        json!({"request":"command","command":{"op":"update_quick_transition","id":2,"kind":"fade","duration_ms":500,"fade_to_black":false}}),
        json!({"request":"command","command":{"op":"move_quick_transition","id":2,"index":0}}),
        json!({"request":"command","command":{"op":"remove_quick_transition","id":1}}),
        json!({"request":"command","command":{"op":"add_quick_transition","kind":"cut","duration_ms":800,"fade_to_black":false}}),
        json!({"request":"command","command":{"op":"set_scene_transition","scene":4,"config":{"kind":"fade","duration_ms":1000}}}),
        json!({"request":"save","path":native}),json!({"request":"load","path":native}),
        json!({"request":"studio_mode","enabled":true}),json!({"request":"select_preview","scene":4}),json!({"request":"transition"}),
        json!({"request":"render_program","path":ordinary,"width":1,"height":1,"time_ns":500_000_000}),json!({"request":"advance","time_ns":1_000_000_000}),
        json!({"request":"select_preview","scene":1}),json!({"request":"cut"}),json!({"request":"select_preview","scene":4}),json!({"request":"quick_transition","id":2}),
        json!({"request":"render_program","path":quick,"width":1,"height":1,"time_ns":1_250_000_000}),json!({"request":"snapshot"}),
    ]);
    let (ok, replies) = protocol(&requests);
    assert!(ok, "{replies:?}");
    assert_ppm(&ordinary, 1, 1, &[[128, 0, 128]]);
    assert_ppm(&quick, 1, 1, &[[128, 0, 128]]);
    let loaded = &replies[10]["result"]["collection"];
    assert_eq!(
        loaded["quick_transitions"],
        json!([
            {"id":2,"config":{"reference":"fade","duration_ms":500},"fade_to_black":false},
            {"id":3,"config":{"reference":"fade","duration_ms":300},"fade_to_black":true},
            {"id":4,"config":{"reference":"cut","duration_ms":800},"fade_to_black":false}
        ])
    );
    assert_eq!(
        loaded["scenes"][1]["transition_override"],
        json!({"reference":"fade","duration_ms":1000})
    );
    assert_eq!(
        replies.last().unwrap()["result"]["collection"]["transition"],
        json!({"reference":"fade","duration_ms":300})
    );
    assert_eq!(
        replies.last().unwrap()["result"]["collection"]["revision"],
        9
    );
}

#[test]
fn manual_and_quick_errors_are_explicit_and_prepublication() {
    for (studio, request, code) in [
        (
            false,
            json!({"request":"tbar_position","value":512}),
            "transition_not_studio",
        ),
        (
            false,
            json!({"request":"quick_transition","id":2}),
            "transition_not_studio",
        ),
        (
            true,
            json!({"request":"tbar_position","value":1024}),
            "transition_invalid_position",
        ),
        (
            true,
            json!({"request":"quick_transition","id":500}),
            "transition_quick_not_found",
        ),
        (
            true,
            json!({"request":"quick_transition","id":0}),
            "invalid_request",
        ),
    ] {
        let mut requests = color_scenes();
        if studio {
            requests.push(json!({"request":"studio_mode","enabled":true}));
        }
        requests.push(request);
        let (ok, replies) = protocol(&requests);
        assert!(!ok);
        let error = &replies.last().unwrap()["error"];
        assert_eq!(error["code"], code, "{replies:?}");
        assert!(error.get("phase").is_none());
    }
    for request in [
        json!({"request":"quick_transition","id":2}),
        json!({"request":"tbar_position","value":512}),
    ] {
        let mut requests = color_scenes();
        requests.extend([
            json!({"request":"studio_mode","enabled":true}),
            json!({"request":"select_preview","scene":4}),
            json!({"request":"transition"}),
            request,
        ]);
        let (ok, replies) = protocol(&requests);
        assert!(!ok);
        let error = &replies.last().unwrap()["error"];
        assert_eq!(error["code"], "transition_busy");
        assert!(error.get("phase").is_none());
    }
}

#[test]
fn held_manual_pair_keeps_both_gif_clocks_at_their_independent_activation_times() {
    let temp = Temp::new();
    let a = temp.0.join("a.gif");
    let b = temp.0.join("b.gif");
    write_two_frame_gif(&a, [[255, 0, 0], [0, 255, 0]]);
    write_two_frame_gif(&b, [[0, 0, 255], [255, 255, 255]]);
    let first = temp.0.join("held-first.ppm");
    let next = temp.0.join("held-next.ppm");
    let (ok, replies) = protocol(&[
        json!({"request":"command","command":{"op":"set_video","width":16,"height":16,"fps_num":30,"fps_den":1}}),
        json!({"request":"import_image","scene":1,"name":"A GIF","path":a}),
        json!({"request":"command","command":{"op":"set_transform","scene":1,"item":3,"transform":{"x":0,"y":0,"width":16,"height":16,"opacity":1}}}),
        json!({"request":"command","command":{"op":"add_scene","name":"B"}}),
        json!({"request":"import_image","scene":4,"name":"B GIF","path":b}),
        json!({"request":"command","command":{"op":"set_transform","scene":4,"item":6,"transform":{"x":0,"y":0,"width":16,"height":16,"opacity":1}}}),
        json!({"request":"studio_mode","enabled":true}),
        json!({"request":"select_preview","scene":4,"time_ns":50_000_000}),
        json!({"request":"tbar_position","value":512}),
        json!({"request":"render_program","path":first,"width":1,"height":1,"time_ns":10_050_000_000u64}),
        json!({"request":"tbar_release"}),
        json!({"request":"render_program","path":next,"width":1,"height":1,"time_ns":10_150_000_000u64}),
        json!({"request":"snapshot"}),
        json!({"request":"save","path":temp.0.join("held.json")}),
    ]);
    assert!(ok, "{replies:?}");
    // A has elapsed10.05s/red, B exactly10s/lastwhite (inclusive endpoints).
    assert_ppm(&first, 1, 1, &[[255, 128, 128]]);
    // A has elapsed10.15s/green, B exactly10.1s/firstblue.
    assert_ppm(&next, 1, 1, &[[0, 128, 128]]);
    assert_eq!(
        replies[12]["result"]["transition"]["manual"]["current"],
        0.5
    );
    assert_eq!(replies[12]["result"]["transition"]["needs_tick"], false);
    assert_eq!(replies[12]["result"]["transition"]["active"], true);
}

#[test]
fn program_automatic_fade_renders_start_midpoint_end_and_native_config_reopens() {
    let temp = Temp::new();
    let saved = temp.0.join("fade-native.json");
    let initial = temp.0.join("initial.ppm");
    let start = temp.0.join("start.ppm");
    let middle = temp.0.join("middle.ppm");
    let end = temp.0.join("end.ppm");
    let reopened = temp.0.join("reopened.ppm");
    let mut requests = color_scenes();
    requests.extend([
        json!({"request":"render_program","path":initial,"width":2,"height":1}),
        json!({"request":"select_preview","scene":4,"time_ns":10_000_000}),
        json!({"request":"render_program","path":start,"width":2,"height":1,"time_ns":10_000_000}),
        json!({"request":"render_program","path":middle,"width":2,"height":1,"time_ns":160_000_000}),
        json!({"request":"render_program","path":end,"width":2,"height":1,"time_ns":310_000_000}),
        json!({"request":"snapshot"}),
        json!({"request":"command","command":{"op":"set_transition","kind":"cut","duration_ms":750}}),
        json!({"request":"save","path":saved}),
        json!({"request":"load","path":saved}),
        json!({"request":"render_program","path":reopened,"width":2,"height":1}),
        json!({"request":"close"}),
    ]);
    let (ok, replies) = protocol(&requests);
    assert!(ok, "{replies:?}");
    assert!(
        replies.iter().all(|reply| reply["ok"] == true),
        "{replies:?}"
    );
    for path in [&initial, &start] {
        assert_ppm(path, 2, 1, &[[255, 0, 0]; 2]);
    }
    assert_ppm(&middle, 2, 1, &[[128, 0, 128]; 2]);
    assert_ppm(&end, 2, 1, &[[0, 0, 255]; 2]);
    assert_ppm(&reopened, 2, 1, &[[0, 0, 255]; 2]);
    assert_eq!(replies[5]["result"]["transition"]["active"], true);
    assert_eq!(replies[9]["result"]["transition"]["active"], false);
    assert_eq!(replies[9]["result"]["program"], 4);
    assert_eq!(
        replies[9]["result"]["collection"]["transition"],
        json!({"reference":"fade","duration_ms":300})
    );
    assert_eq!(
        replies[12]["result"]["collection"]["transition"],
        json!({"reference":"cut","duration_ms":750})
    );
    assert_eq!(replies[12]["result"]["dirty"], false);
}

#[test]
fn studio_program_freezes_layout_keeps_shared_properties_swaps_and_exits_without_unsent_preview() {
    let temp = Temp::new();
    let full = temp.0.join("private-layout.ppm");
    let draft = temp.0.join("draft-layout.ppm");
    let shared = temp.0.join("shared-color.ppm");
    let middle = temp.0.join("studio-mid.ppm");
    let exit = temp.0.join("studio-exit.ppm");
    let saved = temp.0.join("studio.json");
    let mut requests = color_scenes();
    requests.extend([
        json!({"request":"studio_mode","enabled":true}),
        json!({"request":"command","command":{"op":"set_transform","scene":1,"item":3,"transform":{"x":8,"y":0,"width":8,"height":16,"opacity":1}}}),
        json!({"request":"render_program","path":full,"width":2,"height":1}),
        json!({"request":"render","path":draft,"width":2,"height":1}),
        json!({"request":"command","command":{"op":"set_color","source":2,"rgba":[0,255,0,255]}}),
        json!({"request":"render_program","path":shared,"width":2,"height":1}),
        json!({"request":"select_preview","scene":4}),
        json!({"request":"transition"}),
        json!({"request":"render_program","path":middle,"width":2,"height":1,"time_ns":150_000_000}),
        json!({"request":"advance","time_ns":300_000_000}),
        json!({"request":"select_preview","scene":1}),
        json!({"request":"studio_mode","enabled":false}),
        json!({"request":"render_program","path":exit,"width":2,"height":1}),
        json!({"request":"save","path":saved}),
        json!({"request":"close"}),
    ]);
    let (ok, replies) = protocol(&requests);
    assert!(ok, "{replies:?}");
    assert_ppm(&full, 2, 1, &[[255, 0, 0]; 2]);
    assert_ppm(&draft, 2, 1, &[[0, 0, 0], [255, 0, 0]]);
    assert_ppm(&shared, 2, 1, &[[0, 255, 0]; 2]);
    assert_ppm(&middle, 2, 1, &[[0, 128, 128]; 2]);
    assert_ppm(&exit, 2, 1, &[[0, 0, 255]; 2]);
    let swapped = &replies[13]["result"];
    assert_eq!(
        (swapped["preview"].as_u64(), swapped["program"].as_u64()),
        (Some(1), Some(4))
    );
    assert_eq!(swapped["transition"]["active"], false);
    let exited = &replies[15]["result"];
    assert_eq!(
        (exited["preview"].as_u64(), exited["program"].as_u64()),
        (Some(4), Some(4))
    );
    assert_eq!(exited["transition"]["studio"], false);
    assert_eq!(exited["collection"]["revision"], 6);
}

#[test]
fn studio_rejects_overlapping_transition_before_publication_and_program_export_is_strict() {
    let temp = Temp::new();
    let saved = temp.0.join("before-busy.json");
    let mut requests = color_scenes();
    requests.extend([
        json!({"request":"studio_mode","enabled":true}),
        json!({"request":"select_preview","scene":4}),
        json!({"request":"transition"}),
        json!({"request":"save","path":saved}),
        json!({"request":"transition","time_ns":100_000_000}),
    ]);
    let (ok, replies) = protocol(&requests);
    assert!(!ok);
    let rejected = replies.last().unwrap();
    assert_eq!(rejected["error"]["code"], "transition_busy");
    assert!(rejected["error"].get("phase").is_none());
    let native: Value = serde_json::from_slice(&fs::read(&saved).unwrap()).unwrap();
    assert_eq!(native["revision"], 4);
    let image = temp.0.join("linked.bmp");
    write_bmp(&image, [[255, 255, 255]; 4]);
    let missing_native = temp.0.join("missing-native.json");
    let output = temp.0.join("partial-program.ppm");
    let mut setup = color_scenes();
    setup.extend([
        json!({"request":"import_image","scene":4,"name":"Linked","path":image}),
        json!({"request":"save","path":missing_native}),
        json!({"request":"close"}),
    ]);
    let (ok, replies) = protocol(&setup);
    assert!(ok, "{replies:?}");
    fs::remove_file(&image).unwrap();
    let (ok, replies) = protocol(&[
        json!({"request":"load","path":missing_native}),
        json!({"request":"studio_mode","enabled":true}),
        json!({"request":"select_preview","scene":4}),
        json!({"request":"transition"}),
        json!({"request":"render_program","path":output,"width":2,"height":1,"time_ns":150_000_000}),
    ]);
    assert!(!ok);
    assert_eq!(
        replies.last().unwrap()["error"]["code"],
        "image_resources_unavailable"
    );
    assert!(
        replies.last().unwrap()["error"]["detail"]
            .as_str()
            .unwrap()
            .contains("image_io")
    );
    assert!(!output.exists());
}

#[test]
fn configured_cut_is_immediate_and_force_cut_retires_an_active_fade() {
    let temp = Temp::new();
    let blue = temp.0.join("cut-blue.ppm");
    let red = temp.0.join("force-cut-red.ppm");
    let saved = temp.0.join("cut.json");
    let mut requests = color_scenes();
    requests.extend([
        json!({"request":"command","command":{"op":"set_transition","kind":"cut","duration_ms":1000}}),
        json!({"request":"select_preview","scene":4,"time_ns":1}),
        json!({"request":"render_program","path":blue,"width":2,"height":1}),
        json!({"request":"command","command":{"op":"set_transition","kind":"fade","duration_ms":300}}),
        json!({"request":"select_preview","scene":1,"time_ns":2}),
        json!({"request":"cut","time_ns":3}),
        json!({"request":"render_program","path":red,"width":2,"height":1}),
        json!({"request":"save","path":saved}),json!({"request":"close"}),
    ]);
    let (ok, replies) = protocol(&requests);
    assert!(ok, "{replies:?}");
    assert_eq!(replies[5]["result"]["transition"]["active"], false);
    assert_eq!(replies[8]["result"]["transition"]["active"], true);
    assert_eq!(replies[9]["result"]["transition"]["active"], false);
    assert_ppm(&blue, 2, 1, &[[0, 0, 255]; 2]);
    assert_ppm(&red, 2, 1, &[[255, 0, 0]; 2]);
}

#[test]
fn program_gif_clock_activates_on_import_and_no_op_reload_while_raw_render_keeps_explicit_time() {
    let temp = Temp::new();
    let gif = temp.0.join("clock.gif");
    let saved = temp.0.join("clock.json");
    let mut bytes = b"GIF89a".to_vec();
    bytes.extend_from_slice(&[1, 0, 1, 0, 0x80, 0, 0, 255, 0, 0, 0, 255, 0]);
    bytes.extend_from_slice(b"\x21\xff\x0bNETSCAPE2.0\x03\x01\0\0\0");
    for data in [0x44, 0x4c] {
        bytes.extend_from_slice(&[
            0x21, 0xf9, 4, 0, 10, 0, 0, 0, 0x2c, 0, 0, 0, 0, 1, 0, 1, 0, 0, 2, 2, data, 1, 0,
        ]);
    }
    bytes.push(0x3b);
    fs::write(&gif, bytes).unwrap();
    let before = temp.0.join("before-reload.ppm");
    let restarted = temp.0.join("restarted.ppm");
    let later = temp.0.join("later.ppm");
    let activated = temp.0.join("activated.ppm");
    let raw_zero = temp.0.join("raw-zero.ppm");
    let raw_explicit = temp.0.join("raw-explicit.ppm");
    let requests = [
        json!({"request":"command","command":{"op":"set_video","width":16,"height":16,"fps_num":30,"fps_den":1}}),
        json!({"request":"import_image","scene":1,"name":"GIF A","path":gif}),
        json!({"request":"command","command":{"op":"transform_item","scene":1,"item":3,"action":"stretch_to_canvas"}}),
        json!({"request":"advance","time_ns":150_000_000}),
        json!({"request":"render_program","path":before,"width":2,"height":1}),
        json!({"request":"reload_image","source":2,"path":gif,"expected_revision":3}),
        json!({"request":"render_program","path":restarted,"width":2,"height":1}),
        json!({"request":"advance","time_ns":250_000_001}),
        json!({"request":"render_program","path":later,"width":2,"height":1}),
        json!({"request":"import_image","scene":1,"name":"GIF B","path":gif}),
        json!({"request":"command","command":{"op":"transform_item","scene":1,"item":5,"action":"stretch_to_canvas"}}),
        json!({"request":"advance","time_ns":350_000_002}),
        json!({"request":"render_program","path":activated,"width":2,"height":1}),
        json!({"request":"render","path":raw_zero,"width":2,"height":1}),
        json!({"request":"render","path":raw_explicit,"width":2,"height":1,"time_ns":150_000_000}),
        json!({"request":"save","path":saved}),
        json!({"request":"close"}),
    ];
    let (ok, replies) = protocol(&requests);
    assert!(ok, "{replies:?}");
    assert_eq!(replies[5]["result"]["changed"], false);
    for path in [&before, &later, &activated, &raw_explicit] {
        assert_ppm(path, 2, 1, &[[0, 255, 0]; 2]);
    }
    for path in [&restarted, &raw_zero] {
        assert_ppm(path, 2, 1, &[[255, 0, 0]; 2]);
    }
    assert_eq!(replies[11]["result"]["transition"]["time_ns"], 350_000_002);
}

#[test]
fn transition_capabilities_are_explicit_and_studio_preference_survives_new_owner() {
    let temp = Temp::new();
    let path = temp.0.join("empty.json");
    let (ok, replies) = protocol(&[
        json!({"request":"capabilities"}),
        json!({"request":"studio_mode","enabled":true}),
        json!({"request":"save","path":path}),
        json!({"request":"new"}),
        json!({"request":"load","path":path}),
        json!({"request":"close"}),
    ]);
    assert!(ok, "{replies:?}");
    let implemented = replies[0]["result"]["implemented"].as_array().unwrap();
    assert!(implemented.contains(&json!("automatic_fade")));
    assert!(implemented.contains(&json!("studio_scene_instances")));
    for capability in [
        "manual_tbar",
        "quick_transitions",
        "fade_to_black",
        "scene_transition_override",
    ] {
        assert!(implemented.contains(&json!(capability)));
    }
    assert!(
        !replies[0]["result"]["unavailable"]
            .as_array()
            .unwrap()
            .contains(&json!("transition_tbar"))
    );
    assert!(
        !replies[0]["result"]["unavailable"]
            .as_array()
            .unwrap()
            .contains(&json!("timed_transitions"))
    );
    assert_eq!(replies[3]["result"]["transition"]["studio"], true);
    assert_eq!(replies[4]["result"]["transition"]["studio"], true);
}
#[test]
fn gif_explicit_timeline_default_zero_finite_stop_and_native_reopen_use_real_pixels() {
    let temp = Temp::new();
    let path = temp.0.join("once.gif");
    let saved = temp.0.join("gif.json");
    // Independent tiny GIF: two full-canvas frames, red/green, 100ms each,
    // no repeat extension => a single play as in OBS.
    let mut bytes = b"GIF89a".to_vec();
    bytes.extend_from_slice(&[1, 0, 1, 0, 0x80, 0, 0, 255, 0, 0, 0, 255, 0]);
    for data in [0x44, 0x4c] {
        bytes.extend_from_slice(&[0x21, 0xf9, 4, 0, 10, 0, 0, 0]);
        bytes.extend_from_slice(&[0x2c, 0, 0, 0, 0, 1, 0, 1, 0, 0, 2, 2, data, 1, 0]);
    }
    bytes.push(0x3b);
    fs::write(&path, bytes).unwrap();
    let times = [
        0,
        100_000_000,
        100_000_001,
        150_000_000,
        200_000_001,
        u64::MAX,
    ];
    let mut requests = vec![
        json!({"request":"import_image","scene":1,"name":"GIF","path":path}),
        json!({"request":"snapshot"}),
    ];
    // First source/item ids are 2/3. Stretch through the public command.
    requests.push(json!({"request":"command","command":{"op":"transform_item","scene":1,"item":3,"action":"stretch_to_canvas"}}));
    for (i, time) in times.iter().enumerate() {
        requests.push(json!({"request":"render","path":temp.0.join(format!("{i}.ppm")),"width":2,"height":1,"time_ns":time}));
    }
    requests.extend([
        json!({"request":"save","path":saved}),
        json!({"request":"load","path":saved}),
        json!({"request":"render","path":temp.0.join("reopen.ppm"),"width":2,"height":1}),
        json!({"request":"close"}),
    ]);
    let (success, responses) = protocol(&requests);
    assert!(success, "{responses:?}");
    assert!(responses.iter().all(|r| r["ok"] == true), "{responses:?}");
    for (i, _) in times.iter().enumerate() {
        let bytes = fs::read(temp.0.join(format!("{i}.ppm"))).unwrap();
        let pixel = if i < 2 { [255, 0, 0] } else { [0, 255, 0] };
        assert_eq!(
            bytes,
            [b"P6\n2 1\n255\n".as_slice(), pixel.repeat(2).as_slice()].concat()
        );
    }
    assert_eq!(
        fs::read(temp.0.join("reopen.ppm")).unwrap(),
        fs::read(temp.0.join("0.ppm")).unwrap()
    );
}
#[test]
fn real_image_import_reopen_changed_file_reload_and_strict_export_share_the_engine() {
    let temp = Temp::new();
    let image = temp.0.join("pattern.bmp");
    let native = temp.0.join("images.json");
    let ppm = temp.0.join("pattern.ppm");
    let colors = [[255, 0, 0], [0, 255, 0], [0, 0, 255], [255, 255, 255]];
    write_bmp(&image, colors);
    let (ok, replies) = protocol(&[
        json!({"request":"command","command":{"op":"set_video","width":16,"height":16,"fps_num":30,"fps_den":1}}),
        json!({"request":"import_image","scene":1,"name":"Pattern","path":image}),
        json!({"request":"command","command":{"op":"transform_item","scene":1,"item":3,"action":"stretch_to_canvas"}}),
        json!({"request":"render","path":ppm,"width":16,"height":16}),
        json!({"request":"save","path":native}),
        json!({"request":"close"}),
    ]);
    assert!(ok, "{replies:?}");
    let bytes = fs::read(ppm).unwrap();
    let prefix = b"P6\n16 16\n255\n";
    assert!(bytes.starts_with(prefix));
    for y in 0..16 {
        for x in 0..16 {
            let region = usize::from(x >= 8) + usize::from(y >= 8) * 2;
            let i = prefix.len() + (y * 16 + x) * 3;
            assert_eq!(&bytes[i..i + 3], &colors[region]);
        }
    }
    write_bmp(&image, [[255, 0, 255]; 4]);
    let rejected = temp.0.join("stale.ppm");
    let (ok, replies) = protocol(&[
        json!({"request":"load","path":native}),
        json!({"request":"render","path":rejected,"width":16,"height":16}),
    ]);
    assert!(!ok);
    assert_eq!(replies[1]["error"]["code"], "image_resources_unavailable");
    assert!(
        replies[1]["error"]["detail"]
            .as_str()
            .unwrap()
            .contains("image_changed")
    );
    assert!(!rejected.exists());
    let updated = temp.0.join("reloaded.ppm");
    let (ok, replies) = protocol(&[
        json!({"request":"load","path":native}),
        json!({"request":"reload_image","source":2,"path":image}),
        json!({"request":"render","path":updated,"width":16,"height":16}),
        json!({"request":"save","path":native}),
        json!({"request":"close"}),
    ]);
    assert!(ok, "{replies:?}");
    assert_eq!(
        &fs::read(updated).unwrap()[prefix.len()..],
        &[255, 0, 255].repeat(16 * 16)
    );
    fs::remove_file(image).unwrap();
    let missing = temp.0.join("missing.ppm");
    let (ok, replies) = protocol(&[
        json!({"request":"load","path":native}),
        json!({"request":"render","path":missing,"width":16,"height":16}),
    ]);
    assert!(!ok);
    assert!(
        replies[1]["error"]["detail"]
            .as_str()
            .unwrap()
            .contains("image_io")
    );
    assert!(!missing.exists());
}
#[test]
fn obs_image_relative_file_import_uses_the_shared_collection_base() {
    let temp = Temp::new();
    let image = temp.0.join("tiny.bmp");
    write_bmp(&image, [[255, 0, 0]; 4]);
    let mut fixture: Value = serde_json::from_slice(
        &fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../crates/host/tests/fixtures/obs-image-absolute.json"),
        )
        .unwrap(),
    )
    .unwrap();
    fixture["sources"][0]["settings"]["file"] = "tiny.bmp".into();
    fixture["sources"][1]["settings"]["items"][0]["crop_left"] = 0.into();
    fixture["sources"][1]["settings"]["items"][0]["crop_right"] = 0.into();
    let collection = temp.0.join("collection.json");
    fs::write(&collection, serde_json::to_vec(&fixture).unwrap()).unwrap();
    let saved = temp.0.join("saved.json");
    let (ok, replies) = protocol(&[
        json!({"request":"import_obs","path":collection,"width":1920,"height":1080,"fps_num":30,"fps_den":1}),
        json!({"request":"save","path":saved}),
        json!({"request":"close"}),
    ]);
    assert!(ok, "{replies:?}");
    let sources = &replies[0]["result"]["collection"]["sources"];
    assert_eq!(sources[0]["kind"]["kind"], "image");
    assert!(
        std::path::Path::new(sources[0]["kind"]["asset"]["path"].as_str().unwrap()).is_absolute()
    );
}
#[test]
fn affine_commands_render_real_pixels_and_survive_native_reopen() {
    let dir = Temp::new();
    let path = dir.0.join("affine.json");
    let first = dir.0.join("first.ppm");
    let rotated = dir.0.join("rotated.ppm");
    let (ok, replies) = protocol(&[
        json!({"request":"command","command":{"op":"set_video","width":16,"height":16,"fps_num":30,"fps_den":1}}),
        json!({"request":"command","command":{"op":"add_color_source","scene":1,"name":"Red","rgba":[255,0,0,255]}}),
        json!({"request":"command","command":{"op":"set_color_size","source":2,"width":4,"height":2}}),
        json!({"request":"command","command":{"op":"set_transform","scene":1,"item":3,"transform":{"x":8,"y":8,"width":8,"height":2,"opacity":1,"rotation_degrees":90,"flip_x":true,"flip_y":false,"crop":{"left":1,"right":1,"top":0,"bottom":0}}}}),
        json!({"request":"render","path":first,"width":16,"height":16}),
        json!({"request":"command","command":{"op":"transform_item","scene":1,"item":3,"action":"rotate_90_cw"}}),
        json!({"request":"render","path":rotated,"width":16,"height":16}),
        json!({"request":"command","command":{"op":"undo"}}),
        json!({"request":"save","path":path}),
        json!({"request":"close"}),
    ]);
    assert!(ok, "{replies:?}");
    for (path, (x0, y0, x1, y1)) in [(first, (6, 4, 8, 8)), (rotated, (6, 4, 10, 6))] {
        let bytes = fs::read(path).unwrap();
        let prefix = b"P6\n16 16\n255\n";
        assert!(bytes.starts_with(prefix));
        let pixels = &bytes[prefix.len()..];
        for y in 0..16 {
            for x in 0..16 {
                let expected = if (x0..x1).contains(&x) && (y0..y1).contains(&y) {
                    [255, 0, 0]
                } else {
                    [0, 0, 0]
                };
                assert_eq!(
                    &pixels[(y * 16 + x) * 3..(y * 16 + x + 1) * 3],
                    &expected,
                    "pixel {x},{y}"
                );
            }
        }
    }
    let (ok, replies) = protocol(&[json!({"request":"load","path":path})]);
    assert!(ok);
    let transform = &replies[0]["result"]["collection"]["scenes"][0]["items"][0]["transform"];
    assert_eq!(transform["rotation_degrees"], 90.0);
    assert_eq!(transform["flip_x"], true);
    assert_eq!(transform["crop"]["left"], 1);
}
#[test]
fn edit_render_save_and_reopen_through_real_cli() {
    let dir = Temp::new();
    let path = dir.0.join("show.json");
    let ppm = dir.0.join("preview.ppm");
    let (ok, responses) = protocol(&[
        json!({"request":"command","command":{"op":"add_color_source","scene":1,"name":"Red","rgba":[255,0,0,255]}}),
        json!({"request":"render","path":ppm,"width":2,"height":1}),
        json!({"request":"save","path":path}),
        json!({"request":"close"}),
    ]);
    assert!(ok, "{responses:?}");
    assert_eq!(responses.len(), 4);
    assert_eq!(
        fs::read(ppm).unwrap(),
        b"P6\n2 1\n255\n\xff\x00\x00\xff\x00\x00"
    );
    let (ok, responses) = protocol(&[
        json!({"request":"load","path":path}),
        json!({"request":"snapshot"}),
    ]);
    assert!(ok);
    assert_eq!(
        responses[1]["result"]["collection"]["sources"][0]["name"],
        "Red"
    );
    assert_eq!(responses[1]["result"]["dirty"], false);
}
#[test]
fn invalid_and_conflicting_commands_fail_nonzero() {
    let (ok, responses) = protocol(&[
        json!({"request":"command","expected_revision":9,"command":{"op":"add_scene","name":"Second"}}),
    ]);
    assert!(!ok);
    assert_eq!(responses[0]["ok"], false);
    assert_eq!(responses[0]["error"]["code"], "revision_conflict");
    assert!(
        responses[0]["error"]["detail"]
            .as_str()
            .unwrap()
            .contains("expected revision")
    );
    let (ok, responses) =
        protocol(&[json!({"request":"command","command":{"op":"remove_scene","scene":1}})]);
    assert!(!ok);
    assert_eq!(responses[0]["ok"], false);
}
#[test]
fn close_and_recovery_cannot_discard_dirty_state() {
    let (ok, responses) = protocol(&[
        json!({"request":"command","command":{"op":"add_scene","name":"Second"}}),
        json!({"request":"close"}),
    ]);
    assert!(!ok);
    assert_eq!(responses[0]["ok"], true);
    assert_eq!(responses[1]["ok"], false);
    let dir = Temp::new();
    let path = dir.0.join("show.json");
    let (ok, _) = protocol(&[
        json!({"request":"command","command":{"op":"rename_collection","name":"Recovery"}}),
        json!({"request":"checkpoint","path":path}),
    ]);
    assert!(ok);
    let (ok, responses) = protocol(&[json!({"request":"recover","path":path})]);
    assert!(ok);
    assert_eq!(responses[0]["result"]["collection"]["name"], "Recovery");
    assert_eq!(responses[0]["result"]["dirty"], true);
}
#[test]
fn obs_subset_import_export_preserves_external_identity_through_cli() {
    let dir = Temp::new();
    let path = dir.0.join("native.json");
    let export = dir.0.join("obs.json");
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../crates/host/tests/fixtures/obs-color-absolute.json");
    let (ok, responses) = protocol(&[
        json!({"request":"import_obs","path":fixture,"width":1920,"height":1080,"fps_num":30,"fps_den":1}),
        json!({"request":"export_obs","path":export}),
        json!({"request":"save","path":path}),
        json!({"request":"close"}),
    ]);
    assert!(ok, "{responses:?}");
    let original: Value = serde_json::from_slice(&fs::read(fixture).unwrap()).unwrap();
    let exported: Value = serde_json::from_slice(&fs::read(export).unwrap()).unwrap();
    assert_eq!(
        original["sources"][0]["uuid"],
        exported["sources"][0]["uuid"]
    );
    assert_eq!(original["scene_order"], exported["scene_order"]);
}

#[test]
fn profile_crud_preserves_scene_graph_global_preferences_and_native_scopes_through_cli() {
    let temp = Temp::new();
    let saved = temp.0.join("profiles.json");
    let mut cli = LiveProtocol::new();
    cli.good(json!({"request":"command","command":{"op":"add_scene","name":"Keep scene"}}));
    let scenes = cli.good(json!({"request":"snapshot"}))["collection"]["scenes"].clone();
    cli.good(json!({"request":"command","command":{"op":"set_audio_settings","settings":{"sample_rate":44100,"speakers":"mono","buffering":{"max_ms":20,"fixed":true}}}}));
    cli.good(json!({"request":"command","command":{"op":"profile","command":{"op":"duplicate","name":"Duplicate"}}}));
    let copy = cli.good(json!({"request":"snapshot"}));
    assert_eq!(copy["profiles"]["selected"], 2);
    assert_eq!(copy["collection"]["audio"]["sample_rate"], 44100);
    cli.good(json!({"request":"command","command":{"op":"profile","command":{"op":"new","name":"Fresh"}}}));
    let fresh = cli.good(json!({"request":"snapshot"}));
    assert_eq!(fresh["profiles"]["selected"], 3);
    assert_eq!(fresh["collection"]["audio"]["sample_rate"], 48000);
    assert_eq!(
        fresh["collection"]["audio"]["buffering"],
        json!({"max_ms":20,"fixed":true})
    );
    assert_eq!(fresh["collection"]["scenes"], scenes);
    cli.good(json!({"request":"command","command":{"op":"profile","command":{"op":"rename","id":2,"name":"  A copy\t"}}}));
    cli.good(
        json!({"request":"command","command":{"op":"profile","command":{"op":"remove","id":3}}}),
    );
    let removed = cli.good(json!({"request":"snapshot"}));
    assert_eq!(removed["profiles"]["selected"], 2);
    assert_eq!(removed["profiles"]["profiles"][1]["name"], "A copy");
    cli.good(json!({"request":"command","command":{"op":"undo"}}));
    let undone = cli.good(json!({"request":"snapshot"}));
    assert_eq!(undone["collection"]["scenes"].as_array().unwrap().len(), 1);
    assert_eq!(undone["profiles"], removed["profiles"]);
    assert_eq!(undone["preferences"], removed["preferences"]);
    cli.good(json!({"request":"command","command":{"op":"redo"}}));
    cli.good(json!({"request":"save","path":saved}));
    cli.good(json!({"request":"new"}));
    let new = cli.good(json!({"request":"snapshot"}));
    assert_eq!(new["profiles"], removed["profiles"]);
    assert_eq!(new["preferences"], removed["preferences"]);
    assert_eq!(new["collection"]["scenes"].as_array().unwrap().len(), 1);
    cli.good(json!({"request":"load","path":saved}));
    let reopened = cli.good(json!({"request":"snapshot"}));
    assert_eq!(reopened["profiles"], removed["profiles"]);
    assert_eq!(reopened["collection"]["scenes"], scenes);
    let inactive = cli.good(json!({"request":"read_native_audio_program"}));
    assert_eq!(inactive["native_capture_requested"], false);
    cli.good(json!({"request":"close"}));
    assert!(cli.finish());
    let document: Value = serde_json::from_slice(&fs::read(saved).unwrap()).unwrap();
    assert_eq!(document["schema"], skylive_host::storage::CURRENT_SCHEMA);
    assert!(document["collection"].get("audio").is_none());
    assert!(document["collection"].get("video").is_none());
    assert_eq!(document["profiles"], removed["profiles"]);
}

#[test]
fn third_party_plugins_default_off_bundled_default_on_pan_pcm_notes_and_native_reopen_share_the_actual_engine()
 {
    let temp = Temp::new();
    let saved = temp.0.join("plugins.json");
    let mut cli = LiveProtocol::new();
    let catalog = cli.good(json!({"request":"plugins"}));
    assert_eq!(catalog["providers"].as_array().unwrap().len(), 145);
    assert_eq!(
        catalog["providers"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|p| p["descriptor"]["origin"] == "bundled")
            .count(),
        2
    );
    assert!(
        catalog["providers"]
            .as_array()
            .unwrap()
            .iter()
            .all(|p| p["enabled"]
                == (p["descriptor"]["implemented"] == true
                    && p["descriptor"]["origin"] != "third_party"))
    );
    assert_eq!(catalog["native_binary_loading"], false);
    assert_eq!(catalog["obs_binary_compatible"], false);
    cli.good(json!({"request":"command","command":{"op":"add_audio_capture_source","scene":1,"name":"Reference PCM","binding":microphone_binding()}}));
    let pan = json!({"kind":"pan","pan_left":100.0,"pan_right":-100.0,"gain_left_db":0.0,"gain_right_db":0.0,"invert_left":false,"invert_right":false});
    cli.good(json!({"request":"command","command":{"op":"set_plugin_enabled","plugin":"norihiro/obs-audio-pan-filter","enabled":true}}));
    cli.good(json!({"request":"command","command":{"op":"set_source_audio_filters","source":2,"filters":[pan]}}));
    let mix = json!({"request":"mix_program_audio","sample_rate":48000,"channels":2,"frames":1,"inputs":[{"kind":"samples","source":2,"sample_rate":48000,"channels":2,"samples":[0.25,0.75]}]});
    let output = cli.good(mix.clone());
    let samples = output["mixed"]["samples"].as_array().unwrap();
    assert!((samples[0].as_f64().unwrap() - 0.75).abs() < 0.000001);
    assert!((samples[1].as_f64().unwrap() - 0.25).abs() < 0.000001);
    cli.good(json!({"request":"command","command":{"op":"set_plugin_enabled","plugin":"norihiro/obs-audio-pan-filter","enabled":false}}));
    assert_eq!(cli.good(mix)["mixed"]["samples"], json!([0.25, 0.75]));
    cli.good(json!({"request":"command","command":{"op":"set_plugin_enabled","plugin":"exeldro/obs-scene-notes-dock","enabled":true}}));
    cli.good(json!({"request":"command","command":{"op":"set_scene_notes","scene":1,"text":"Cue 1\nПривіт"}}));
    cli.good(json!({"request":"command","command":{"op":"set_plugin_enabled","plugin":"exeldro/obs-scene-notes-dock","enabled":false}}));
    cli.good(json!({"request":"save","path":saved}));
    cli.good(json!({"request":"load","path":saved}));
    let current = cli.good(json!({"request":"snapshot"}));
    assert_eq!(current["preferences"]["enabled_plugins"], json!([]));
    assert_eq!(current["collection"]["scenes"][0]["notes"], "Cue 1\nПривіт");
    assert_eq!(current["collection"]["sources"][0]["audio_filters"][0], pan);
    assert_eq!(
        cli.good(json!({"request":"read_native_audio_program"}))["native_capture_requested"],
        false
    );
    cli.good(json!({"request":"close"}));
    assert!(cli.finish());
    let native: Value = serde_json::from_slice(&fs::read(saved).unwrap()).unwrap();
    assert_eq!(
        native["collection"]["sources"][0]["audio_filters"][0]["kind"],
        "pan"
    );
}
#[test]
fn unported_plugin_activation_and_disabled_configuration_fail_without_publishing() {
    for command in [
        json!({"op":"set_plugin_enabled","plugin":"norihiro/obs-mute-filter","enabled":true}),
        json!({"op":"set_scene_notes","scene":1,"text":"Disabled"}),
    ] {
        let (ok, replies) = protocol(&[
            json!({"request":"plugins"}),
            json!({"request":"snapshot"}),
            json!({"request":"command","command":command}),
        ]);
        assert!(!ok);
        assert_eq!(replies.len(), 3);
        assert_eq!(replies[2]["error"]["code"], "unavailable");
        assert_eq!(
            replies[1]["result"]["preferences"]["enabled_plugins"],
            json!([])
        );
        let mute = replies[0]["result"]["providers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["descriptor"]["id"] == "norihiro/obs-mute-filter")
            .unwrap();
        assert_eq!(mute["descriptor"]["implemented"], false);
        assert_eq!(mute["enabled"], false);
    }
}

#[test]
fn localization_external_csv_language_save_reopen_and_failure_are_transactional() {
    let temp = Temp::new();
    let table = temp.0.join("translations.csv");
    fs::write(&table,"key,en-US,ru-RU\nSettings,Settings,Настройки теста\nSkyLive.LanguageName,English,Русский\n").unwrap();
    let saved = temp.0.join("language.json");
    let mut cli = LiveProtocol::new();
    let result = cli
        .good(json!({"request":"localization","path":table,"language":"ru-RU","key":"Settings"}));
    assert_eq!(result["text"], "Настройки теста");
    assert_eq!(result["native_capture"], false);
    cli.good(json!({"request":"set_language","language":"ru-RU","path":table}));
    cli.good(json!({"request":"save","path":saved}));
    cli.good(json!({"request":"set_language","language":"de-DE"}));
    cli.good(json!({"request":"save","path":temp.0.join("alternate-language.json")}));
    cli.good(json!({"request":"load","path":saved}));
    assert_eq!(
        cli.good(json!({"request":"snapshot"}))["preferences"]["language"],
        "ru-RU"
    );
    cli.good(json!({"request":"close"}));
    assert!(cli.finish());
    fs::write(
        &table,
        "key,en-US,ru-RU\nA,English,Русский\nA,Duplicate,Дубликат\n",
    )
    .unwrap();
    let (ok, replies) =
        protocol(&[json!({"request":"set_language","language":"ru-RU","path":table})]);
    assert!(!ok);
    assert_eq!(replies[0]["ok"], false);
    assert_eq!(replies[0]["error"]["phase"], "before_publication");
    let native: Value = serde_json::from_slice(&fs::read(saved).unwrap()).unwrap();
    assert_eq!(native["preferences"]["language"], "ru-RU");
}

#[test]
fn every_imported_language_loads_through_the_real_cli_without_device_access() {
    let mut cli = LiveProtocol::new();
    let catalog = cli.good(json!({"request":"localization"}));
    let languages = catalog["languages"].as_array().unwrap();
    assert!(languages.len() >= 77);
    for language in languages {
        let value=cli.good(json!({"request":"localization","language":language["code"],"key":"Basic.Main.StartStreaming"}));
        assert!(value["text"].as_str().is_some_and(|text| !text.is_empty()));
        assert_eq!(value["native_capture"], false);
    }
    cli.good(json!({"request":"close"}));
    assert!(cli.finish());
}

#[test]
fn private_group_commands_render_and_native_reopen_then_ungroup_with_one_undo() {
    let temp = Temp::new();
    let native = temp.0.join("groups-native.json");
    let grouped = temp.0.join("grouped.ppm");
    let reopened = temp.0.join("reopened.ppm");
    let ungrouped = temp.0.join("ungrouped.ppm");
    let mut cli = LiveProtocol::new();
    cli.good(json!({"request":"command","command":{"op":"set_video","width":16,"height":16,"fps_num":30,"fps_den":1}}));
    let mut items = vec![];
    for (name, rgba, x) in [("Red", [255, 0, 0, 255], 2), ("Blue", [0, 0, 255, 255], 8)] {
        cli.good(json!({"request":"command","command":{"op":"add_color_source","scene":1,"name":name,"rgba":rgba}}));
        let snapshot = cli.good(json!({"request":"snapshot"}));
        let source = snapshot["collection"]["sources"]
            .as_array()
            .unwrap()
            .iter()
            .find(|source| source["name"] == name)
            .unwrap()["id"]
            .clone();
        let item = snapshot["collection"]["scenes"][0]["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["source"] == source)
            .unwrap()["id"]
            .clone();
        cli.good(json!({"request":"command","command":{"op":"set_color_size","source":source,"width":4,"height":2}}));
        cli.good(json!({"request":"command","command":{"op":"set_transform","scene":1,"item":item,"transform":{"x":x,"y":2,"width":4,"height":2,"opacity":1}}}));
        items.push(item);
    }
    cli.good(json!({"request":"command","command":{"op":"group_items","scene":1,"name":"Group","items":items}}));
    let snapshot = cli.good(json!({"request":"snapshot"}));
    assert_eq!(
        snapshot["collection"]["scenes"].as_array().unwrap().len(),
        1
    );
    assert_eq!(
        snapshot["collection"]["groups"].as_array().unwrap().len(),
        1
    );
    let group_item = snapshot["collection"]["scenes"][0]["items"][0]["id"].clone();
    let private = snapshot["collection"]["groups"][0]["id"].clone();
    assert_ne!(private, json!(1));
    assert_eq!(
        snapshot["collection"]["groups"][0]["items"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    cli.good(json!({"request":"render","path":grouped,"width":16,"height":16}));
    cli.good(json!({"request":"save","path":native}));
    cli.good(json!({"request":"load","path":native}));
    cli.good(json!({"request":"render","path":reopened,"width":16,"height":16}));
    cli.good(json!({"request":"command","command":{"op":"ungroup","scene":1,"item":group_item}}));
    cli.good(json!({"request":"render","path":ungrouped,"width":16,"height":16}));
    let after = cli.good(json!({"request":"snapshot"}));
    assert!(
        after["collection"]
            .get("groups")
            .and_then(Value::as_array)
            .is_none_or(Vec::is_empty)
    );
    cli.good(json!({"request":"command","command":{"op":"undo"}}));
    let restored = cli.good(json!({"request":"snapshot"}));
    assert_eq!(
        restored["collection"]["groups"],
        snapshot["collection"]["groups"]
    );
    assert_eq!(
        restored["collection"]["scenes"],
        snapshot["collection"]["scenes"]
    );
    cli.good(json!({"request":"save","path":native}));
    cli.good(json!({"request":"close"}));
    assert!(cli.finish());
    let expected: Vec<[u8; 3]> = (0..16)
        .flat_map(|y| {
            (0..16).map(move |x| {
                if (2..4).contains(&y) && (2..6).contains(&x) {
                    [255, 0, 0]
                } else if (2..4).contains(&y) && (8..12).contains(&x) {
                    [0, 0, 255]
                } else {
                    [0, 0, 0]
                }
            })
        })
        .collect();
    for path in [&grouped, &reopened, &ungrouped] {
        assert_ppm(path, 16, 16, &expected);
    }
}

#[test]
fn recording_profile_settings_round_trip_duplicate_and_stay_outside_scene_undo() {
    let temp = Temp::new();
    let native = temp.0.join("recording-profile.json");
    let mut cli = LiveProtocol::new();
    let before = cli.good(json!({"request":"snapshot"}));
    let id = before["profiles"]["selected"].clone();
    cli.good(json!({"request":"command","command":{"op":"add_color_source","scene":1,"name":"Scene history","rgba":[1,2,3,255]}}));
    let settings = json!({"directory":temp.0.join("Записи"),"video_bitrate_kbps":12000,"audio_bitrate_kbps":192,"audio_enabled":false,"prefer_hardware":false});
    cli.good(json!({"request":"command","command":{"op":"profile","command":{"op":"set_recording","id":id,"settings":settings}}}));
    cli.good(json!({"request":"command","command":{"op":"undo"}}));
    let undone = cli.good(json!({"request":"snapshot"}));
    assert!(
        undone["collection"]["scenes"][0]["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(undone["profiles"]["profiles"][0]["recording"], settings);
    cli.good(json!({"request":"command","command":{"op":"redo"}}));
    cli.good(json!({"request":"command","command":{"op":"profile","command":{"op":"duplicate","name":"Recording copy"}}}));
    let duplicated = cli.good(json!({"request":"snapshot"}));
    let selected = &duplicated["profiles"]["selected"];
    let profile = duplicated["profiles"]["profiles"]
        .as_array()
        .unwrap()
        .iter()
        .find(|profile| &profile["id"] == selected)
        .unwrap();
    assert_eq!(profile["recording"], settings);
    cli.good(json!({"request":"save","path":native}));
    cli.good(json!({"request":"load","path":native}));
    let loaded = cli.good(json!({"request":"snapshot"}));
    assert_eq!(loaded["profiles"], duplicated["profiles"]);
    cli.good(json!({"request":"close"}));
    assert!(cli.finish());
}

#[test]
fn recording_requires_explicit_start_and_supported_file_before_any_native_open() {
    let temp = Temp::new();
    let unsupported = temp.0.join("unsupported.mov");
    let (ok, replies) = protocol(&[json!({"request":"record_frame","time_ns":0})]);
    assert!(!ok);
    assert_eq!(replies[0]["ok"], false);
    assert!(
        replies[0]["error"]["detail"]
            .as_str()
            .unwrap()
            .contains("start recording")
    );
    let (ok, replies) = protocol(&[
        json!({"request":"start_recording","path":unsupported,"time_ns":0,"audio":false}),
    ]);
    assert!(!ok);
    assert_eq!(replies[0]["error"]["code"], "recording_invalid");
    assert!(!unsupported.exists());
    assert_eq!(fs::read_dir(&temp.0).unwrap().count(), 0);
}

#[test]
fn hotkey_preferences_roundtrip_without_native_start_or_scene_history_changes() {
    let temp = Temp::new();
    let path = temp.0.join("shortcut-preferences.json");
    let mut cli = LiveProtocol::new();
    cli.good(json!({"request":"command","command":{"op":"add_color_source","scene":1,"name":"Scene history","rgba":[3,4,5,255]}}));
    let bindings = json!([{"action":"start_recording","chord":{"key":"F9","primary":false,"alt":false,"shift":false}}]);
    cli.good(json!({"request":"command","command":{"op":"set_hotkeys","hotkeys":bindings}}));
    let before = cli.good(json!({"request":"snapshot"}));
    assert_eq!(before["preferences"]["hotkeys"], bindings);
    cli.good(json!({"request":"command","command":{"op":"undo"}}));
    let undone = cli.good(json!({"request":"snapshot"}));
    assert_eq!(undone["preferences"]["hotkeys"], bindings);
    assert!(
        undone["collection"]["scenes"][0]["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    cli.good(json!({"request":"save","path":path}));
    cli.good(json!({"request":"close"}));
    assert!(cli.finish());
    let mut cli = LiveProtocol::new();
    cli.good(json!({"request":"load","path":path}));
    assert_eq!(
        cli.good(json!({"request":"snapshot"}))["preferences"]["hotkeys"],
        bindings
    );
    let status = cli.good(json!({"request":"recording_status"}));
    assert_eq!(
        status["status"]["active"], false,
        "a shortcut assignment is inert metadata in CLI"
    );
    cli.good(json!({"request":"close"}));
    assert!(cli.finish());
}

#[test]
fn real_obs_keyer_commands_pixels_undo_native_and_strict_export_cross_cli_boundary() {
    let temp = Temp::new();
    for (kind, expected, provider) in [
        ("color_key", [0, 146, 0], "color_key_filter"),
        ("chroma_key", [0, 0, 0], "chroma_key_filter"),
    ] {
        let native = temp.0.join(format!("{kind}.json"));
        let obs = temp.0.join(format!("{kind}-obs.json"));
        let image = temp.0.join(format!("{kind}.ppm"));
        let clear = temp.0.join(format!("{kind}-clear.ppm"));
        let restored = temp.0.join(format!("{kind}-restored.ppm"));
        let mut effect = json!({"kind":kind,"key_color_type":"green","key_color":[12,34,56],"similarity":80,"smoothness":50,"opacity":1.0,"contrast":0.0,"brightness":0.0,"gamma":0.0});
        if kind == "chroma_key" {
            effect["similarity"] = json!(400);
            effect["smoothness"] = json!(80);
            effect["spill"] = json!(100);
        }
        let mut cli = LiveProtocol::new();
        cli.good(json!({"request":"command","command":{"op":"set_video","width":16,"height":16,"fps_num":30,"fps_den":1}}));
        cli.good(json!({"request":"command","command":{"op":"add_color_source","scene":1,"name":"Green","rgba":[0,230,0,255]}}));
        cli.good(json!({"request":"command","command":{"op":"set_source_video_filters","source":2,"filters":[{"name":"Мій ключ","enabled":true,"effect":effect}]}}));
        let filters =
            cli.good(json!({"request":"snapshot"}))["collection"]["sources"][0]["video_filters"]
                .clone();
        cli.good(
            json!({"request":"render_program","path":image,"width":16,"height":16,"time_ns":0}),
        );
        cli.good(json!({"request":"command","command":{"op":"undo"}}));
        cli.good(
            json!({"request":"render_program","path":clear,"width":16,"height":16,"time_ns":0}),
        );
        cli.good(json!({"request":"command","command":{"op":"redo"}}));
        cli.good(json!({"request":"save","path":native}));
        cli.good(json!({"request":"load","path":native}));
        assert_eq!(
            cli.good(json!({"request":"snapshot"}))["collection"]["sources"][0]["video_filters"],
            filters
        );
        cli.good(
            json!({"request":"render_program","path":restored,"width":16,"height":16,"time_ns":0}),
        );
        cli.good(json!({"request":"export_obs","path":obs}));
        cli.good(json!({"request":"close"}));
        assert!(cli.finish());
        assert_ppm(&image, 16, 16, &vec![expected; 256]);
        assert_ppm(&clear, 16, 16, &vec![[0, 230, 0]; 256]);
        assert_eq!(fs::read(image).unwrap(), fs::read(restored).unwrap());
        let value: Value = serde_json::from_slice(&fs::read(obs).unwrap()).unwrap();
        let filter = &value["sources"][0]["filters"][0];
        assert_eq!(filter["id"], provider);
        assert_eq!(filter["versioned_id"], format!("{provider}_v2"));
        assert_eq!(filter["name"], "Мій ключ");
        assert_eq!(filter["settings"]["key_color"], 0x0038220cu32);
    }
}

#[test]
fn scene_root_filter_migration_report_and_current_schema_pixels_cross_actual_cli() {
    let temp = Temp::new();
    let current = temp.0.join("current.json");
    let legacy = temp.0.join("old3.json");
    let saved = temp.0.join("new4.json");
    let ppm = temp.0.join("scene.ppm");
    let mut cli = LiveProtocol::new();
    let snapshot = cli.good(json!({"request":"snapshot"}));
    let scene = snapshot["preview"].clone();
    cli.good(json!({"request":"command","command":{"op":"set_video","width":16,"height":16,"fps_num":30,"fps_den":1}}));
    cli.good(json!({"request":"command","command":{"op":"add_color_source","scene":scene,"name":"White","rgba":[255,255,255,255]}}));
    let filter = json!({"enabled":true,"effect":{"kind":"color_correction","gamma":0.0,"contrast":0.0,"brightness":0.0,"saturation":0.0,"hue_shift":0.0,"opacity":0.5,"color_multiply":[255,255,255],"color_add":[0,0,0]}});
    cli.good(json!({"request":"command","command":{"op":"set_scene_video_filters","scene":scene,"filters":[filter]}}));
    let expected_collection = cli.good(json!({"request":"snapshot"}))["collection"].clone();
    cli.good(json!({"request":"save","path":current}));
    let mut native: Value = serde_json::from_slice(&fs::read(&current).unwrap()).unwrap();
    assert_eq!(native["schema"], skylive_host::storage::CURRENT_SCHEMA);
    native["schema"] = json!(3);
    fs::write(&legacy, serde_json::to_vec(&native).unwrap()).unwrap();
    let loaded = cli.good(json!({"request":"load","path":legacy}));
    assert_eq!(loaded["dirty"], true);
    assert_eq!(loaded["migration"]["from_schema"], 3);
    assert_eq!(
        loaded["migration"]["to_schema"],
        skylive_host::storage::CURRENT_SCHEMA
    );
    assert_eq!(
        loaded["migration"]["scene_source_filters"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        loaded["migration"]["scene_source_filters"][0]["scene"],
        scene
    );
    // Compare the exact same complete protocol DTO; the native envelope stores
    // revision and selected profile formats separately from collection.
    assert_eq!(
        loaded["collection"], expected_collection,
        "no forged canonical edit or ID migration"
    );
    cli.good(json!({"request":"render_program","path":ppm,"width":16,"height":16,"time_ns":0}));
    cli.good(json!({"request":"save","path":saved}));
    let loaded = cli.good(json!({"request":"load","path":saved}));
    assert_eq!(loaded["dirty"], false);
    assert_eq!(loaded["migration"]["scene_source_filters"], json!([]));
    cli.good(json!({"request":"close"}));
    assert!(cli.finish());
    assert_ppm(&ppm, 16, 16, &vec![[188; 3]; 256]);
}

#[path = "support/streaming_settings.rs"]
mod streaming_settings;

#[cfg(target_os = "windows")]
#[path = "support/streaming_native_protocol_tests.rs"]
mod streaming_native_protocol_tests;
#[path = "support/streaming_protocol_tests.rs"]
mod streaming_protocol_tests;

#[path = "support/obs_dynamics_protocol_tests.rs"]
mod obs_dynamics_protocol_tests;

#[path = "support/watermark_protocol_tests.rs"]
mod watermark_protocol_tests;

#[test]
fn atomic_filter_paste_uses_shared_cli_command_fresh_video_ids_undo_and_native_reopen() {
    let temp = Temp::new();
    let saved = temp.0.join("clipboard.json");
    let mut cli = LiveProtocol::new();
    let initial = cli.good(json!({"request":"snapshot"}));
    let scene = initial["collection"]["scenes"][0]["id"].clone();
    for name in ["Clipboard source", "Clipboard destination"] {
        cli.good(json!({"request":"command","command":{"op":"add_color_source_sized","scene":scene,"name":name,"rgba":[255,255,255,255],"width":16,"height":16}}));
    }
    let initial = cli.good(json!({"request":"snapshot"}));
    let from = initial["collection"]["sources"][0]["id"].clone();
    let to = initial["collection"]["sources"][1]["id"].clone();
    let filter = skylive_core::VideoFilter::new(skylive_core::VideoEffect::color_correction())
        .unwrap()
        .with_name("Copied effect".into())
        .unwrap();
    cli.good(json!({"request":"command","command":{"op":"set_source_video_filters","source":from,"filters":[filter]}}));
    let before = cli.good(json!({"request":"snapshot"}));
    let old = before["collection"]["sources"][0]["video_filters"][0]["id"].clone();
    let before_revision = before["collection"]["revision"].as_u64().unwrap();
    cli.good(json!({"request":"command","expected_revision":before_revision,"command":{"op":"paste_source_filters","from":from,"to":to}}));
    let pasted = cli.good(json!({"request":"snapshot"}));
    assert_eq!(pasted["collection"]["revision"], before_revision + 1);
    assert_eq!(
        pasted["collection"]["sources"][1]["video_filters"][0]["name"],
        "Copied effect"
    );
    assert_ne!(
        pasted["collection"]["sources"][1]["video_filters"][0]["id"],
        old
    );
    cli.good(json!({"request":"command","command":{"op":"undo"}}));
    let undone = cli.good(json!({"request":"snapshot"}));
    let mut expected_before = before["collection"].clone();
    expected_before["next_id"] = pasted["collection"]["next_id"].clone();
    expected_before["revision"] = json!(before_revision + 2);
    assert_eq!(undone["collection"], expected_before);
    cli.good(json!({"request":"command","command":{"op":"redo"}}));
    let redone = cli.good(json!({"request":"snapshot"}));
    let mut expected_after = pasted["collection"].clone();
    expected_after["revision"] = json!(before_revision + 3);
    assert_eq!(redone["collection"], expected_after);
    cli.good(json!({"request":"save","path":saved}));
    cli.good(json!({"request":"load","path":saved}));
    let reopened = cli.good(json!({"request":"snapshot"}));
    assert_eq!(reopened["collection"], redone["collection"]);
    cli.good(json!({"request":"close"}));
    assert!(cli.finish());
    let native: Value = serde_json::from_slice(&fs::read(saved).unwrap()).unwrap();
    assert_eq!(native["schema"], skylive_host::storage::CURRENT_SCHEMA);
}

#[test]
fn neural_filter_paste_uses_activation_guards_one_history_step_and_current_native() {
    let temp = Temp::new();
    let initial_file = temp.0.join("clipboard-neural-initial.json");
    let disabled_file = temp.0.join("clipboard-neural-disabled.json");
    let pasted_file = temp.0.join("clipboard-neural-pasted.json");
    let mut cli = LiveProtocol::new();
    let first = cli.good(json!({"request":"snapshot"}));
    let scene = first["collection"]["scenes"][0]["id"].clone();
    for name in ["Neural clipboard source", "Neural clipboard destination"] {
        cli.good(json!({"request":"command","command":{"op":"add_color_source_sized","scene":scene,"name":name,"rgba":[255,255,255,255],"width":16,"height":16}}));
    }
    let initial = cli.good(json!({"request":"snapshot"}));
    let from = initial["collection"]["sources"][0]["id"].clone();
    let to = initial["collection"]["sources"][1]["id"].clone();
    let mut filter =
        skylive_core::VideoFilter::new(skylive_core::VideoEffect::PersonBackgroundRemoval {
            threshold: 0.68,
            softness: 0.21,
        })
        .unwrap()
        .with_name("Copied neural settings".into())
        .unwrap();
    filter.enabled = false;
    cli.good(json!({"request":"save","path":initial_file}));
    let rejected = cli.ask(json!({"request":"command","command":{"op":"set_source_video_filters","source":from,"filters":[filter]}}));
    assert_eq!(rejected["ok"], false);
    assert_eq!(rejected["error"]["code"], "unavailable");
    assert!(!cli.finish(), "a rejected request ends this CLI owner");
    // The CLI is deliberately fail-fast. Each rejection is the last request
    // for its owner; reopen the accepted checkpoint in another actual process.
    let mut cli = LiveProtocol::new();
    cli.good(json!({"request":"load","path":initial_file}));
    assert_eq!(
        cli.good(json!({"request":"snapshot"}))["collection"],
        initial["collection"]
    );
    cli.good(json!({"request":"command","command":{"op":"set_plugin_enabled","plugin":skylive_core::PERSON_BACKGROUND_PLUGIN,"enabled":true}}));
    cli.good(json!({"request":"command","command":{"op":"set_source_video_filters","source":from,"filters":[filter]}}));
    cli.good(json!({"request":"command","command":{"op":"set_plugin_enabled","plugin":skylive_core::PERSON_BACKGROUND_PLUGIN,"enabled":false}}));
    let disabled = cli.good(json!({"request":"snapshot"}));
    let source_filter = disabled["collection"]["sources"][0]["video_filters"][0].clone();
    cli.good(json!({"request":"save","path":disabled_file}));
    let rejected = cli.ask(json!({"request":"command","expected_revision":disabled["collection"]["revision"],"command":{"op":"paste_source_filters","from":from,"to":to}}));
    assert_eq!(rejected["ok"], false);
    assert_eq!(rejected["error"]["code"], "unavailable");
    assert!(!cli.finish(), "a rejected paste ends this CLI owner");
    let mut cli = LiveProtocol::new();
    cli.good(json!({"request":"load","path":disabled_file}));
    assert_eq!(
        cli.good(json!({"request":"snapshot"}))["collection"],
        disabled["collection"]
    );
    let reopened = cli.good(json!({"request":"snapshot"}));
    assert_eq!(reopened["collection"], disabled["collection"]);
    assert_eq!(reopened["preferences"], disabled["preferences"]);
    cli.good(json!({"request":"command","command":{"op":"set_plugin_enabled","plugin":skylive_core::PERSON_BACKGROUND_PLUGIN,"enabled":true}}));
    let before = cli.good(json!({"request":"snapshot"}));
    let revision = before["collection"]["revision"].as_u64().unwrap();
    cli.good(json!({"request":"command","expected_revision":revision,"command":{"op":"paste_source_filters","from":from,"to":to}}));
    let pasted = cli.good(json!({"request":"snapshot"}));
    let copied = &pasted["collection"]["sources"][1]["video_filters"][0];
    assert_eq!(copied["effect"], source_filter["effect"]);
    assert_eq!(copied["name"], source_filter["name"]);
    assert_eq!(copied["enabled"], false);
    assert_ne!(copied["id"], source_filter["id"]);
    assert_eq!(pasted["collection"]["revision"], revision + 1);
    cli.good(json!({"request":"command","command":{"op":"undo"}}));
    let undone = cli.good(json!({"request":"snapshot"}));
    let mut expected_before = before["collection"].clone();
    expected_before["next_id"] = pasted["collection"]["next_id"].clone();
    expected_before["revision"] = json!(revision + 2);
    assert_eq!(undone["collection"], expected_before);
    cli.good(json!({"request":"command","command":{"op":"redo"}}));
    let redone = cli.good(json!({"request":"snapshot"}));
    let mut expected_after = pasted["collection"].clone();
    expected_after["revision"] = json!(revision + 3);
    assert_eq!(redone["collection"], expected_after);
    cli.good(json!({"request":"save","path":pasted_file}));
    cli.good(json!({"request":"load","path":pasted_file}));
    assert_eq!(
        cli.good(json!({"request":"snapshot"}))["collection"],
        redone["collection"]
    );
    cli.good(json!({"request":"close"}));
    assert!(cli.finish());
    for file in [disabled_file, pasted_file] {
        let native: Value = serde_json::from_slice(&fs::read(file).unwrap()).unwrap();
        assert_eq!(native["schema"], skylive_host::storage::CURRENT_SCHEMA);
    }
}

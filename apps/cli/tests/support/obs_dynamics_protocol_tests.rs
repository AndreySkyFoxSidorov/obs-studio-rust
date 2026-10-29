use super::*;
#[test]
fn real_cli_dynamics_process_literal_pcm_and_native_roundtrip_without_native_capture() {
    let temp = Temp::new();
    let path = temp.0.join("obs-dynamics.json");
    let binding = microphone_binding();
    let mut cli = LiveProtocol::new();
    cli.good(json!({"request":"command","command":{"op":"add_audio_capture_source","scene":1,"name":"CPU-only dynamics","binding":binding}}));
    let chain = json!([{"kind":"obs_expander","threshold_db":-40.0,"ratio":2.0,"attack_ms":10,"release_ms":50,"output_db":0.0,"detector":"peak","preset":"expander","enabled":false},{"kind":"obs_compressor","threshold_db":-18.0,"ratio":10.0,"attack_ms":6,"release_ms":60,"output_db":0.0,"enabled":false},{"kind":"obs_limiter","threshold_db":-6.0,"release_ms":60,"enabled":true}]);
    cli.good(json!({"request":"command","command":{"op":"set_source_audio_filters","source":2,"filters":chain}}));
    cli.good(json!({"request":"command","command":{"op":"set_audio","source":2,"gain":0.5,"muted":false}}));
    let identity = cli.good(json!({"request":"audio_program_reset"}))["identity"].clone();
    cli.good(program_packet(
        &identity,
        &binding,
        1,
        1,
        "0",
        48000,
        json!(vec![1.0; 4096]),
    ));
    cli.good(json!({"request":"audio_program_finish","identity":identity,"binding":binding,"stream_generation":1,"sequence":1}));
    let output = cli.good(json!({"request":"render_audio_program","start_frame":0,"frames":1024}));
    let pcm = output["block"]["samples"].as_array().unwrap();
    assert_eq!(pcm.len(), 2048);
    for sample in pcm {
        assert!((sample.as_f64().unwrap() - 0.2505936168).abs() < 0.000002);
    }
    assert_eq!(output["block"]["gaps"], json!([]));
    assert_eq!(output["native_capture"], false);
    cli.good(json!({"request":"save","path":path}));
    cli.good(json!({"request":"load","path":path}));
    assert_eq!(
        cli.good(json!({"request":"snapshot"}))["collection"]["sources"][0]["audio_filters"],
        chain
    );
    cli.good(json!({"request":"close"}));
    assert!(cli.finish());
    let native: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    assert_eq!(native["collection"]["sources"][0]["audio_filters"], chain);
}

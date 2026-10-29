//! Append/include in CLI protocol tests only after schema5/owner integration.
use super::*;
#[test]
fn authored_custom_rtmp_profile_save_reopen_is_inert_and_exact() {
    let temp = Temp::new();
    let path = temp.0.join("stream-profile.json");
    let mut cli = LiveProtocol::new();
    let settings = json!({"server_url":"rtmp://[::1]:1935/live/raw?auth=a%2Fb","stream_key":"  public/Ключ?token=x%2By  ","video_bitrate_kbps":8000,"keyframe_interval_seconds":2,"audio_enabled":false,"audio_bitrate_kbps":128});
    cli.good(json!({"request":"command","command":{"op":"profile","command":{"op":"set_streaming","id":1,"settings":settings}}}));
    let first = cli.good(json!({"request":"snapshot"}));
    assert_eq!(first["profiles"]["profiles"][0]["streaming"], settings);
    cli.good(json!({"request":"command","command":{"op":"add_scene","name":"Scene history"}}));
    cli.good(json!({"request":"command","command":{"op":"undo"}}));
    assert_eq!(
        cli.good(json!({"request":"snapshot"}))["profiles"]["profiles"][0]["streaming"],
        settings
    );
    cli.good(json!({"request":"save","path":path}));
    cli.good(json!({"request":"load","path":path}));
    assert_eq!(
        cli.good(json!({"request":"snapshot"}))["profiles"]["profiles"][0]["streaming"],
        settings
    );
    cli.good(json!({"request":"close"}));
    assert!(cli.finish());
    let native: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(native["schema"], skylive_host::storage::CURRENT_SCHEMA);
    assert_eq!(native["profiles"]["profiles"][0]["streaming"], settings);
    // No Start request was made; this entire subprocess workflow performs no
    // native encoding, DNS resolution or socket admission.
}
#[test]
fn unsupported_stream_url_rejects_before_any_profile_or_network_publication() {
    let (ok, replies) = protocol(&[
        json!({"request":"command","command":{"op":"profile","command":{"op":"set_streaming","id":1,"settings":{"server_url":"rtmps://127.0.0.1/live","stream_key":"public-test","video_bitrate_kbps":6000,"keyframe_interval_seconds":2,"audio_enabled":true,"audio_bitrate_kbps":160}}}}),
    ]);
    assert!(!ok);
    assert_eq!(replies[0]["ok"], false);
}

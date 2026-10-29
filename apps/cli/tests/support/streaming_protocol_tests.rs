//! Ordinary subprocess contracts perform no native Start/DNS/socket operation.
use super::*;
#[test]
fn streaming_idle_status_is_inert_and_preserves_model_scope() {
    let (ok, replies) = protocol(&[
        json!({"request":"snapshot"}),
        json!({"request":"streaming_status"}),
        json!({"request":"snapshot"}),
        json!({"request":"capabilities"}),
        json!({"request":"close"}),
    ]);
    assert!(ok);
    assert_eq!(replies[0]["result"], replies[2]["result"]);
    let status = &replies[1]["result"];
    assert_eq!(status["active"], false);
    assert!(status["status"].is_null());
    assert_eq!(status["video_schedule"], "caller_driven");
    let cap = &replies[3]["result"]["streaming"];
    assert_eq!(cap["available"], cfg!(target_os = "windows"));
    assert_eq!(cap["rtmps"], false);
    assert_eq!(cap["automatic_reconnect"], false);
    assert_eq!(cap["service_compatibility_verified"], false);
}
#[test]
fn streaming_start_without_authored_server_fails_before_output_admission() {
    let (ok, replies) = protocol(&[
        json!({"request":"streaming_status"}),
        json!({"request":"start_streaming"}),
    ]);
    assert!(!ok);
    assert_eq!(replies[0]["result"]["active"], false);
    assert_eq!(replies[1]["error"]["code"], "streaming_invalid");
    assert_eq!(replies[1]["error"]["phase"], "streaming_output");
}
#[test]
fn streaming_key_is_strict_and_frame_cannot_invent_an_owner() {
    for key in [
        json!({"session":0,"output":1,"epoch":1}),
        json!({"session":1,"output":1,"epoch":1,"unknown":true}),
    ] {
        let (ok, replies) = protocol(&[json!({"request":"stream_frame","key":key})]);
        assert!(!ok);
        assert_eq!(replies[0]["error"]["code"], "invalid_request");
    }
    let (ok, replies) =
        protocol(&[json!({"request":"stream_frame","key":{"session":1,"output":1,"epoch":1}})]);
    assert!(!ok);
    assert_eq!(replies[0]["error"]["code"], "streaming_state");
}
#[test]
fn unsupported_stream_dimensions_reject_before_any_native_network_admission() {
    let settings = json!({"server_url":"rtmp://127.0.0.1:9/live","stream_key":"exact/key?token=abc","video_bitrate_kbps":1000,"keyframe_interval_seconds":1,"audio_enabled":false,"audio_bitrate_kbps":128});
    let (ok, replies) = protocol(&[
        json!({"request":"command","command":{"op":"set_video","width":17,"height":16,"fps_num":30,"fps_den":1}}),
        json!({"request":"command","command":{"op":"profile","command":{"op":"set_streaming","id":1,"settings":settings}}}),
        json!({"request":"start_streaming"}),
    ]);
    assert!(!ok);
    assert_eq!(replies[0]["ok"], true);
    assert_eq!(replies[1]["ok"], true);
    assert_eq!(replies[2]["error"]["code"], "streaming_h264");
}

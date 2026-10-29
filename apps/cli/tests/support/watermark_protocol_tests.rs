use super::*;
#[test]
fn keyed_finish_stream_ports_are_registered_and_inert_without_a_live_owner() {
    for request in ["finish_stream_video", "finish_stream_audio"] {
        let (success, replies) =
            protocol(&[json!({"request":request,"key":{"session":1,"output":1,"epoch":1}})]);
        assert!(!success);
        assert_eq!(replies.len(), 1);
        assert_eq!(replies[0]["ok"], false);
        assert_eq!(replies[0]["error"]["code"], "streaming_state");
        assert_eq!(replies[0]["error"]["phase"], "streaming_output");
    }
}

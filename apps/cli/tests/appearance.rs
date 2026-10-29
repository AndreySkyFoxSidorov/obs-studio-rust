//! Native-free protocol persistence of global UI metadata, not a GUI test.
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    process::{Command, Stdio},
};
#[test]
fn appearance_commands_persist_without_scene_undo_or_output_activation() {
    let path = std::env::temp_dir().join(format!(
        "skylive-cli-appearance-{}.json",
        std::process::id()
    ));
    let requests = [
        json!({"request":"command","command":{"op":"set_appearance","appearance":{"theme":"system","scale_percent":125}}}),
        json!({"request":"snapshot"}),
        json!({"request":"command","command":{"op":"add_color_source","scene":1,"name":"History","rgba":[1,2,3,255]}}),
        json!({"request":"command","command":{"op":"undo"}}),
        json!({"request":"snapshot"}),
        json!({"request":"save","path":path}),
        json!({"request":"load","path":path}),
        json!({"request":"snapshot"}),
        json!({"request":"recording_status"}),
        json!({"request":"close"}),
    ];
    let mut child = Command::new(env!("CARGO_BIN_EXE_skylive-cli"))
        .arg("--stdio")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    {
        let mut input = child.stdin.take().unwrap();
        for request in &requests {
            writeln!(input, "{request}").unwrap();
        }
    }
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let replies: Vec<Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(replies.len(), requests.len());
    for reply in &replies {
        assert_eq!(reply["ok"], true, "{reply}");
    }
    let expected = json!({"theme":"system","scale_percent":125});
    for index in [1, 4, 7] {
        assert_eq!(
            replies[index]["result"]["preferences"]["appearance"],
            expected
        );
    }
    assert!(
        replies[4]["result"]["collection"]["scenes"][0]["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(replies[8]["result"]["status"]["active"], false);
    let saved: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(saved["preferences"]["appearance"], expected);
    fs::remove_file(&path).unwrap();
}

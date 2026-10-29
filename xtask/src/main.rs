//! Headless architecture checks. Every direct edge, including target-specific,
//! optional, development and build dependencies, is subject to the same policy.
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

const PACKAGES: &[&str] = &[
    "skylive-game-capture-protocol",
    "skylive-game-hook",
    "skylive-core",
    "skylive-media",
    "skylive-host",
    "skylive-cli",
    "skylive-desktop",
    "xtask",
    "skylive-codec-bits",
    "skylive-h264",
    "skylive-aac",
    "skylive-mp4",
    "skylive-person-matte",
    "skylive-virtual-camera-protocol",
    "skylive-virtual-camera-source",
    "skylive-virtual-camera-install",
    "skylive-browser-protocol",
    "skylive-browser-helper",
];

fn allowed_dependencies(package: &str) -> Option<&'static [&'static str]> {
    match package {
        "skylive-game-capture-protocol" => Some(&[]),
        "skylive-game-hook" => Some(&["skylive-game-capture-protocol","windows"]),
        "skylive-core" => Some(&["serde"]),
        "skylive-media" => Some(&["rubato"]),
        "skylive-virtual-camera-protocol" => Some(&["windows-core"]),
        "skylive-virtual-camera-source" => Some(&["skylive-virtual-camera-protocol","windows","windows-core"]),
        "skylive-virtual-camera-install" => Some(&["skylive-virtual-camera-protocol"]),
        "skylive-browser-protocol" => Some(&[]),
        "skylive-browser-helper" => Some(&["skylive-browser-protocol", "windows"]),
        "skylive-person-matte" => Some(&["rten", "rten-tensor"]),
        "skylive-codec-bits" => Some(&[]),
        "skylive-h264" | "skylive-aac" | "skylive-mp4" => {
            Some(&["skylive-codec-bits", "thiserror"])
        }
        "skylive-host" => Some(&[
            "skylive-game-capture-protocol",
            "skylive-core",
            "skylive-media",
            "skylive-person-matte",
            "skylive-virtual-camera-protocol",
            "skylive-browser-protocol",
            "skylive-codec-bits",
            "skylive-h264",
            "skylive-aac",
            "skylive-mp4",
            "serde",
            "serde_json",
            "image",
            "gif",
            "sha2",
            "wgpu",
            "cosmic-text",
            "rtrb",
            "windows",
            "windows-core",
            "objc2",
            "block2",
            "objc2-foundation",
            "objc2-core-foundation",
            "objc2-core-graphics",
            "objc2-screen-capture-kit",
            "objc2-core-audio",
            "objc2-core-audio-types",
            "objc2-audio-toolbox",
            "objc2-core-media",
            "objc2-av-foundation",
            "objc2-avf-audio",
            "objc2-core-video",
            "dispatch2",
        ]),
        "skylive-cli" => Some(&[
            "skylive-core",
            "skylive-media",
            "skylive-host",
            "serde",
            "serde_json",
        ]),
        "skylive-desktop" => Some(&[
            "skylive-core",
            "skylive-host",
            "skylive-media",
            "eframe",
            "raw-window-handle",
            "serde",
            "serde_json",
        ]),
        "xtask" => Some(&["serde_json"]),
        _ => None,
    }
}

#[derive(Debug)]
struct Validation {
    packages: usize,
    edges: usize,
    violations: Vec<String>,
}

fn required_string<'a>(value: &'a Value, field: &str) -> Result<&'a str, String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("metadata field {field:?} must be a string"))
}

fn required_array<'a>(value: &'a Value, field: &str) -> Result<&'a Vec<Value>, String> {
    value
        .get(field)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("metadata field {field:?} must be an array"))
}

fn validate(metadata: &Value) -> Result<Validation, String> {
    let members: BTreeSet<&str> = required_array(metadata, "workspace_members")?
        .iter()
        .map(|member| {
            member
                .as_str()
                .ok_or_else(|| "workspace member identity must be a string".to_owned())
        })
        .collect::<Result<_, _>>()?;
    if members.is_empty() {
        return Err("workspace metadata contains no members".to_owned());
    }
    let mut packages = BTreeMap::new();
    let mut names = BTreeMap::new();
    for package in required_array(metadata, "packages")? {
        let id = required_string(package, "id")?;
        if !members.contains(id) {
            continue;
        }
        let name = required_string(package, "name")?;
        let manifest = PathBuf::from(required_string(package, "manifest_path")?);
        let directory = manifest
            .parent()
            .ok_or_else(|| format!("invalid manifest path for {name}"))?
            .to_path_buf();
        if packages.insert(id, package).is_some() || names.insert(name, directory).is_some() {
            return Err(format!("duplicate workspace package identity/name: {name}"));
        }
    }
    if packages.len() != members.len() {
        return Err("workspace member missing from metadata packages".to_owned());
    }
    let mut result = Validation {
        packages: packages.len(),
        edges: 0,
        violations: Vec::new(),
    };
    for package in packages.values() {
        let name = required_string(package, "name")?;
        let Some(allowed) = allowed_dependencies(name) else {
            result
                .violations
                .push(format!("unclassified workspace package: {name}"));
            continue;
        };
        for dependency in required_array(package, "dependencies")? {
            result.edges += 1;
            // Cargo reports the package's real name here even when Cargo.toml
            // renames it. Never apply the allowlist to the alias.
            let dependency_name = required_string(dependency, "name")?;
            let alias = dependency.get("rename").and_then(Value::as_str);
            let kind = dependency
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or("normal");
            let target = dependency
                .get("target")
                .and_then(Value::as_str)
                .unwrap_or("all targets");
            let context = format!(
                "{name} -> {dependency_name} (alias={}, kind={kind}, target={target})",
                alias.unwrap_or(dependency_name)
            );
            if !matches!(kind, "normal" | "dev" | "build") {
                result
                    .violations
                    .push(format!("unknown dependency kind: {context}"));
            }
            if dependency_name == name || !allowed.contains(&dependency_name) {
                result
                    .violations
                    .push(format!("forbidden dependency: {context}"));
                continue;
            }
            if PACKAGES.contains(&dependency_name) {
                match (
                    names.get(dependency_name),
                    dependency.get("path").and_then(Value::as_str),
                ) {
                    (Some(expected), Some(path)) if expected == Path::new(path) => {}
                    _ => result.violations.push(format!(
                        "workspace edge must point to the actual workspace member path: {context}"
                    )),
                }
            } else if dependency.get("path").is_some_and(|value| !value.is_null()) {
                result.violations.push(format!(
                    "external dependency allowlist does not authorize a local path override: {context}"
                ));
            }
        }
    }
    Ok(result)
}

fn check_layers() -> Result<(), String> {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or_else(|| "xtask has no workspace directory".to_owned())?;
    let output = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
        .args(["metadata", "--no-deps", "--format-version", "1", "--locked"])
        .current_dir(workspace)
        .output()
        .map_err(|error| format!("could not run cargo metadata: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "cargo metadata failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let metadata = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("cargo metadata returned invalid JSON: {error}"))?;
    let result = validate(&metadata)?;
    if !result.violations.is_empty() {
        return Err(result.violations.join("\n"));
    }
    println!(
        "Layer policy passed: {} workspace packages, {} direct dependency edges (all kinds and targets).",
        result.packages, result.edges
    );
    Ok(())
}

fn main() -> ExitCode {
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    if arguments.as_slice() != ["layers"] {
        eprintln!("Usage: cargo xtask layers");
        return ExitCode::from(2);
    }
    match check_layers() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn dependency(name: &str) -> Value {
        json!({"name":name,"rename":null,"kind":null,"target":null,"path":null})
    }

    fn fixture() -> Value {
        let mut packages: Vec<_> = PACKAGES
            .iter()
            .map(|name| json!({"name":name,"id":name,"manifest_path":format!("/workspace/{name}/Cargo.toml"),"dependencies":[]}))
            .collect();
        packages[0]["dependencies"] = json!([dependency("serde")]);
        let mut core = dependency("skylive-core");
        core["path"] = json!("/workspace/skylive-core");
        packages[2]["dependencies"] = json!([core, dependency("serde_json")]);
        packages[5]["dependencies"] = json!([dependency("serde_json")]);
        json!({"workspace_members":PACKAGES,"packages":packages})
    }

    #[test]
    fn allowed_graph_is_accepted() {
        let result = validate(&fixture()).unwrap();
        assert_eq!(result.packages, PACKAGES.len());
        assert_eq!(result.edges, 4);
        assert!(result.violations.is_empty());
    }

    #[test]
    fn renamed_target_specific_dev_dependency_cannot_hide_host_in_core() {
        let mut metadata = fixture();
        let mut hidden = dependency("skylive-host");
        hidden["rename"] = json!("serde");
        hidden["kind"] = json!("dev");
        hidden["target"] = json!("cfg(windows)");
        hidden["optional"] = json!(true);
        hidden["path"] = json!("/workspace/skylive-host");
        metadata["packages"][0]["dependencies"] = json!([hidden]);
        let result = validate(&metadata).unwrap();
        assert_eq!(result.violations.len(), 1);
        assert!(result.violations[0].contains("skylive-core -> skylive-host"));
        assert!(result.violations[0].contains("alias=serde, kind=dev, target=cfg(windows)"));
    }

    #[test]
    fn media_build_dependencies_are_not_exempt() {
        let mut metadata = fixture();
        let mut native = dependency("cc");
        native["kind"] = json!("build");
        native["target"] = json!("cfg(target_os = \"macos\")");
        metadata["packages"][1]["dependencies"] = json!([native]);
        assert!(validate(&metadata).unwrap().violations[0].contains("skylive-media -> cc"));
    }

    #[test]
    fn unknown_workspace_layer_fails_closed() {
        let mut metadata = fixture();
        metadata["packages"][0]["name"] = json!("skylive-unclassified");
        assert!(
            validate(&metadata)
                .unwrap()
                .violations
                .iter()
                .any(|value| value.contains("unclassified"))
        );
    }

    #[test]
    fn workspace_package_name_cannot_point_to_another_path() {
        let mut metadata = fixture();
        metadata["packages"][2]["dependencies"][0]["path"] = json!("/other/skylive-core");
        assert!(
            validate(&metadata).unwrap().violations[0].contains("actual workspace member path")
        );
    }

    #[test]
    fn local_override_is_not_authorized_by_external_name() {
        let mut metadata = fixture();
        metadata["packages"][0]["dependencies"][0]["path"] = json!("/other/serde");
        assert!(validate(&metadata).unwrap().violations[0].contains("local path override"));
    }

    #[test]
    fn missing_member_metadata_is_an_error() {
        let mut metadata = fixture();
        metadata["packages"].as_array_mut().unwrap().pop();
        assert!(
            validate(&metadata)
                .unwrap_err()
                .contains("workspace member missing")
        );
    }
}

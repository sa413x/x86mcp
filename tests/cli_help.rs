use assert_cmd::Command;

#[test]
fn help_lists_only_the_approved_commands() {
    let output = Command::cargo_bin("x86mcp")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let help = String::from_utf8(output).unwrap();
    assert!(help.contains("index"));
    assert!(help.contains("setup"));
    assert!(help.contains("status"));
    assert!(help.contains("serve"));
    assert!(!help.contains("http"));
}

#[test]
fn status_reports_missing_snapshot_with_exit_code_two() {
    let root = tempfile::tempdir().unwrap();
    let output = Command::cargo_bin("x86mcp")
        .unwrap()
        .args(["--root", root.path().to_str().unwrap(), "status"])
        .assert()
        .code(2)
        .get_output()
        .stdout
        .clone();
    let status: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(status["state"], "missing");
    assert_eq!(status["snapshot_id"], serde_json::Value::Null);
}

#[test]
fn status_before_setup_reports_missing_without_creating_the_root() {
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("not-installed");
    let output = Command::cargo_bin("x86mcp")
        .unwrap()
        .env_remove("X86MCP_ROOT")
        .args(["--root", root.to_str().unwrap(), "status"])
        .assert()
        .code(2)
        .get_output()
        .stdout
        .clone();
    let status: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(status["state"], "missing");
    assert!(!root.exists());
}

#[cfg(unix)]
#[test]
fn status_with_default_root_before_setup_reports_missing() {
    let home = tempfile::tempdir().unwrap();
    let workdir = tempfile::tempdir().unwrap();
    let output = Command::cargo_bin("x86mcp")
        .unwrap()
        .current_dir(workdir.path())
        .env("HOME", home.path())
        .env("XDG_DATA_HOME", home.path().join("data"))
        .env("XDG_CACHE_HOME", home.path().join("cache"))
        .env_remove("X86MCP_ROOT")
        .arg("status")
        .assert()
        .code(2)
        .get_output()
        .stdout
        .clone();
    let status: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(status["state"], "missing");
    assert_eq!(std::fs::read_dir(home.path()).unwrap().count(), 0);
}

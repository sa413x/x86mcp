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

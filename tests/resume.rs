use serde_json::{Value, json};
use std::{fs, process::Command};

#[test]
fn resume_print_preserves_pi_session_path_and_never_launches_agent() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("an agent's store");
    fs::create_dir(&root).unwrap();
    let path = root.join("session.jsonl");
    fs::write(&path,format!("{}\n{}\n",json!({"type":"session","version":3,"id":"f6ff9ef0-3fb2-420b-a93a-2c036bce7311","timestamp":"2026-01-01T00:00:00Z","cwd":"/nonexistent/project with ' quotes"}),json!({"type":"message","id":"a","parentId":null,"message":{"role":"user","content":"marker"}}))).unwrap();
    let db = temp.path().join("db.sqlite3");
    let binary = env!("CARGO_BIN_EXE_ams");
    let output = Command::new(binary)
        .args([
            "--db",
            db.to_str().unwrap(),
            "scan",
            "--no-defaults",
            "--root",
            &format!("pi={}", root.display()),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = Command::new(binary)
        .args([
            "--db",
            db.to_str().unwrap(),
            "--json",
            "resume",
            "f6ff9ef0",
            "--print",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let command: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(command["program"], "pi");
    assert_eq!(command["args"], json!(["--session", path]));
    assert_eq!(command["cwd"], "/nonexistent/project with ' quotes");
}

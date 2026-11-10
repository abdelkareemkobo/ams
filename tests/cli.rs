use serde_json::{Value, json};
use std::{
    fs,
    process::{Command, Output},
};

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ams"))
        .args(args)
        .output()
        .unwrap()
}
fn session(path: &std::path::Path) {
    let rows = [
        json!({"type":"session","version":3,"id":"fa39e83e-96e7-46ac-9713-a0467255c273","timestamp":"2026-01-01T00:00:00Z","cwd":"/tmp"}),
        json!({"type":"message","id":"1","parentId":null,"timestamp":"2026-01-01T00:00:01Z","message":{"role":"user","content":[{"type":"text","text":"forgotten zebra deployment"}],"timestamp":1767225601000_i64}}),
        json!({"type":"message","id":"2","parentId":"1","timestamp":"2026-01-01T00:00:02Z","message":{"role":"assistant","content":[{"type":"thinking","thinking":"private reasoning must not move"},{"type":"text","text":"The fix is the pool size."}],"api":"openai-responses","provider":"openai","model":"gpt-5","usage":{"input":1,"output":1,"cacheRead":0,"cacheWrite":0,"totalTokens":2,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},"stopReason":"stop","timestamp":1767225602000_i64}}),
    ];
    fs::write(
        path,
        rows.iter().map(|r| format!("{r}\n")).collect::<String>(),
    )
    .unwrap();
}

#[test]
fn strict_migration_refuses_loss_before_installing() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source.jsonl");
    session(&source);
    let home = temp.path().join("target");
    let output = run(&[
        "transfer",
        source.to_str().unwrap(),
        "--from",
        "pi",
        "--to",
        "omp",
        "--home",
        home.to_str().unwrap(),
        "--strict",
    ]);
    assert!(!output.status.success());
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        result["error"]
            .as_str()
            .unwrap()
            .contains("strict migration rejected losses"),
        "{result}"
    );
    assert!(!home.exists());
}

#[test]
fn dry_run_never_creates_target_home() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source.jsonl");
    session(&source);
    let home = temp.path().join("target");
    let output = run(&[
        "transfer",
        source.to_str().unwrap(),
        "--from",
        "pi",
        "--to",
        "omp",
        "--home",
        home.to_str().unwrap(),
        "--dry-run",
    ]);
    assert!(
        output.status.success(),
        "{} {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["dry_run"], true);
    assert_eq!(result["installed"], false);
    assert!(!home.exists());
}

#[test]
fn forgotten_body_terms_are_found_without_knowing_agent_or_title() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source.jsonl");
    session(&source);
    let db = temp.path().join("index.sqlite3");
    let root = format!("pi={}", temp.path().display());
    let indexed = run(&[
        "--db",
        db.to_str().unwrap(),
        "scan",
        "--no-defaults",
        "--root",
        &root,
    ]);
    assert!(
        indexed.status.success(),
        "{}",
        String::from_utf8_lossy(&indexed.stderr)
    );
    let found = run(&[
        "--db",
        db.to_str().unwrap(),
        "--json",
        "find",
        "zebra deployment",
    ]);
    assert!(
        found.status.success(),
        "{}",
        String::from_utf8_lossy(&found.stderr)
    );
    let rows: Vec<Value> = serde_json::from_slice(&found.stdout).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["path"], source.to_str().unwrap());
    assert_eq!(rows[0]["format"], "pi");
    let absent = run(&[
        "--db",
        db.to_str().unwrap(),
        "--json",
        "find",
        "private reasoning",
    ]);
    let rows: Vec<Value> = serde_json::from_slice(&absent.stdout).unwrap();
    assert!(rows.is_empty());
}

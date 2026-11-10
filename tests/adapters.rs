use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;

use ams::adapters::{load, render, verify};
use ams::model::{Event, Format, Session};
use serde_json::{Value, json};

fn fixture(name: &str, format: Format) -> Session {
    load(
        &Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/adapters")
            .join(name),
        Some(format),
    )
    .unwrap()
}
fn journal(records: &[Value], format: Format) -> anyhow::Result<Session> {
    let mut file = tempfile::NamedTempFile::new()?;
    for record in records {
        serde_json::to_writer(&mut file, record)?;
        writeln!(file)?;
    }
    file.flush()?;
    load(file.path(), Some(format))
}
fn header() -> Value {
    json!({"type":"session","version":3,"id":"source-session","timestamp":"2026-08-17T12:00:00Z","cwd":"/work"})
}
fn text_entry(id: &str, parent: Option<&str>, text: &str) -> Value {
    json!({"type":"message","id":id,"parentId":parent,"timestamp":"2026-08-17T12:00:00Z","message":{"role":"user","content":text}})
}
fn text(session: &Session) -> Vec<&str> {
    session
        .events
        .iter()
        .filter_map(|e| match e {
            Event::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}
fn portable_session() -> Session {
    Session {
        format: Format::Omp,
        native_id: "source".into(),
        title: "Synthetic round trip".into(),
        cwd: "/work".into(),
        started_at: "2026-08-17T12:00:00Z".into(),
        losses: BTreeMap::new(),
        events: vec![
            Event::Text {
                role: "user".into(),
                text: "Find the forgotten maple deployment".into(),
            },
            Event::Image {
                role: "user".into(),
                mime_type: "image/png".into(),
                data: "c3ludGhldGlj".into(),
            },
            Event::Text {
                role: "assistant".into(),
                text: "Looking in the log".into(),
            },
            Event::ToolCall {
                id: "call_1".into(),
                name: "read".into(),
                arguments: json!({"path":"log.txt"}),
            },
            Event::ToolResult {
                id: "call_1".into(),
                name: "read".into(),
                text: "deployment found".into(),
                is_error: false,
            },
            Event::Summary {
                text: "Maple deployment was recorded yesterday".into(),
            },
            Event::Text {
                role: "user".into(),
                text: "Continue".into(),
            },
            Event::Text {
                role: "assistant".into(),
                text: "Found it".into(),
            },
        ],
    }
}

#[test]
fn sanitized_five_format_readers_preserve_readable_timeline() {
    for (name, format) in [
        ("omp.jsonl", Format::Omp),
        ("pi.jsonl", Format::Pi),
        ("codex.jsonl", Format::Codex),
        ("opencode.json", Format::Opencode),
        ("claude.jsonl", Format::Claude),
    ] {
        let session = fixture(name, format);
        assert!(!session.native_id.is_empty(), "{name}");
        assert!(!session.cwd.is_empty(), "{name}");
        assert!(
            session
                .events
                .iter()
                .any(|e| matches!(e, Event::Text { role, .. } if role == "user")),
            "{name}"
        );
        assert!(
            session
                .events
                .iter()
                .any(|e| matches!(e, Event::Text { role, .. } if role == "assistant")),
            "{name}"
        );
        assert!(
            session
                .events
                .iter()
                .any(|e| matches!(e, Event::ToolCall { .. })),
            "{name}"
        );
        assert!(
            session
                .events
                .iter()
                .any(|e| matches!(e, Event::ToolResult { .. })),
            "{name}"
        );
        if matches!(format, Format::Omp | Format::Codex | Format::Claude) {
            assert!(
                session
                    .events
                    .iter()
                    .any(|e| matches!(e, Event::Summary { .. })),
                "{name}"
            );
        }
        assert!(
            !session.losses.is_empty(),
            "runtime/source metadata must be accounted for: {name}"
        );
    }
}

#[test]
fn four_native_writers_round_trip_every_portable_event() {
    let session = portable_session();
    for target in [Format::Omp, Format::Pi, Format::Codex, Format::Opencode] {
        let rendered = render(&session, target, "/work").unwrap();
        assert_ne!(rendered.id, session.native_id);
        assert!(!rendered.relative_path.is_absolute());
        verify(&session, &rendered, target).unwrap();
        let second = render(&session, target, "/work").unwrap();
        assert_ne!(rendered.id, second.id);
    }
}

#[test]
fn latest_tree_leaf_excludes_inactive_branches() {
    let session = journal(
        &[
            header(),
            text_entry("a", None, "root"),
            text_entry("abandoned", Some("a"), "DO_NOT_RESURRECT"),
            text_entry("selected", Some("a"), "active"),
        ],
        Format::Pi,
    )
    .unwrap();
    assert_eq!(text(&session), ["root", "active"]);
    assert_eq!(session.losses["history:inactive_branch"], 1);
}

#[test]
fn reset_boundary_never_replays_old_omp_history() {
    let reset = json!({"type":"reset_boundary","id":"reset","parentId":"old","timestamp":"2026-08-17T12:00:00Z"});
    let session = journal(
        &[
            header(),
            text_entry("old", None, "OLD_SECRET"),
            reset,
            text_entry("new", Some("reset"), "fresh"),
        ],
        Format::Omp,
    )
    .unwrap();
    assert_eq!(text(&session), ["fresh"]);
    assert_eq!(session.losses["history:pre_reset"], 1);
    assert_eq!(session.losses["runtime:reset_boundary"], 1);
}

#[test]
fn tree_schema_and_ancestry_fail_closed() {
    assert!(
        journal(
            &[header(), text_entry("child", Some("missing"), "bad")],
            Format::Pi
        )
        .is_err()
    );
    assert!(
        journal(
            &[
                header(),
                text_entry("a", None, "one"),
                text_entry("a", None, "two")
            ],
            Format::Pi
        )
        .is_err()
    );
    assert!(journal(&[header(), text_entry("a", Some("a"), "cycle")], Format::Pi).is_err());
    let mut wrong = header();
    wrong["version"] = json!(99);
    assert!(journal(&[wrong], Format::Pi).is_err());
    let mut compact = json!({"type":"compaction","id":"summary","parentId":null,"timestamp":"2026-08-17T12:00:00Z","summary":"summary","firstKeptEntryId":"missing","tokensBefore":0});
    assert!(journal(&[header(), compact.clone()], Format::Omp).is_err());
    compact["firstKeptEntryId"] = json!("summary");
    assert!(journal(&[header(), compact], Format::Pi).is_ok());
}

#[test]
fn reasoning_and_runtime_omissions_are_explicit() {
    let assistant = json!({"type":"message","id":"a","parentId":null,"timestamp":"2026-08-17T12:00:00Z","message":{"role":"assistant","content":[{"type":"thinking","thinking":"private thought"},{"type":"text","text":"public answer"}],"usage":{"input":123},"provider":"test","model":"test"}});
    let runtime = json!({"type":"credential_pin","id":"b","parentId":"a","timestamp":"2026-08-17T12:00:00Z","credential":"DO_NOT_INDEX"});
    let session = journal(&[header(), assistant, runtime], Format::Omp).unwrap();
    assert_eq!(text(&session), ["public answer"]);
    for key in [
        "reasoning",
        "message:usage",
        "message:model",
        "message:provider",
        "runtime:credential_pin",
        "event:timestamp",
    ] {
        assert!(session.losses.contains_key(key), "{key}");
    }
    let rendered = render(&session, Format::Pi, "/work").unwrap();
    for (key, count) in session.losses {
        assert_eq!(rendered.losses[&key], count);
    }
}

#[test]
fn omp_title_slot_is_exactly_256_bytes_with_unicode() {
    let mut session = portable_session();
    session.title = "أمس 🦀 ".repeat(100);
    let rendered = render(&session, Format::Omp, "/work").unwrap();
    assert_eq!(
        rendered.bytes.iter().position(|&b| b == b'\n').unwrap() + 1,
        256
    );
    let slot: Value = serde_json::from_slice(&rendered.bytes[..255]).unwrap();
    assert_eq!(slot["type"], "title");
    assert!(session.title.starts_with(slot["title"].as_str().unwrap()));
    assert_eq!(rendered.losses["session:title_truncated"], 1);
    verify(&session, &rendered, Format::Omp).unwrap();
}

#[test]
fn omp_trailing_title_and_pi_session_info_override_header() {
    let title = json!({"type":"title_change","id":"title","parentId":"a","timestamp":"2026-08-17T12:00:00Z","title":"Changed OMP title"});
    let omp = journal(
        &[header(), text_entry("a", None, "body"), title],
        Format::Omp,
    )
    .unwrap();
    assert_eq!(omp.title, "Changed OMP title");
    let info = json!({"type":"session_info","id":"info","parentId":"a","timestamp":"2026-08-17T12:00:00Z","name":"Named Pi session"});
    let pi = journal(&[header(), text_entry("a", None, "body"), info], Format::Pi).unwrap();
    assert_eq!(pi.title, "Named Pi session");
}

#[test]
fn paginated_codex_uses_only_canonical_completed_messages() {
    let session = fixture("codex-paginated.jsonl", Format::Codex);
    assert_eq!(text(&session).len(), 4);
    assert!(!text(&session).join(" ").contains("INTERNAL ENVIRONMENT"));
    assert!(text(&session)[0].contains("BETA-2048"));
    assert_eq!(session.losses["codex:paginated_provider_message"], 5);
    assert!(
        session
            .events
            .iter()
            .any(|e| matches!(e, Event::Image { .. }))
    );
}

#[test]
fn codex_replacement_lineage_and_rollback_are_not_flattened() {
    let meta = json!({"type":"session_meta","timestamp":"2026-08-17T12:00:00Z","payload":{"id":"source","cwd":"/work","history_mode":"legacy"}});
    let replacement =
        json!({"type":"compacted","payload":{"message":"summary","replacement_history":[]}});
    assert!(
        journal(&[meta.clone(), replacement], Format::Codex)
            .unwrap_err()
            .to_string()
            .contains("replacement_history")
    );
    let rollback =
        json!({"type":"event_msg","payload":{"type":"thread_rolled_back","num_turns":1}});
    assert!(journal(&[meta.clone(), rollback], Format::Codex).is_err());
    let mut inherited = meta;
    inherited["payload"]["history_base"] = json!({"id":"other"});
    assert!(journal(&[inherited], Format::Codex).is_err());
}

#[test]
fn codex_error_flag_loss_is_reported_and_verified() {
    let mut session = portable_session();
    if let Event::ToolResult { is_error, .. } = &mut session.events[4] {
        *is_error = true;
    }
    let rendered = render(&session, Format::Codex, "/work").unwrap();
    assert_eq!(rendered.losses["tool_result:is_error"], 1);
    verify(&session, &rendered, Format::Codex).unwrap();
}

#[test]
fn empty_tool_result_is_valid_and_preserved() {
    let mut session = portable_session();
    if let Event::ToolResult { text, .. } = &mut session.events[4] {
        text.clear();
    }
    for target in [Format::Omp, Format::Pi, Format::Codex, Format::Opencode] {
        let rendered = render(&session, target, "/work").unwrap();
        verify(&session, &rendered, target).unwrap();
    }
}

#[test]
fn opencode_refuses_nonadjacent_tool_result_reordering() {
    let mut session = portable_session();
    session.events.insert(
        4,
        Event::Text {
            role: "assistant".into(),
            text: "intervening answer".into(),
        },
    );
    assert!(
        render(&session, Format::Opencode, "/work")
            .unwrap_err()
            .to_string()
            .contains("reorder")
    );
    for target in [Format::Pi, Format::Codex] {
        let rendered = render(&session, target, "/work").unwrap();
        verify(&session, &rendered, target).unwrap();
    }
}

#[test]
fn orphan_and_duplicate_tools_fail_without_fabricated_calls() {
    let mut session = portable_session();
    session.events.remove(3);
    assert!(render(&session, Format::Pi, "/work").is_err());
    let mut session = portable_session();
    session.events.insert(4, session.events[3].clone());
    assert!(render(&session, Format::Codex, "/work").is_err());
}

#[test]
fn claude_uuid_graph_order_beats_physical_order() {
    let record = |id: &str, parent: Option<&str>, role: &str, content: Value| json!({"type":role,"uuid":id,"parentUuid":parent,"sessionId":"source","timestamp":"2026-08-17T12:00:00Z","cwd":"/work","message":{"role":role,"content":content}});
    let records = [
        record("root", None, "user", json!("root")),
        record(
            "child",
            Some("call"),
            "user",
            json!([{"type":"tool_result","tool_use_id":"tool","content":"result"}]),
        ),
        record(
            "call",
            Some("root"),
            "assistant",
            json!([{"type":"tool_use","id":"tool","name":"read","input":{}}]),
        ),
        json!({"type":"last-prompt","leafUuid":"child"}),
    ];
    let session = journal(&records, Format::Claude).unwrap();
    assert!(matches!(&session.events[1], Event::ToolCall { id, .. } if id == "tool"));
    assert!(
        matches!(&session.events[2], Event::ToolResult { id, name, .. } if id == "tool" && name == "read")
    );
}

#[test]
fn claude_inactive_branch_and_missing_parent_are_never_replayed() {
    let record = |id: &str, parent: Option<&str>, content: &str| json!({"type":"user","uuid":id,"parentUuid":parent,"sessionId":"source","timestamp":"2026-08-17T12:00:00Z","cwd":"/work","message":{"role":"user","content":content}});
    let session = journal(
        &[
            record("a", None, "root"),
            record("dead", Some("a"), "inactive"),
            record("b", Some("a"), "active"),
        ],
        Format::Claude,
    )
    .unwrap();
    assert_eq!(text(&session), ["root", "active"]);
    assert_eq!(session.losses["history:inactive_or_pre_compaction"], 1);
    assert!(journal(&[record("bad", Some("missing"), "bad")], Format::Claude).is_err());
}

#[test]
fn claude_writer_and_privileged_role_cutovers_are_unsupported() {
    let mut session = portable_session();
    assert!(render(&session, Format::Claude, "/work").is_err());
    session.events[0] = Event::Text {
        role: "system".into(),
        text: "privileged".into(),
    };
    for target in [Format::Omp, Format::Pi, Format::Codex, Format::Opencode] {
        assert!(render(&session, target, "/work").is_err());
    }
}

#[test]
fn opencode_revert_hides_reverted_messages_and_counts_loss() {
    let mut bundle: Value =
        serde_json::from_str(include_str!("fixtures/adapters/opencode.json")).unwrap();
    bundle["info"]["revert"] = json!({"messageID":"msg_00000000000000000000000002"});
    let session = journal(&[bundle], Format::Opencode).unwrap();
    assert_eq!(text(&session), ["SYNTHETIC_USER_MARKER"]);
    assert_eq!(session.losses["history:reverted"], 1);
    assert!(session.losses.contains_key("session:revert"));
}

#[test]
fn opencode_v2_public_bundle_has_real_readable_content() {
    let bundle = json!({"info":{"id":"ses_v2","title":"Native v2","location":{"directory":"/work"},"time":{"created":1787054400000_i64},"cost":0,"tokens":{}},"messages":[
        {"id":"msg_1","type":"user","text":"lost session","time":{"created":1787054400000_i64}},
        {"id":"msg_2","type":"assistant","content":[{"type":"reasoning","text":"secret thought"},{"type":"text","text":"found session"},{"type":"tool","id":"call","name":"read","state":{"status":"completed","input":{},"content":[{"type":"text","text":"result"}]}}],"time":{"created":1787054400001_i64}}
    ]});
    let session = journal(&[bundle], Format::Opencode).unwrap();
    assert_eq!(text(&session), ["lost session", "found session"]);
    assert_eq!(session.losses["reasoning"], 1);
    assert!(
        matches!(session.events.last().unwrap(), Event::ToolResult { text, .. } if text == "result")
    );
}

#[test]
fn claude_preserved_segment_cycle_requires_declared_metadata() {
    let boundary = json!({"type":"system","subtype":"compact_boundary","uuid":"boundary","parentUuid":null,"logicalParentUuid":"tail","sessionId":"source","timestamp":"2026-08-17T12:00:00Z","cwd":"/work","compactMetadata":{"preservedSegment":{"anchorUuid":"summary","headUuid":"head","tailUuid":"tail"},"preservedMessages":{"allUuids":["head","tail"]}}});
    let summary = json!({"type":"user","uuid":"summary","parentUuid":"boundary","sessionId":"source","timestamp":"2026-08-17T12:00:00Z","isCompactSummary":true,"message":{"role":"user","content":"summary"}});
    let record = |id: &str, parent: &str| json!({"type":"user","uuid":id,"parentUuid":parent,"sessionId":"source","timestamp":"2026-08-17T12:00:00Z","message":{"role":"user","content":id}});
    let mut records = vec![
        boundary,
        summary,
        record("head", "summary"),
        record("tail", "head"),
        json!({"type":"last-prompt","leafUuid":"tail"}),
    ];
    let session = journal(&records, Format::Claude).unwrap();
    assert_eq!(text(&session), ["head", "tail"]);
    assert!(matches!(&session.events[0], Event::Summary { text } if text == "summary"));
    records[0]["compactMetadata"] = Value::Null;
    assert!(journal(&records, Format::Claude).is_err());
}

#[test]
fn malformed_opencode_ancestry_and_revert_fail_closed() {
    let mut bundle: Value =
        serde_json::from_str(include_str!("fixtures/adapters/opencode.json")).unwrap();
    bundle["messages"][1]["info"]["parentID"] = json!("msg_missing");
    assert!(journal(&[bundle], Format::Opencode).is_err());
    let mut bundle: Value =
        serde_json::from_str(include_str!("fixtures/adapters/opencode.json")).unwrap();
    bundle["info"]["revert"] = json!({"unknown":"cannot infer boundary"});
    assert!(journal(&[bundle], Format::Opencode).is_err());
}

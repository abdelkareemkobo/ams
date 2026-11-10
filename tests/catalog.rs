use ams::catalog::Catalog;
use ams::model::Format;
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use std::fs;
use std::path::Path;

fn jsonl(path: &Path, records: &[Value]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        path,
        records.iter().map(|v| format!("{v}\n")).collect::<String>(),
    )
    .unwrap();
}
fn pi(path: &Path, id: &str, body: &str) {
    jsonl(
        path,
        &[
            json!({"type":"session","version":3,"id":id,"cwd":"/sample","timestamp":"2026-01-01T00:00:00Z"}),
            json!({"type":"message","message":{"role":"user","content":[{"type":"text","text":"ordinary opening"}]}}),
            json!({"type":"message","message":{"role":"assistant","content":[{"type":"text","text":body},{"type":"thinking","thinking":"hiddenreasoning"},{"type":"toolCall","name":"api","arguments":{"secret":"toolcredential"}}]}}),
            json!({"type":"message","message":{"role":"toolResult","content":[{"type":"text","text":"tooloutputsecret"}]}}),
            json!({"type":"session_info","name":"Neutral heading"}),
        ],
    );
}

#[test]
fn forgotten_terms_deep_in_all_five_formats_are_searchable_and_snippets_center_hits() {
    let tmp = tempfile::tempdir().unwrap();
    let deep = format!(
        "{} sapphire forgotten résumé العربية",
        "unrelated context ".repeat(300)
    );
    pi(&tmp.path().join("pi/s.jsonl"), "pi-session", &deep);
    let omp = tmp.path().join("omp/s.jsonl");
    pi(&omp, "omp-session", &deep);
    let mut original = fs::read_to_string(&omp).unwrap();
    original.insert_str(
        0,
        &format!(
            "{}\n",
            json!({"type":"title","v":1,"title":"Initial padded title","pad":" ".repeat(4000)})
        ),
    );
    fs::write(&omp, original).unwrap();
    jsonl(
        &tmp.path().join("codex/s.jsonl"),
        &[
            json!({"type":"session_meta","payload":{"id":"codex-session","cwd":"/sample","timestamp":"2026-01-01"}}),
            json!({"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":deep}]}}),
            json!({"type":"response_item","payload":{"type":"function_call_output","output":"tooloutputsecret"}}),
        ],
    );
    jsonl(
        &tmp.path().join("claude/s.jsonl"),
        &[
            json!({"type":"user","sessionId":"claude-session","cwd":"/sample","timestamp":"2026-01-01","message":{"role":"user","content":"initial request"}}),
            json!({"type":"assistant","sessionId":"claude-session","message":{"role":"assistant","content":[{"type":"text","text":deep},{"type":"thinking","thinking":"hiddenreasoning"}]}}),
            json!({"type":"user","sessionId":"claude-session","message":{"role":"user","content":[{"type":"tool_result","content":"tooloutputsecret"}]}}),
        ],
    );
    fs::create_dir(tmp.path().join("opencode")).unwrap();
    fs::write(tmp.path().join("opencode/export.json"),json!({"info":{"id":"ses_export","title":"Neutral heading","directory":"/sample","time":{"created":1}},"messages":[{"info":{"role":"assistant"},"parts":[{"type":"text","text":deep},{"type":"tool","state":{"input":{"secret":"toolcredential"},"output":"tooloutputsecret"}}]}]}).to_string()).unwrap();
    fs::write(
        tmp.path().join("opencode/auth.json"),
        "{ definitely not an exported session",
    )
    .unwrap();
    let roots = [
        (Format::Pi, tmp.path().join("pi")),
        (Format::Omp, tmp.path().join("omp")),
        (Format::Codex, tmp.path().join("codex")),
        (Format::Claude, tmp.path().join("claude")),
        (Format::Opencode, tmp.path().join("opencode")),
    ];
    let mut catalog = Catalog::open(&tmp.path().join("catalog.db")).unwrap();
    let report = catalog.scan(&roots, 3).unwrap();
    assert_eq!(report.scanned, 5);
    assert_eq!(report.errors, 0);
    let results = catalog.find("sapphire forgotten", None, 20).unwrap();
    assert_eq!(results.len(), 5);
    assert!(results.iter().all(|e| e.summary.contains("sapphire")));
    assert_eq!(catalog.find("résumé العربية", None, 20).unwrap().len(), 5);
    assert_eq!(
        catalog
            .find("sapphire", Some(Format::Claude), 20)
            .unwrap()
            .len(),
        1
    );
    for secret in ["hiddenreasoning", "toolcredential", "tooloutputsecret"] {
        assert!(catalog.find(secret, None, 20).unwrap().is_empty());
    }
    assert!(
        catalog
            .find_titles("sapphire", None, 20)
            .unwrap()
            .is_empty()
    );
    assert_eq!(catalog.scan(&[], 2).unwrap().unchanged, 5);
}

#[test]
fn incremental_changes_missing_files_and_failed_roots_have_distinct_states() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("sessions");
    let source = root.join("one.jsonl");
    pi(&source, "one-native", "firstneedle");
    let database = tmp.path().join("catalog.db");
    let mut catalog = Catalog::open(&database).unwrap();
    assert_eq!(
        catalog
            .scan(&[(Format::Pi, root.clone())], 1)
            .unwrap()
            .scanned,
        1
    );
    drop(catalog);
    let mut catalog = Catalog::open(&database).unwrap();
    assert_eq!(catalog.scan(&[], 1).unwrap().unchanged, 1);
    pi(&source, "one-native", "replacementneedle");
    assert_eq!(catalog.scan(&[], 1).unwrap().scanned, 1);
    assert!(catalog.find("firstneedle", None, 10).unwrap().is_empty());
    assert_eq!(
        catalog.find("replacementneedle", None, 10).unwrap().len(),
        1
    );
    fs::rename(&root, tmp.path().join("moved")).unwrap();
    let failed = catalog.scan(&[], 1).unwrap();
    assert_eq!(failed.missing, 0);
    assert_eq!(failed.errors, 1);
    assert_eq!(catalog.resolve("one-native").unwrap().status, "unavailable");
    assert_eq!(
        catalog.find("replacementneedle", None, 10).unwrap()[0].status,
        "unavailable"
    );
    fs::rename(tmp.path().join("moved"), &root).unwrap();
    fs::remove_file(&source).unwrap();
    assert_eq!(catalog.scan(&[], 1).unwrap().missing, 1);
    assert_eq!(catalog.resolve("one-native").unwrap().status, "missing");
    assert!(
        catalog
            .find("replacementneedle", None, 10)
            .unwrap()
            .is_empty()
    );
    pi(&source, "one-native", "reappearedneedle");
    assert_eq!(catalog.scan(&[], 1).unwrap().scanned, 1);
    assert_eq!(catalog.resolve("one-native").unwrap().status, "ok");
}

#[test]
fn malformed_sources_are_isolated_retried_and_remove_old_search_text() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("sessions");
    pi(&root.join("good.jsonl"), "good", "healthynative");
    pi(&root.join("bad.jsonl"), "bad", "oldneedle");
    let mut catalog = Catalog::open(&tmp.path().join("catalog.db")).unwrap();
    catalog.scan(&[(Format::Pi, root.clone())], 2).unwrap();
    fs::write(
        root.join("bad.jsonl"),
        "invalid transcript with secretnotforexposure",
    )
    .unwrap();
    let report = catalog.scan(&[], 2).unwrap();
    assert_eq!(report.errors, 1);
    assert_eq!(report.unchanged, 1);
    assert!(catalog.find("oldneedle", None, 10).unwrap().is_empty());
    assert_eq!(catalog.find("healthynative", None, 10).unwrap().len(), 1);
    let entries = catalog.list(None, 10).unwrap();
    assert_eq!(entries.iter().filter(|e| e.status == "error").count(), 1);
    assert!(
        !serde_json::to_string(&entries)
            .unwrap()
            .contains("secretnotforexposure")
    );
    assert_eq!(catalog.scan(&[], 2).unwrap().errors, 1);
}

#[test]
fn literal_search_and_prefix_resolution_do_not_accept_sql_or_match_syntax() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("sessions");
    pi(&root.join("a.jsonl"), "shared-a", "alpha beta");
    pi(&root.join("b.jsonl"), "shared-b", "alpha only");
    pi(&root.join("c.jsonl"), "same-native", "gamma");
    pi(&root.join("d.jsonl"), "same-native", "delta");
    let mut catalog = Catalog::open(&tmp.path().join("catalog.db")).unwrap();
    catalog.scan(&[(Format::Pi, root)], 2).unwrap();
    assert_eq!(catalog.find("alpha beta", None, 10).unwrap().len(), 1);
    assert_eq!(
        catalog.find("\"alpha\" + beta*", None, 10).unwrap().len(),
        1
    );
    assert!(
        catalog
            .find("alpha OR nonexistent", None, 10)
            .unwrap()
            .is_empty()
    );
    assert!(catalog.find(" \" * : ", None, 10).is_err());
    assert!(
        catalog
            .resolve("shared-")
            .unwrap_err()
            .to_string()
            .contains("ambiguous")
    );
    assert!(
        catalog
            .resolve("same-native")
            .unwrap_err()
            .to_string()
            .contains("ambiguous")
    );
    assert!(catalog.resolve("%").is_err());
    assert!(catalog.resolve("").is_err());
    let exact = catalog.resolve("shared-a").unwrap();
    assert_eq!(catalog.resolve(&exact.id[..20]).unwrap().id, exact.id);
}

fn live_db(path: &Path) -> Connection {
    let db = Connection::open(path).unwrap();
    db.execute_batch("CREATE TABLE session(id TEXT PRIMARY KEY,title TEXT,directory TEXT,time_created INTEGER,time_updated INTEGER);
        CREATE TABLE message(id TEXT PRIMARY KEY,session_id TEXT,time_created INTEGER,time_updated INTEGER,data TEXT);
        CREATE TABLE part(id TEXT PRIMARY KEY,message_id TEXT,session_id TEXT,time_created INTEGER,time_updated INTEGER,data TEXT);
        CREATE TABLE session_v2(id TEXT PRIMARY KEY,title TEXT,directory TEXT,time_created INTEGER,time_updated INTEGER);
        CREATE TABLE session_message(id TEXT PRIMARY KEY,session_id TEXT,type TEXT,seq INTEGER,time_created INTEGER,time_updated INTEGER,data TEXT);
        CREATE TABLE session_part(message_id TEXT,session_id TEXT,ordinal INTEGER,type TEXT,time_created INTEGER,time_updated INTEGER,data TEXT);").unwrap();
    db
}

#[test]
fn live_opencode_indexes_legacy_and_v2_read_only_without_runtime_or_tools() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("opencode.db");
    let db = live_db(&source);
    for id in ["ses_legacy", "ses_duplicate"] {
        db.execute(
            "INSERT INTO session VALUES(?1,'legacy title','/sample',100,200)",
            [id],
        )
        .unwrap();
        db.execute(
            "INSERT INTO message VALUES(?1,?1,100,200,?2)",
            params![
                id,
                json!({"role":"assistant","provider":{"apiKey":"credentialsecret"}}).to_string()
            ],
        )
        .unwrap();
        db.execute(
            "INSERT INTO part VALUES(?1,?1,?1,100,200,?2)",
            params![
                id,
                json!({"type":"text","text":"legacybodyneedle"}).to_string()
            ],
        )
        .unwrap();
    }
    for id in ["ses_v2", "ses_duplicate"] {
        db.execute(
            "INSERT INTO session_v2 VALUES(?1,'current title','/sample',100,200)",
            [id],
        )
        .unwrap();
        db.execute(
            "INSERT INTO session_message VALUES(?1,?1,'user',0,100,200,?2)",
            params![id, json!({"text":"currentuserneedle"}).to_string()],
        )
        .unwrap();
        let assistant = format!("assistant-{id}");
        db.execute("INSERT INTO session_message VALUES(?1,?2,'assistant',1,100,200,?3)",params![assistant,id,json!({"content":[{"type":"text","text":"currentassistantneedle"},{"type":"tool","state":{"input":{"apiKey":"credentialsecret"},"content":[{"type":"text","text":"tooloutputsecret"}]}},{"type":"reasoning","text":"reasoningsecret"}]}).to_string()]).unwrap();
        db.execute(
            "INSERT INTO session_part VALUES(?1,?2,0,'text',100,200,?3)",
            params![
                assistant,
                id,
                json!({"type":"text","text":"currentpartneedle"}).to_string()
            ],
        )
        .unwrap();
    }
    db.execute(
        "INSERT INTO session_v2 VALUES('ses_bad','bad session','/sample',100,200)",
        [],
    )
    .unwrap();
    db.execute(
        "INSERT INTO session_message VALUES('badmsg','ses_bad','user',0,100,200,'broken JSON')",
        [],
    )
    .unwrap();
    drop(db);
    let before = blake3::hash(&fs::read(&source).unwrap());
    let mut catalog = Catalog::open(&tmp.path().join("catalog.db")).unwrap();
    let report = catalog
        .scan(&[(Format::Opencode, source.clone())], 2)
        .unwrap();
    assert_eq!(report.scanned, 4);
    assert_eq!(report.errors, 1);
    assert_eq!(catalog.find("legacybodyneedle", None, 10).unwrap().len(), 1);
    for needle in [
        "currentuserneedle",
        "currentassistantneedle",
        "currentpartneedle",
    ] {
        assert_eq!(catalog.find(needle, None, 10).unwrap().len(), 2);
    }
    for needle in ["credentialsecret", "tooloutputsecret", "reasoningsecret"] {
        assert!(catalog.find(needle, None, 10).unwrap().is_empty());
    }
    assert_eq!(
        catalog.resolve("ses_duplicate").unwrap().title,
        "current title"
    );
    assert_eq!(catalog.resolve("ses_v2").unwrap().path, source);
    assert_eq!(blake3::hash(&fs::read(&source).unwrap()), before);
    let warm = catalog.scan(&[], 2).unwrap();
    assert_eq!(warm.unchanged, 3);
    assert_eq!(warm.errors, 1);
    let db = Connection::open(&source).unwrap();
    db.execute(
        "UPDATE session_message SET data=?1,time_updated=201 WHERE id='ses_v2'",
        [json!({"text":"newlivebodyneedle"}).to_string()],
    )
    .unwrap();
    drop(db);
    catalog.scan(&[], 2).unwrap();
    assert_eq!(
        catalog.find("newlivebodyneedle", None, 10).unwrap().len(),
        1
    );
}

#[test]
fn unavailable_live_database_does_not_mark_previous_sessions_missing() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("opencode.db");
    let db = live_db(&source);
    db.execute(
        "INSERT INTO session_v2 VALUES('ses_live','title','/sample',1,2)",
        [],
    )
    .unwrap();
    drop(db);
    let mut catalog = Catalog::open(&tmp.path().join("catalog.db")).unwrap();
    catalog
        .scan(&[(Format::Opencode, source.clone())], 1)
        .unwrap();
    fs::write(&source, "not sqlite").unwrap();
    let report = catalog.scan(&[], 1).unwrap();
    assert_eq!(report.missing, 0);
    assert_eq!(report.errors, 1);
    assert_eq!(catalog.resolve("ses_live").unwrap().status, "unavailable");
}

#[cfg(unix)]
#[test]
fn catalog_permissions_are_private_without_chmodding_an_existing_parent() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let parent = tmp.path().join("existing");
    fs::create_dir(&parent).unwrap();
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o755)).unwrap();
    let db = parent.join("catalog.db");
    Catalog::open(&db).unwrap();
    assert_eq!(
        fs::metadata(&parent).unwrap().permissions().mode() & 0o777,
        0o755
    );
    assert_eq!(
        fs::metadata(&db).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let nested = tmp.path().join("new/private/catalog.db");
    Catalog::open(&nested).unwrap();
    assert_eq!(
        fs::metadata(nested.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    let link = tmp.path().join("symlink.db");
    std::os::unix::fs::symlink(&db, &link).unwrap();
    assert!(Catalog::open(&link).is_err());
}

#[cfg(feature = "semantic")]
#[test]
fn missing_embeddings_fail_clearly_without_fetching_a_model() {
    let tmp = tempfile::tempdir().unwrap();
    let catalog = Catalog::open(&tmp.path().join("catalog.db")).unwrap();
    let error = ams::semantic::search(
        &catalog,
        "forgotten term",
        "definitely-not-a-network-request",
        None,
        10,
        true,
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("no embeddings") || error.contains("disabled"));
}

#[test]
fn unregistering_roots_only_removes_unowned_catalog_rows_and_never_sources() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("sessions");
    let child = root.join("project");
    let source = child.join("one.jsonl");
    pi(&source, "shared-source", "findablebody");
    let mut catalog = Catalog::open(&tmp.path().join("catalog.db")).unwrap();
    catalog
        .scan(
            &[(Format::Pi, root.clone()), (Format::Pi, child.clone())],
            2,
        )
        .unwrap();
    assert_eq!(catalog.list(None, 10).unwrap().len(), 1);
    assert_eq!(catalog.remove_root(Format::Pi, &root.join(".")).unwrap(), 0);
    assert_eq!(catalog.find("findablebody", None, 10).unwrap().len(), 1);
    assert_eq!(catalog.roots().unwrap().len(), 1);
    assert_eq!(catalog.remove_root(Format::Pi, &child).unwrap(), 1);
    assert!(catalog.roots().unwrap().is_empty());
    assert!(catalog.list(None, 10).unwrap().is_empty());
    assert!(catalog.find("findablebody", None, 10).unwrap().is_empty());
    assert!(source.exists());
}

#[test]
fn changed_content_invalidates_vectors_but_unchanged_scan_preserves_them() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("sessions/one.jsonl");
    pi(&source, "vector-source", "originalbody");
    let database = tmp.path().join("catalog.db");
    let mut catalog = Catalog::open(&database).unwrap();
    catalog
        .scan(&[(Format::Pi, source.parent().unwrap().into())], 1)
        .unwrap();
    let connection = Connection::open(&database).unwrap();
    connection
        .execute(
            "INSERT INTO embedding_models VALUES('fake-local-test','identity')",
            [],
        )
        .unwrap();
    connection.execute("INSERT INTO embeddings SELECT id,'fake-local-test',content_hash,0,zeroblob(4) FROM entries",[]).unwrap();
    catalog.scan(&[], 1).unwrap();
    let count: i64 = connection
        .query_row("SELECT count(*) FROM embeddings", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1);
    pi(&source, "vector-source", "changedbody");
    catalog.scan(&[], 1).unwrap();
    let count: i64 = connection
        .query_row("SELECT count(*) FROM embeddings", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 0);
    #[cfg(feature = "semantic")]
    {
        let error =
            ams::semantic::search(&catalog, "changedbody", "fake-local-test", None, 10, true)
                .unwrap_err()
                .to_string();
        assert!(error.contains("missing or stale embeddings"));
    }
}

use ams::{catalog::Catalog, model::Format};
use rusqlite::Connection;
use serde_json::json;
use std::fs;

#[test]
fn project_name_is_searchable_with_generic_title_after_v1_index_upgrade() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("index.db");
    let file = tmp.path().join("session.jsonl");
    let records = [
        json!({"type":"session","version":3,"id":"0f54c872-dfe7-4d0d-a7e5-efdd4e75c882","cwd":"/work/Projects/codetrail","timestamp":"2026-01-01T00:00:00Z"}),
        json!({"type":"message","message":{"role":"user","content":"Compare CPU and GPU embeddings."}}),
        json!({"type":"session_info","name":"An ordinary discussion"}),
    ];
    fs::write(
        &file,
        records.iter().map(|v| format!("{v}\n")).collect::<String>(),
    )
    .unwrap();
    let mut catalog = Catalog::open(&db).unwrap();
    catalog.scan(&[(Format::Pi, file.clone())], 1).unwrap();
    let entry = catalog.list(None, 1).unwrap().pop().unwrap();
    drop(catalog);
    let connection = Connection::open(&db).unwrap();
    connection.execute("INSERT INTO embeddings(id,model,content_hash,chunk,vector) SELECT id,'existing-model',content_hash,0,?1 FROM entries",[vec![0_u8,0,128,63]]).unwrap();
    connection.execute_batch("DROP TABLE sessions_fts;
        CREATE VIRTUAL TABLE sessions_fts USING fts5(id UNINDEXED,title,search_text,tokenize='unicode61');
        INSERT INTO sessions_fts(rowid,id,title,search_text) SELECT row_id,id,title,search_text FROM entries;
        PRAGMA user_version=1;").unwrap();
    drop(connection);
    let catalog = Catalog::open(&db).unwrap();
    let found = catalog.find("codetrail", None, 20).unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].id, entry.id);
    assert_eq!(found[0].path, file);
    assert_eq!(
        catalog.find("CPU embeddings", None, 20).unwrap()[0].id,
        entry.id
    );
    assert!(
        catalog
            .find_titles("codetrail", None, 20)
            .unwrap()
            .is_empty()
    );
    drop(catalog);
    let connection = Connection::open(&db).unwrap();
    let count: i64 = connection
        .query_row(
            "SELECT count(*) FROM embeddings WHERE model='existing-model'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        count, 1,
        "metadata index upgrade must preserve existing embeddings"
    );
    assert_eq!(
        fs::read_to_string(file).unwrap(),
        records.iter().map(|v| format!("{v}\n")).collect::<String>()
    );
}

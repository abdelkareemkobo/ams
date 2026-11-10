use ams::{catalog::Catalog, model::Format};
use rusqlite::Connection;
use serde_json::json;
use std::{fs, path::Path};

fn journal(path: &Path, id: &str, text: &str) {
    fs::write(path, format!("{}\n{}\n",json!({"type":"session","version":3,"id":id,"cwd":"/sample","timestamp":"2026-01-01T00:00:00Z"}),json!({"type":"message","message":{"role":"user","content":text}}))).unwrap();
}

#[test]
fn forgetting_a_root_and_vacuuming_do_not_reassociate_search_hits() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("index.db");
    let first = tmp.path().join("a.jsonl");
    let second = tmp.path().join("b.jsonl");
    journal(&first, "first", "retiredorchard");
    journal(&second, "second", "originalgarden");
    let mut catalog = Catalog::open(&db).unwrap();
    catalog
        .scan(
            &[(Format::Pi, first.clone()), (Format::Pi, second.clone())],
            1,
        )
        .unwrap();
    assert_eq!(catalog.remove_root(Format::Pi, &first).unwrap(), 1);
    drop(catalog);
    Connection::open(&db)
        .unwrap()
        .execute_batch("VACUUM")
        .unwrap();
    journal(&second, "second", "replacementgarden");
    let mut catalog = Catalog::open(&db).unwrap();
    catalog.scan(&[], 1).unwrap();
    assert!(catalog.find("retiredorchard", None, 20).unwrap().is_empty());
    assert!(catalog.find("originalgarden", None, 20).unwrap().is_empty());
    let found = catalog.find("replacementgarden", None, 20).unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].native_id, "second");
    assert!(
        first.exists(),
        "forgetting a root must not delete native sessions"
    );
}

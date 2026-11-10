use ams::{catalog::Catalog, model::Format};
use serde_json::json;
use std::fs;

#[test]
fn title_search_observes_native_rename_precedence_across_stores() {
    let tmp = tempfile::tempdir().unwrap();
    let mut catalog = Catalog::open(&tmp.path().join("index.db")).unwrap();
    let fixtures = [
        (
            Format::Codex,
            vec![
                json!({"type":"session_meta","payload":{"id":"codex-id","cwd":"/sample","timestamp":"2026-01-01T00:00:00Z"}}),
                json!({"type":"event_msg","payload":{"type":"thread_name_updated","name":"oldtitle"}}),
                json!({"type":"event_msg","payload":{"type":"thread_name_updated","thread_name":"codexneedle"}}),
                json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"ordinary body"}]}}),
            ],
        ),
        (
            Format::Pi,
            vec![
                json!({"type":"session","version":3,"id":"pi-id","cwd":"/sample","timestamp":"2026-01-01T00:00:00Z","title":"oldtitle"}),
                json!({"type":"message","message":{"role":"user","content":"ordinary body"}}),
                json!({"type":"session_info","name":"pineedle"}),
            ],
        ),
        (
            Format::Omp,
            vec![
                json!({"type":"title","v":1,"title":"ompneedle"}),
                json!({"type":"session","version":3,"id":"omp-id","cwd":"/sample","timestamp":"2026-01-01T00:00:00Z","title":"oldtitle"}),
                json!({"type":"message","message":{"role":"user","content":"ordinary body"}}),
                json!({"type":"title_change","title":"oldtitle"}),
            ],
        ),
    ];
    let roots: Vec<_> = fixtures
        .iter()
        .map(|(format, records)| {
            let path = tmp.path().join(format!("{format}.jsonl"));
            fs::write(
                &path,
                records.iter().map(|r| format!("{r}\n")).collect::<String>(),
            )
            .unwrap();
            (*format, path)
        })
        .collect();
    assert_eq!(catalog.scan(&roots, 1).unwrap().errors, 0);
    for (term, format) in [
        ("codexneedle", Format::Codex),
        ("pineedle", Format::Pi),
        ("ompneedle", Format::Omp),
    ] {
        let hits = catalog.find_titles(term, None, 20).unwrap();
        assert_eq!(hits.len(), 1, "{term}");
        assert_eq!(hits[0].format, format);
    }
    assert!(
        catalog
            .find_titles("oldtitle", None, 20)
            .unwrap()
            .is_empty()
    );
}

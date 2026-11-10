use crate::model::Format;
use anyhow::{Context, Result, anyhow, bail};
use rayon::prelude::*;
use rusqlite::{Connection, OpenFlags, OptionalExtension, Row, params};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;
use walkdir::WalkDir;

#[derive(Clone, Debug, Serialize)]
pub struct Entry {
    pub id: String,
    pub format: Format,
    pub native_id: String,
    pub path: PathBuf,
    pub title: String,
    pub cwd: String,
    pub started_at: String,
    pub bytes: u64,
    pub status: String,
    pub error: Option<String>,
    pub summary: String,
}

#[derive(Default, Debug, Serialize)]
pub struct ScanReport {
    pub scanned: usize,
    pub unchanged: usize,
    pub missing: usize,
    pub errors: usize,
    pub files_seen: usize,
    pub roots: usize,
    pub root_errors: Vec<String>,
}

pub struct Catalog {
    pub(crate) db: Connection,
}

const COLUMNS: &str = "e.id,e.format,e.native_id,e.path,e.title,e.cwd,e.started_at,e.bytes,e.status,e.error,e.summary";

impl Catalog {
    pub fn open(path: &Path) -> Result<Self> {
        if path != Path::new(":memory:") {
            if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
                let existed = parent.exists();
                fs::create_dir_all(parent)?;
                #[cfg(unix)]
                if !existed {
                    use std::os::unix::fs::PermissionsExt;
                    fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
                }
            }
            // Create privately before SQLite can create any journal containing transcript text.
            let mut options = OpenOptions::new();
            options.write(true).create(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
                options.mode(0o600);
                if let Ok(meta) = fs::symlink_metadata(path) {
                    if meta.file_type().is_symlink() {
                        bail!("catalog database must not be a symlink");
                    }
                    if meta.permissions().mode() & 0o077 != 0 {
                        bail!(
                            "existing catalog database is not private; set its permissions to 0600"
                        );
                    }
                }
                options.open(path)?;
            }
            #[cfg(not(unix))]
            options.open(path)?;
        }
        let db = Connection::open(path)?;
        db.busy_timeout(std::time::Duration::from_secs(5))?;
        let version: i64 = db.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version > 2 {
            bail!("catalog schema {version} is newer than this ams binary supports");
        }
        if version == 0
            && db.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='entries')",
                [],
                |r| r.get::<_, bool>(0),
            )?
        {
            bail!(
                "unversioned development catalog; select a new --db path and rebuild from native sources"
            );
        }
        db.execute_batch("PRAGMA foreign_keys=ON;
            CREATE TABLE IF NOT EXISTS roots(format TEXT NOT NULL,path TEXT NOT NULL,PRIMARY KEY(format,path));
            CREATE TABLE IF NOT EXISTS entries(
                row_id INTEGER PRIMARY KEY,id TEXT NOT NULL UNIQUE,format TEXT NOT NULL,native_id TEXT NOT NULL,path TEXT NOT NULL,
                title TEXT NOT NULL,cwd TEXT NOT NULL,started_at TEXT NOT NULL,bytes INTEGER NOT NULL,
                status TEXT NOT NULL,error TEXT,summary TEXT NOT NULL,search_text TEXT NOT NULL,
                fingerprint TEXT NOT NULL,content_hash TEXT NOT NULL);
            CREATE INDEX IF NOT EXISTS entries_native ON entries(native_id);
            CREATE TABLE IF NOT EXISTS root_entries(format TEXT NOT NULL,root TEXT NOT NULL,id TEXT NOT NULL,
                PRIMARY KEY(format,root,id),FOREIGN KEY(id) REFERENCES entries(id) ON DELETE CASCADE);
            CREATE VIRTUAL TABLE IF NOT EXISTS sessions_fts USING fts5(id UNINDEXED,title,search_text,project,native_id,tokenize='unicode61');
            CREATE TABLE IF NOT EXISTS embeddings(id TEXT NOT NULL,model TEXT NOT NULL,content_hash TEXT NOT NULL,
                chunk INTEGER NOT NULL,vector BLOB NOT NULL,PRIMARY KEY(id,model,chunk),
                FOREIGN KEY(id) REFERENCES entries(id) ON DELETE CASCADE);
            CREATE TABLE IF NOT EXISTS embedding_models(name TEXT PRIMARY KEY,identity TEXT NOT NULL);")?;
        if version == 1 {
            // Metadata joins the lexical index; readable text and vectors stay unchanged.
            db.execute_batch("BEGIN IMMEDIATE;
                DROP TABLE sessions_fts;
                CREATE VIRTUAL TABLE sessions_fts USING fts5(id UNINDEXED,title,search_text,project,native_id,tokenize='unicode61');
                INSERT INTO sessions_fts(rowid,id,title,search_text,project,native_id)
                    SELECT row_id,id,title,search_text,cwd,native_id FROM entries WHERE status IN ('ok','unavailable');
                PRAGMA user_version=2;
                COMMIT;")?;
        } else if version == 0 {
            db.execute_batch("PRAGMA user_version=2")?;
        }
        Ok(Self { db })
    }

    pub fn roots(&self) -> Result<Vec<(Format, PathBuf)>> {
        let mut statement = self
            .db
            .prepare("SELECT format,path FROM roots ORDER BY format,path")?;
        let rows =
            statement.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        rows.map(|row| {
            let (format, path) = row?;
            Ok((
                format
                    .parse()
                    .map_err(|_| anyhow!("invalid persisted format"))?,
                PathBuf::from(path),
            ))
        })
        .collect()
    }

    pub fn remove_root(&mut self, format: Format, path: &Path) -> Result<usize> {
        let root = absolute(path);
        let tx = self.db.transaction()?;
        let ids = {
            let mut statement =
                tx.prepare("SELECT id FROM root_entries WHERE format=?1 AND root=?2")?;
            let rows = statement
                .query_map(params![format.to_string(), root.to_string_lossy()], |r| {
                    r.get::<_, String>(0)
                })?;
            rows.collect::<std::result::Result<Vec<_>, _>>()?
        };
        tx.execute(
            "DELETE FROM root_entries WHERE format=?1 AND root=?2",
            params![format.to_string(), root.to_string_lossy()],
        )?;
        tx.execute(
            "DELETE FROM roots WHERE format=?1 AND path=?2",
            params![format.to_string(), root.to_string_lossy()],
        )?;
        let mut removed = 0;
        for id in ids {
            let remaining: i64 = tx.query_row(
                "SELECT count(*) FROM root_entries WHERE id=?1",
                [&id],
                |r| r.get(0),
            )?;
            if remaining == 0 {
                tx.execute(
                    "DELETE FROM sessions_fts WHERE rowid=(SELECT row_id FROM entries WHERE id=?1)",
                    [&id],
                )?;
                removed += tx.execute("DELETE FROM entries WHERE id=?1", [&id])?;
            }
        }
        tx.commit()?;
        Ok(removed)
    }

    pub fn scan(&mut self, roots: &[(Format, PathBuf)], jobs: usize) -> Result<ScanReport> {
        // One durable transaction per scan, not one fsync per journal and root link.
        self.db.execute_batch("BEGIN IMMEDIATE")?;
        let result = self.scan_inner(roots, jobs).and_then(|report| {
            self.db.execute_batch("COMMIT")?;
            Ok(report)
        });
        if result.is_err() {
            let _ = self.db.execute_batch("ROLLBACK");
        }
        result
    }

    fn scan_inner(&mut self, roots: &[(Format, PathBuf)], jobs: usize) -> Result<ScanReport> {
        let mut effective = self.roots()?;
        effective.extend_from_slice(roots);
        if effective.is_empty() {
            effective = default_roots();
        }
        let mut unique = BTreeSet::new();
        effective.retain(|(format, path)| unique.insert((format.to_string(), absolute(path))));
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(jobs.max(1))
            .build()?;
        let mut report = ScanReport::default();
        for (format, root) in effective {
            let root = absolute(&root);
            self.db.execute(
                "INSERT OR IGNORE INTO roots(format,path) VALUES(?1,?2)",
                params![format.to_string(), root.to_string_lossy()],
            )?;
            if !root.exists() {
                report
                    .root_errors
                    .push(format!("{} {}: root unavailable", format, root.display()));
                report.errors += 1;
                self.mark_unavailable(format, &root, None)?;
                continue;
            }
            report.roots += 1;
            let mut complete = true;
            let mut paths = Vec::new();
            if root.is_file() {
                paths.push(root.clone());
            } else {
                let walker = WalkDir::new(&root)
                    .follow_links(false)
                    .into_iter()
                    .filter_entry(|item| {
                        !matches!(format, Format::Opencode)
                            || item.depth() == 0
                            || !item.file_type().is_dir()
                            || !matches!(
                                item.file_name().to_str(),
                                Some(
                                    "storage"
                                        | "snapshot"
                                        | "node_modules"
                                        | "shell"
                                        | "repos"
                                        | "tool-output"
                                        | "log"
                                )
                            )
                    });
                for item in walker {
                    match item {
                        Ok(item) if item.file_type().is_file() => {
                            let path = item.path();
                            let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
                            if ext == "jsonl"
                                || (matches!(format, Format::Opencode)
                                    && (ext == "json"
                                        || path.file_name().and_then(|s| s.to_str())
                                            == Some("opencode.db")))
                            {
                                // OpenCode auth/configuration and storage sidecars are not exported sessions.
                                if matches!(format, Format::Opencode)
                                    && ext == "json"
                                    && !is_export_candidate(path)
                                {
                                    continue;
                                }
                                paths.push(path.to_path_buf());
                            }
                        }
                        Ok(_) => {}
                        Err(_) => {
                            complete = false;
                            report.errors += 1;
                            report.root_errors.push(format!(
                                "{} {}: directory traversal failed",
                                format,
                                root.display()
                            ));
                        }
                    }
                }
            }
            if !complete {
                self.mark_unavailable(format, &root, None)?;
            }
            paths.sort();
            paths.dedup();
            report.files_seen += paths.len();
            let mut seen = BTreeSet::new();
            let mut changed = Vec::new();
            for path in paths {
                if matches!(format, Format::Opencode)
                    && matches!(
                        path.extension().and_then(|s| s.to_str()),
                        Some("db" | "sqlite" | "sqlite3")
                    )
                {
                    match self.scan_opencode(&path, &root, &mut report) {
                        Ok(ids) => seen.extend(ids),
                        Err(_) => {
                            // A failed inventory must never mark all of its sessions missing.
                            complete = false;
                            report.errors += 1;
                            report.root_errors.push(format!(
                                "opencode {}: database inventory failed or schema unsupported",
                                path.display()
                            ));
                            self.mark_unavailable(format, &root, Some(&path))?;
                        }
                    }
                    continue;
                }
                let id = entry_id(format, &path, "");
                seen.insert(id.clone());
                match fingerprint(&path) {
                    Ok((fingerprint, bytes)) => {
                        let previous: Option<(String, String)> = self
                            .db
                            .query_row(
                                "SELECT fingerprint,status FROM entries WHERE id=?1",
                                [&id],
                                |r| Ok((r.get(0)?, r.get(1)?)),
                            )
                            .optional()?;
                        if previous
                            .as_ref()
                            .is_some_and(|(old, status)| old == &fingerprint && status == "ok")
                        {
                            report.unchanged += 1;
                            self.link_root(format, &root, &id)?;
                        } else {
                            changed.push((path, id, fingerprint, bytes));
                        }
                    }
                    Err(_) => {
                        report.errors += 1;
                        self.store(
                            &failed_entry(
                                format,
                                &path,
                                id.clone(),
                                0,
                                "source metadata unavailable",
                            ),
                            "",
                            "",
                            "",
                        )?;
                        self.link_root(format, &root, &id)?;
                    }
                }
            }
            // Bound retained transcript memory to a small multiple of worker count.
            for batch in changed.chunks(jobs.max(1).saturating_mul(2)) {
                let extracted: Vec<_> = pool.install(|| {
                    batch
                        .par_iter()
                        .map(|(path, id, fingerprint, bytes)| {
                            let parsed = extract_file(path, format, id.clone(), *bytes);
                            (path, id, fingerprint, bytes, parsed)
                        })
                        .collect()
                });
                for (path, id, fingerprint, bytes, parsed) in extracted {
                    report.scanned += 1;
                    match parsed {
                        Ok((entry, text)) => self.store(
                            &entry,
                            &text,
                            fingerprint,
                            &content_hash(&entry.title, &text),
                        )?,
                        Err(error) => {
                            report.errors += 1;
                            self.store(
                                &failed_entry(format, path, id.clone(), *bytes, &error.to_string()),
                                "",
                                fingerprint,
                                "",
                            )?;
                        }
                    }
                    self.link_root(format, &root, id)?;
                }
            }
            if complete {
                let mut statement = self
                    .db
                    .prepare("SELECT id FROM root_entries WHERE format=?1 AND root=?2")?;
                let old = statement
                    .query_map(params![format.to_string(), root.to_string_lossy()], |r| {
                        r.get::<_, String>(0)
                    })?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                drop(statement);
                for id in old.into_iter().filter(|id| !seen.contains(id)) {
                    let changed = self.db.execute("UPDATE entries SET status='missing',error='source no longer present' WHERE id=?1 AND status!='missing'", [&id])?;
                    report.missing += changed;
                    self.db
                        .execute("DELETE FROM sessions_fts WHERE rowid=(SELECT row_id FROM entries WHERE id=?1)", [&id])?;
                    self.db
                        .execute("DELETE FROM embeddings WHERE id=?1", [&id])?;
                }
            }
        }
        Ok(report)
    }

    fn link_root(&self, format: Format, root: &Path, id: &str) -> Result<()> {
        self.db.execute(
            "INSERT OR IGNORE INTO root_entries(format,root,id) VALUES(?1,?2,?3)",
            params![format.to_string(), root.to_string_lossy(), id],
        )?;
        Ok(())
    }

    fn mark_unavailable(&self, format: Format, root: &Path, path: Option<&Path>) -> Result<()> {
        self.db.execute("UPDATE entries SET status='unavailable',error='source root or inventory unavailable; showing last indexed content' WHERE status IN ('ok','unavailable') AND id IN (SELECT id FROM root_entries WHERE format=?1 AND root=?2) AND (?3 IS NULL OR path=?3)",
            params![format.to_string(),root.to_string_lossy(),path.map(|p|p.to_string_lossy().into_owned())])?;
        Ok(())
    }

    fn store(&mut self, entry: &Entry, text: &str, fingerprint: &str, hash: &str) -> Result<()> {
        let tx = &self.db;
        tx.execute("INSERT INTO entries(id,format,native_id,path,title,cwd,started_at,bytes,status,error,summary,search_text,fingerprint,content_hash)
            VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)
            ON CONFLICT(id) DO UPDATE SET
            native_id=CASE WHEN excluded.native_id='' THEN entries.native_id ELSE excluded.native_id END,
            title=CASE WHEN excluded.status='error' THEN entries.title ELSE excluded.title END,
            cwd=CASE WHEN excluded.cwd='' THEN entries.cwd ELSE excluded.cwd END,
            started_at=CASE WHEN excluded.started_at='' THEN entries.started_at ELSE excluded.started_at END,
            bytes=excluded.bytes,status=excluded.status,error=excluded.error,
            summary=excluded.summary,search_text=excluded.search_text,fingerprint=excluded.fingerprint,content_hash=excluded.content_hash",
            params![entry.id,entry.format.to_string(),entry.native_id,entry.path.to_string_lossy(),entry.title,entry.cwd,entry.started_at,entry.bytes as i64,entry.status,entry.error,entry.summary,text,fingerprint,hash])?;
        tx.execute(
            "DELETE FROM sessions_fts WHERE rowid=(SELECT row_id FROM entries WHERE id=?1)",
            [&entry.id],
        )?;
        if entry.status == "ok" {
            tx.execute(
                "INSERT INTO sessions_fts(rowid,id,title,search_text,project,native_id) SELECT row_id,id,title,search_text,cwd,native_id FROM entries WHERE id=?1",
                [&entry.id],
            )?;
        }
        tx.execute(
            "DELETE FROM embeddings WHERE id=?1 AND content_hash!=?2",
            params![entry.id, hash],
        )?;
        Ok(())
    }

    pub fn list(&self, format: Option<Format>, limit: usize) -> Result<Vec<Entry>> {
        let sql = format!(
            "SELECT {COLUMNS} FROM entries e WHERE (?1 IS NULL OR e.format=?1) ORDER BY e.started_at DESC,e.id LIMIT ?2"
        );
        let mut statement = self.db.prepare(&sql)?;
        let rows = statement.query_map(
            params![format.map(|f| f.to_string()), bounded_limit(limit)],
            row_entry,
        )?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn find(&self, query: &str, format: Option<Format>, limit: usize) -> Result<Vec<Entry>> {
        self.find_impl(query, format, limit, false)
    }

    pub fn find_titles(
        &self,
        query: &str,
        format: Option<Format>,
        limit: usize,
    ) -> Result<Vec<Entry>> {
        self.find_impl(query, format, limit, true)
    }

    fn find_impl(
        &self,
        query: &str,
        format: Option<Format>,
        limit: usize,
        titles_only: bool,
    ) -> Result<Vec<Entry>> {
        let tokens: Vec<_> = query
            .split(|c: char| !c.is_alphanumeric())
            .filter(|s| !s.is_empty())
            .map(|s| format!("\"{}\"", s.replace('"', "\"\"")))
            .collect();
        if tokens.is_empty() {
            bail!("search query must contain a word or number");
        }
        let expression = if titles_only {
            format!("title : ({})", tokens.join(" AND "))
        } else {
            tokens.join(" AND ")
        };
        let sql = format!(
            "SELECT {COLUMNS},snippet(sessions_fts,2,'[',']',' … ',28) FROM sessions_fts JOIN entries e ON e.row_id=sessions_fts.rowid WHERE sessions_fts MATCH ?1 AND e.status IN ('ok','unavailable') AND (?2 IS NULL OR e.format=?2) ORDER BY bm25(sessions_fts,0.0,4.0,1.0),e.started_at DESC,e.id LIMIT ?3"
        );
        let mut statement = self.db.prepare(&sql)?;
        let rows = statement.query_map(
            params![
                expression,
                format.map(|f| f.to_string()),
                bounded_limit(limit)
            ],
            |row| {
                let mut entry = row_entry(row)?;
                if !titles_only {
                    let snippet: String = row.get(11)?;
                    if !snippet.is_empty() {
                        entry.summary = snippet;
                    }
                }
                Ok(entry)
            },
        )?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn resolve(&self, id: &str) -> Result<Entry> {
        if id.is_empty() {
            bail!("session ID must not be empty");
        }
        let sql = format!(
            "SELECT {COLUMNS} FROM entries e WHERE e.id=?1 OR e.native_id=?1 ORDER BY e.id"
        );
        let mut statement = self.db.prepare(&sql)?;
        let exact = statement
            .query_map([id], row_entry)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        if exact.len() == 1 {
            return Ok(exact.into_iter().next().unwrap());
        }
        if exact.len() > 1 {
            bail!(
                "ambiguous session ID: {} matches; use a catalog ID",
                exact.len()
            );
        }
        let sql = format!(
            "SELECT {COLUMNS} FROM entries e WHERE substr(e.id,1,length(?1))=?1 OR substr(e.native_id,1,length(?1))=?1 ORDER BY e.id LIMIT 2"
        );
        let mut statement = self.db.prepare(&sql)?;
        let entries = statement
            .query_map([id], row_entry)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        match entries.len() {
            0 => bail!("session not found: {id}"),
            1 => Ok(entries.into_iter().next().unwrap()),
            _ => bail!("ambiguous session prefix: {id}; use a longer catalog ID"),
        }
    }

    #[cfg(feature = "semantic")]
    pub(crate) fn semantic_entries(&self, format: Option<Format>) -> Result<Vec<(Entry, String)>> {
        let sql = format!(
            "SELECT {COLUMNS},e.content_hash FROM entries e WHERE e.status='ok' AND (?1 IS NULL OR e.format=?1) ORDER BY e.id"
        );
        let mut statement = self.db.prepare(&sql)?;
        let rows = statement.query_map([format.map(|f| f.to_string())], |r| {
            Ok((row_entry(r)?, r.get(11)?))
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    fn scan_opencode(
        &mut self,
        path: &Path,
        root: &Path,
        report: &mut ScanReport,
    ) -> Result<Vec<String>> {
        fingerprint(path)?;
        let source = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        source.busy_timeout(std::time::Duration::from_secs(2))?;
        source.execute_batch("PRAGMA query_only=ON; BEGIN")?;
        let mut tables = Vec::new();
        for table in ["session_v2", "session"] {
            let columns = table_columns(&source, table)?;
            if columns.is_empty() {
                continue;
            }
            if !["id", "title", "directory", "time_created", "time_updated"]
                .iter()
                .all(|c| columns.contains(*c))
            {
                bail!("unsupported OpenCode session schema");
            }
            tables.push(table);
        }
        if tables.is_empty() {
            bail!("unsupported OpenCode database schema");
        }
        let mut ids = Vec::new();
        for table in &tables {
            let legacy = *table == "session";
            let (messages, parts) = if legacy {
                ("message", "part")
            } else {
                ("session_message", "session_part")
            };
            let message_columns = table_columns(&source, messages)?;
            if !["id", "session_id", "data", "time_updated"]
                .iter()
                .all(|c| message_columns.contains(*c))
            {
                bail!("unsupported OpenCode message schema");
            }
            let part_columns = table_columns(&source, parts)?;
            if !part_columns.is_empty()
                && !["message_id", "session_id", "data", "time_updated"]
                    .iter()
                    .all(|c| part_columns.contains(*c))
            {
                bail!("unsupported OpenCode part schema");
            }
            let filter = if legacy && tables.contains(&"session_v2") {
                " WHERE NOT EXISTS(SELECT 1 FROM session_v2 v WHERE v.id=session.id)"
            } else {
                ""
            };
            let sql = format!(
                "SELECT id,COALESCE(title,''),directory,time_created,time_updated FROM {table}{filter} ORDER BY id"
            );
            let mut statement = source.prepare(&sql)?;
            let sessions = statement
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, i64>(3)?,
                        r.get::<_, i64>(4)?,
                    ))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            for (native_id, title, cwd, created, updated) in sessions {
                let id = entry_id(Format::Opencode, path, &native_id);
                ids.push(id.clone());
                // Count/update aggregates catch body appends/edits even when session timestamps lag.
                let message_state: (i64,i64) = source.query_row(&format!("SELECT count(*),COALESCE(max(time_updated),0) FROM {messages} WHERE session_id=?1"), [&native_id], |r| Ok((r.get(0)?,r.get(1)?)))?;
                let part_state: (i64, i64) = if part_columns.is_empty() {
                    (0, 0)
                } else {
                    source.query_row(&format!("SELECT count(*),COALESCE(max(time_updated),0) FROM {parts} WHERE session_id=?1"), [&native_id], |r| Ok((r.get(0)?,r.get(1)?)))?
                };
                let state = format!(
                    "{table}|{title}|{cwd}|{created}|{updated}|{message_state:?}|{part_state:?}"
                );
                let old: Option<(String, String)> = self
                    .db
                    .query_row(
                        "SELECT fingerprint,status FROM entries WHERE id=?1",
                        [&id],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .optional()?;
                if old.is_some_and(|(old, status)| old == state && status == "ok") {
                    report.unchanged += 1;
                    self.link_root(Format::Opencode, root, &id)?;
                    continue;
                }
                report.scanned += 1;
                let extraction = opencode_text(
                    &source,
                    messages,
                    parts,
                    &native_id,
                    legacy,
                    !part_columns.is_empty(),
                );
                match extraction {
                    Ok((text, bytes)) => {
                        let entry = Entry {
                            id: id.clone(),
                            format: Format::Opencode,
                            native_id,
                            path: path.to_path_buf(),
                            title,
                            cwd,
                            started_at: iso_millis(created),
                            bytes,
                            status: "ok".into(),
                            error: None,
                            summary: bounded(&text, 600),
                        };
                        self.store(&entry, &text, &state, &content_hash(&entry.title, &text))?;
                    }
                    Err(_) => {
                        report.errors += 1;
                        let mut entry = failed_entry(
                            Format::Opencode,
                            path,
                            id.clone(),
                            0,
                            "invalid OpenCode conversation data",
                        );
                        entry.native_id = native_id;
                        entry.title = title;
                        entry.cwd = cwd;
                        entry.started_at = iso_millis(created);
                        self.store(&entry, "", &state, "")?;
                    }
                }
                self.link_root(Format::Opencode, root, &id)?;
            }
        }
        Ok(ids)
    }
}

fn row_entry(row: &Row<'_>) -> rusqlite::Result<Entry> {
    let raw: String = row.get(1)?;
    let format = raw.parse().map_err(|_| rusqlite::Error::InvalidQuery)?;
    Ok(Entry {
        id: row.get(0)?,
        format,
        native_id: row.get(2)?,
        path: PathBuf::from(row.get::<_, String>(3)?),
        title: row.get(4)?,
        cwd: row.get(5)?,
        started_at: row.get(6)?,
        bytes: row.get::<_, i64>(7)?.max(0) as u64,
        status: row.get(8)?,
        error: row.get(9)?,
        summary: row.get(10)?,
    })
}

pub fn default_roots() -> Vec<(Format, PathBuf)> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let env_home = |name: &str, fallback: PathBuf| {
        std::env::var_os(name)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .unwrap_or(fallback)
    };
    let claude = env_home("CLAUDE_CONFIG_DIR", home.join(".claude"));
    let codex = env_home("CODEX_HOME", home.join(".codex"));
    let opencode = env_home("XDG_DATA_HOME", home.join(".local/share")).join("opencode");
    let mut roots = vec![
        (Format::Claude, claude.join("projects")),
        (Format::Codex, codex.join("sessions")),
        (Format::Codex, codex.join("archived_sessions")),
        (Format::Pi, home.join(".pi/agent/sessions")),
        (Format::Omp, home.join(".omp/agent/sessions")),
        (Format::Omp, home.join(".omp/sessions")),
        (Format::Opencode, opencode),
    ];
    if let Some(configured) = std::env::var_os("PI_CODING_AGENT_DIR").filter(|v| !v.is_empty()) {
        let sessions = PathBuf::from(configured).join("sessions");
        roots.push((family_root_format(&sessions), sessions));
    }
    if let Ok(profiles) = fs::read_dir(home.join(".omp/profiles")) {
        for profile in profiles.flatten() {
            if profile.file_type().is_ok_and(|kind| kind.is_dir()) {
                roots.push((Format::Omp, profile.path().join("agent/sessions")));
                roots.push((Format::Omp, profile.path().join("sessions")));
            }
        }
    }
    roots.retain(|(_, p)| p.exists());
    roots.sort_by(|a, b| {
        a.0.to_string()
            .cmp(&b.0.to_string())
            .then_with(|| a.1.cmp(&b.1))
    });
    roots.dedup();
    roots
}

fn family_root_format(path: &Path) -> Format {
    for candidate in WalkDir::new(path)
        .max_depth(3)
        .into_iter()
        .filter_map(std::result::Result::ok)
    {
        if candidate.file_type().is_file()
            && candidate.path().extension().and_then(|e| e.to_str()) == Some("jsonl")
            && let Ok(file) = File::open(candidate.path())
            && let Some(Ok(line)) = BufReader::new(file.take(16 * 1024)).lines().next()
            && let Ok(value) = serde_json::from_str::<Value>(&line)
        {
            if string(&value, "type") == "title" {
                return Format::Omp;
            }
            if string(&value, "type") == "session" {
                return Format::Pi;
            }
        }
    }
    if path.components().any(|part| part.as_os_str() == ".omp") {
        Format::Omp
    } else {
        Format::Pi
    }
}

fn absolute(path: &Path) -> PathBuf {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    };
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}
fn bounded_limit(limit: usize) -> i64 {
    limit.min(i64::MAX as usize) as i64
}
fn bounded(text: &str, limit: usize) -> String {
    text.chars().take(limit).collect()
}
fn content_hash(title: &str, text: &str) -> String {
    let mut hash = blake3::Hasher::new();
    hash.update(title.as_bytes());
    hash.update(&[0]);
    hash.update(text.as_bytes());
    hash.finalize().to_hex().to_string()
}
fn entry_id(format: Format, path: &Path, native_id: &str) -> String {
    let mut hash = blake3::Hasher::new();
    hash.update(format.to_string().as_bytes());
    hash.update(&[0]);
    hash.update(path.as_os_str().as_encoded_bytes());
    hash.update(&[0]);
    hash.update(native_id.as_bytes());
    hash.finalize().to_hex().to_string()
}
fn fingerprint(path: &Path) -> Result<(String, u64)> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        bail!("source is not a regular file");
    }
    let modified = metadata.modified()?.duration_since(UNIX_EPOCH)?.as_nanos();
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok((
            format!(
                "{}:{modified}:{}:{}:{}:{}",
                metadata.len(),
                metadata.dev(),
                metadata.ino(),
                metadata.ctime(),
                metadata.ctime_nsec()
            ),
            metadata.len(),
        ))
    }
    #[cfg(not(unix))]
    Ok((format!("{}:{modified}", metadata.len()), metadata.len()))
}
fn failed_entry(format: Format, path: &Path, id: String, bytes: u64, error: &str) -> Entry {
    Entry {
        id,
        format,
        native_id: String::new(),
        path: path.to_path_buf(),
        title: path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
        cwd: String::new(),
        started_at: String::new(),
        bytes,
        status: "error".into(),
        error: Some(error.into()),
        summary: String::new(),
    }
}
fn string<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}
fn append_text(output: &mut String, text: &str) {
    if !text.is_empty() {
        if !output.is_empty() {
            output.push('\n');
        }
        output.push_str(text);
    }
}
// Deliberately shallow: never descend into tools, reasoning, provider state, or credentials.
fn readable_content(output: &mut String, value: &Value) {
    if let Some(text) = value.as_str() {
        append_text(output, text);
    } else if let Some(items) = value.as_array() {
        for item in items {
            if matches!(string(item, "type"), "text" | "input_text" | "output_text") {
                append_text(output, string(item, "text"));
            }
        }
    }
}

fn extract_file(path: &Path, format: Format, id: String, bytes: u64) -> Result<(Entry, String)> {
    if matches!(format, Format::Opencode) {
        return extract_export(path, id, bytes);
    }
    let mut entry = failed_entry(format, path, id, bytes, "");
    entry.status = "ok".into();
    entry.error = None;
    entry.title.clear();
    let mut text = String::new();
    let mut recognized = false;
    let mut title_slot = false;
    let mut mirrored_text = String::new();
    let mut canonical_messages = false;
    let reader = BufReader::new(File::open(path).context("source cannot be opened")?);
    for (number, line) in reader.lines().enumerate() {
        let line = line.map_err(|_| anyhow!("source cannot be read at line {}", number + 1))?;
        if line.trim().is_empty() {
            continue;
        }
        let v: Value = serde_json::from_str(&line)
            .map_err(|_| anyhow!("invalid JSON record at line {}", number + 1))?;
        if !v.is_object() {
            bail!("non-object JSON record at line {}", number + 1);
        }
        let kind = string(&v, "type");
        match format {
            Format::Omp | Format::Pi => match kind {
                "title" | "session_info" | "title_change" => {
                    if kind == "title" || !title_slot {
                        let title = string(&v, "title");
                        let title = if title.is_empty() {
                            string(&v, "name")
                        } else {
                            title
                        };
                        if !title.trim().is_empty() {
                            entry.title = title.trim().into();
                            title_slot |= kind == "title";
                        }
                    }
                }
                "session" => {
                    recognized = true;
                    entry.native_id = string(&v, "id").into();
                    entry.cwd = string(&v, "cwd").into();
                    entry.started_at = string(&v, "timestamp").into();
                    if entry.title.is_empty() {
                        entry.title = string(&v, "title").into();
                    }
                }
                "message" => {
                    if let Some(message) = v.get("message")
                        && matches!(string(message, "role"), "user" | "assistant")
                    {
                        readable_content(&mut text, &message["content"]);
                    }
                }
                _ => {}
            },
            Format::Codex => {
                let payload = &v["payload"];
                if kind == "session_meta" {
                    recognized = true;
                    entry.native_id = string(payload, "id").into();
                    entry.cwd = string(payload, "cwd").into();
                    entry.started_at = string(payload, "timestamp").into();
                    if entry.started_at.is_empty() {
                        entry.started_at = string(&v, "timestamp").into();
                    }
                }
                if kind == "event_msg" && string(payload, "type") == "thread_name_updated" {
                    let name = string(payload, "name");
                    let name = if name.is_empty() {
                        string(payload, "thread_name")
                    } else {
                        name
                    };
                    if !name.trim().is_empty() {
                        entry.title = name.trim().into();
                    }
                }
                if kind == "response_item"
                    && string(payload, "type") == "message"
                    && matches!(string(payload, "role"), "user" | "assistant")
                {
                    canonical_messages = true;
                    mirrored_text.clear();
                    readable_content(&mut text, &payload["content"]);
                }
                // Legacy rollout JSONL has direct message records rather than response_item wrappers.
                if kind == "message" && matches!(string(&v, "role"), "user" | "assistant") {
                    recognized = true;
                    canonical_messages = true;
                    mirrored_text.clear();
                    readable_content(&mut text, &v["content"]);
                }
                if !canonical_messages
                    && kind == "event_msg"
                    && matches!(string(payload, "type"), "user_message" | "agent_message")
                {
                    append_text(&mut mirrored_text, string(payload, "message"));
                }
            }
            Format::Claude => {
                if !string(&v, "sessionId").is_empty() {
                    recognized = true;
                    entry.native_id = string(&v, "sessionId").into();
                }
                if !string(&v, "cwd").is_empty() {
                    entry.cwd = string(&v, "cwd").into();
                }
                if entry.started_at.is_empty() {
                    entry.started_at = string(&v, "timestamp").into();
                }
                if kind == "custom-title" && !string(&v, "customTitle").is_empty() {
                    entry.title = string(&v, "customTitle").into();
                }
                if matches!(kind, "user" | "assistant") {
                    let message = &v["message"];
                    if matches!(string(message, "role"), "user" | "assistant") {
                        readable_content(&mut text, &message["content"]);
                    }
                }
            }
            Format::Opencode => unreachable!(),
        }
    }
    if !canonical_messages && format == Format::Codex {
        text = mirrored_text;
    }
    if !recognized {
        bail!("no supported native session header or identity");
    }
    if entry.native_id.is_empty() {
        bail!("native session ID is missing");
    }
    if entry.title.is_empty() {
        entry.title = bounded(
            text.lines()
                .find(|line| !line.trim().is_empty())
                .unwrap_or("Untitled session"),
            200,
        );
    }
    entry.summary = bounded(&text, 600);
    Ok((entry, text))
}

fn is_export_candidate(path: &Path) -> bool {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if matches!(
        name,
        "auth.json"
            | "account.json"
            | "opencode.json"
            | "opencode.jsonc"
            | "package.json"
            | "config.json"
    ) {
        return false;
    }
    // Native storage JSON records are not public exported bundles.
    !path.components().any(|p| {
        matches!(
            p.as_os_str().to_str(),
            Some("storage" | "snapshot" | "node_modules" | "shell" | "repos" | "tool-output")
        )
    })
}

fn extract_export(path: &Path, id: String, bytes: u64) -> Result<(Entry, String)> {
    let v: Value = serde_json::from_reader(BufReader::new(File::open(path)?))
        .map_err(|_| anyhow!("invalid OpenCode exported JSON"))?;
    let info = v
        .get("info")
        .or_else(|| v.get("session"))
        .ok_or_else(|| anyhow!("not an OpenCode exported session bundle"))?;
    let native_id = string(info, "id");
    if !native_id.starts_with("ses_") {
        bail!("invalid OpenCode exported session identity");
    }
    let messages = v
        .get("messages")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("OpenCode export has no messages array"))?;
    let mut text = String::new();
    for message in messages {
        let info = message.get("info").unwrap_or(message);
        let role = info
            .get("role")
            .or_else(|| info.get("type"))
            .and_then(Value::as_str)
            .unwrap_or("");
        if !matches!(role, "user" | "assistant") {
            continue;
        }
        if let Some(parts) = message.get("parts") {
            readable_content(&mut text, parts);
        } else {
            readable_content(&mut text, &info["content"]);
            append_text(&mut text, string(info, "text"));
        }
    }
    let created = info
        .pointer("/time/created")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let title = if string(info, "title").is_empty() {
        bounded(&text, 200)
    } else {
        string(info, "title").into()
    };
    let entry = Entry {
        id,
        format: Format::Opencode,
        native_id: native_id.into(),
        path: path.to_path_buf(),
        title,
        cwd: string(info, "directory").into(),
        started_at: iso_millis(created),
        bytes,
        status: "ok".into(),
        error: None,
        summary: bounded(&text, 600),
    };
    Ok((entry, text))
}

fn table_columns(source: &Connection, table: &str) -> Result<BTreeSet<String>> {
    let mut statement = source.prepare(&format!("PRAGMA table_info({table})"))?;
    let rows = statement.query_map([], |r| r.get::<_, String>(1))?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}
fn iso_millis(value: i64) -> String {
    chrono::DateTime::from_timestamp_millis(value)
        .map(|t| t.to_rfc3339())
        .unwrap_or_default()
}

fn opencode_text(
    source: &Connection,
    messages: &str,
    parts: &str,
    id: &str,
    legacy: bool,
    has_parts: bool,
) -> Result<(String, u64)> {
    let role_expr = if legacy {
        "json_extract(data,'$.role')"
    } else {
        "type"
    };
    let order = if legacy { "time_created,id" } else { "seq,id" };
    let mut statement = source.prepare(&format!(
        "SELECT id,{role_expr},data FROM {messages} WHERE session_id=?1 ORDER BY {order}"
    ))?;
    let mut rows = statement.query([id])?;
    let mut text = String::new();
    let mut bytes = 0u64;
    while let Some(row) = rows.next()? {
        let message_id: String = row.get(0)?;
        let role: Option<String> = row.get(1)?;
        if !matches!(role.as_deref(), Some("user" | "assistant")) {
            continue;
        }
        let raw: String = row.get(2)?;
        bytes += raw.len() as u64;
        let value: Value = serde_json::from_str(&raw)?;
        append_text(&mut text, string(&value, "text"));
        readable_content(&mut text, &value["content"]);
        if has_parts {
            let part_order = if legacy { "time_created,id" } else { "ordinal" };
            let part_type = if legacy {
                "json_extract(data,'$.type')"
            } else {
                "type"
            };
            let mut part_statement=source.prepare(&format!("SELECT data,{part_type} FROM {parts} WHERE session_id=?1 AND message_id=?2 ORDER BY {part_order}"))?;
            let raw_parts = part_statement.query_map(params![id, message_id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
            })?;
            for raw in raw_parts {
                let (raw, kind) = raw?;
                bytes += raw.len() as u64;
                if kind.as_deref() != Some("text") {
                    continue;
                }
                let value: Value = serde_json::from_str(&raw)?;
                append_text(&mut text, string(&value, "text"));
            }
        }
    }
    Ok((text, bytes))
}

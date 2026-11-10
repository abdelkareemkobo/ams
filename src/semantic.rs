use crate::catalog::{Catalog, Entry};
use crate::model::Format;
use anyhow::{Result, bail};

#[cfg(feature = "semantic")]
mod enabled {
    use super::*;
    use anyhow::{Context, anyhow};
    use model2vec_rs::model::StaticModel;
    use rusqlite::{OptionalExtension, params};
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::{Path, PathBuf};

    const CHUNK_CHARS: usize = 1800;
    const OVERLAP: usize = 200;
    const MAX_SEARCH_CHUNKS: usize = 2_000_000;

    // This path is only reached by explicit embed/ask commands, never catalog opening or find.
    fn load_model(name: &str) -> Result<(StaticModel, String)> {
        let folder = if Path::new(name).is_dir() {
            PathBuf::from(name)
        } else {
            if cached_model(name).is_none() {
                // Model2Vec's hub implementation fetches assets only on an explicit model request.
                let _ = StaticModel::from_pretrained(name, None, Some(true), None)
                    .with_context(|| format!("cannot download requested embedding model {name}"))?;
            }
            cached_model(name).ok_or_else(|| anyhow!("model was fetched but its pinned cache snapshot cannot be located; supply a local model directory"))?
        };
        let (tokenizer,weights,config) = model_files(&folder)
            .ok_or_else(|| anyhow!("embedding model directory lacks tokenizer.json, model.safetensors, or configuration"))?;
        let tokenizer = fs::read(tokenizer)?;
        let weights = fs::read(weights)?;
        let config = fs::read(config)?;
        let mut hash = blake3::Hasher::new();
        // Include implementation/chunk policy so changed preprocessing cannot reuse old vectors.
        hash.update(b"ams-model2vec-0.4-normalized-chunks1800-overlap200-v1\0");
        for bytes in [&tokenizer, &weights, &config] {
            hash.update(&(bytes.len() as u64).to_le_bytes());
            hash.update(bytes);
        }
        let identity = hash.finalize().to_hex().to_string();
        let model = StaticModel::from_bytes(tokenizer, weights, config, Some(true))
            .context("cannot load requested Model2Vec assets")?;
        Ok((model, identity))
    }

    fn cached_model(name: &str) -> Option<PathBuf> {
        let home = std::env::var_os("HOME").map(PathBuf::from)?;
        let hf_home = std::env::var_os("HF_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                std::env::var_os("XDG_CACHE_HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| home.join(".cache"))
                    .join("huggingface")
            });
        let hub = std::env::var_os("HF_HUB_CACHE")
            .map(PathBuf::from)
            .unwrap_or_else(|| hf_home.join("hub"));
        let repo = hub.join(format!("models--{}", name.replace('/', "--")));
        let revision = fs::read_to_string(repo.join("refs/main")).ok()?;
        let revision = revision.trim();
        if revision.is_empty() || !revision.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        let folder = repo.join("snapshots").join(revision);
        model_files(&folder).map(|_| folder)
    }

    fn model_files(folder: &Path) -> Option<(PathBuf, PathBuf, PathBuf)> {
        let sub = folder.join("0_StaticEmbedding");
        for (config_base, model_base, config_name) in [
            (folder, folder, "config.json"),
            (folder, folder, "config_sentence_transformers.json"),
            (folder, sub.as_path(), "config_sentence_transformers.json"),
            (
                folder.parent().unwrap_or(folder),
                folder,
                "config_sentence_transformers.json",
            ),
        ] {
            let tokenizer = model_base.join("tokenizer.json");
            let weights = model_base.join("model.safetensors");
            let config = config_base.join(config_name);
            if tokenizer.is_file() && weights.is_file() && config.is_file() {
                return Some((tokenizer, weights, config));
            }
        }
        None
    }

    fn chunks(title: &str, text: &str) -> Vec<String> {
        let mut result = vec![title.to_owned()];
        let mut start = 0;
        while start < text.len() {
            let remaining = &text[start..];
            let end = remaining
                .char_indices()
                .nth(CHUNK_CHARS)
                .map(|(i, _)| i)
                .unwrap_or(remaining.len());
            result.push(remaining[..end].to_owned());
            if end == remaining.len() {
                break;
            }
            let next = remaining[..end]
                .char_indices()
                .rev()
                .nth(OVERLAP - 1)
                .map(|(i, _)| i)
                .unwrap_or(end);
            start += next.max(1);
        }
        result
    }

    fn normalize(vector: &mut [f32]) -> Result<()> {
        let norm = vector
            .iter()
            .map(|v| (*v as f64) * (*v as f64))
            .sum::<f64>()
            .sqrt();
        if !norm.is_finite() {
            bail!("embedding model returned non-finite values");
        }
        if norm > 0.0 {
            for value in vector {
                *value = (*value as f64 / norm) as f32;
            }
        }
        Ok(())
    }

    pub fn index(catalog: &mut Catalog, name: &str) -> Result<serde_json::Value> {
        let (model, identity) = load_model(name)?;
        let old_identity: Option<String> = catalog
            .db
            .query_row(
                "SELECT identity FROM embedding_models WHERE name=?1",
                [name],
                |r| r.get(0),
            )
            .optional()?;
        let entries = catalog.semantic_entries(None)?;
        let mut indexed = 0usize;
        let mut unchanged = 0usize;
        let mut chunk_count = 0usize;
        let tx = catalog.db.transaction()?;
        if old_identity.as_deref() != Some(identity.as_str()) {
            tx.execute("DELETE FROM embeddings WHERE model=?1", [name])?;
        }
        for (entry, hash) in entries {
            let existing: i64 = tx.query_row(
                "SELECT count(*) FROM embeddings WHERE id=?1 AND model=?2 AND content_hash=?3",
                params![entry.id, name, hash],
                |r| r.get(0),
            )?;
            if existing > 0 {
                unchanged += 1;
                continue;
            }
            tx.execute(
                "DELETE FROM embeddings WHERE id=?1 AND model=?2",
                params![entry.id, name],
            )?;
            let text: String = tx.query_row(
                "SELECT search_text FROM entries WHERE id=?1",
                [&entry.id],
                |r| r.get(0),
            )?;
            let chunks = chunks(&entry.title, &text);
            for (batch_index, batch) in chunks.chunks(64).enumerate() {
                let vectors = model.encode_with_args(batch, None, 64);
                if vectors.len() != batch.len() {
                    bail!("embedding model returned an invalid batch size");
                }
                for (offset, mut vector) in vectors.into_iter().enumerate() {
                    normalize(&mut vector)?;
                    let bytes: Vec<u8> = vector.iter().flat_map(|v| v.to_le_bytes()).collect();
                    tx.execute("INSERT INTO embeddings(id,model,content_hash,chunk,vector) VALUES(?1,?2,?3,?4,?5)",params![entry.id,name,hash,(batch_index*64+offset) as i64,bytes])?;
                    chunk_count += 1;
                }
            }
            indexed += 1;
        }
        tx.execute("INSERT INTO embedding_models(name,identity) VALUES(?1,?2) ON CONFLICT(name) DO UPDATE SET identity=excluded.identity",params![name,identity])?;
        tx.commit()?;
        Ok(
            serde_json::json!({"model":name,"identity":identity,"indexed":indexed,"unchanged":unchanged,"chunks":chunk_count,"chunk_characters":CHUNK_CHARS,"overlap_characters":OVERLAP}),
        )
    }

    pub fn search(
        catalog: &Catalog,
        query: &str,
        name: &str,
        format: Option<Format>,
        limit: usize,
        hybrid: bool,
    ) -> Result<Vec<Entry>> {
        if query.trim().is_empty() {
            bail!("search query must not be empty");
        }
        let stored_identity: Option<String> = catalog
            .db
            .query_row(
                "SELECT identity FROM embedding_models WHERE name=?1",
                [name],
                |r| r.get(0),
            )
            .optional()?;
        let stored_identity = stored_identity.ok_or_else(|| {
            anyhow!("no embeddings for {name}; run ams embed --model {name} first")
        })?;
        let missing:i64=catalog.db.query_row("SELECT count(*) FROM entries e WHERE e.status='ok' AND (?2 IS NULL OR e.format=?2) AND NOT EXISTS(SELECT 1 FROM embeddings v WHERE v.id=e.id AND v.model=?1 AND v.content_hash=e.content_hash)",params![name,format.map(|f|f.to_string())],|r|r.get(0))?;
        if missing > 0 {
            bail!(
                "{missing} sessions have missing or stale embeddings; run ams embed --model {name} after scanning"
            );
        }
        let count:i64=catalog.db.query_row("SELECT count(*) FROM embeddings v JOIN entries e ON e.id=v.id WHERE v.model=?1 AND e.status='ok' AND v.content_hash=e.content_hash AND (?2 IS NULL OR e.format=?2)",params![name,format.map(|f|f.to_string())],|r|r.get(0))?;
        if count as usize > MAX_SEARCH_CHUNKS {
            bail!(
                "semantic brute-force search exceeds {MAX_SEARCH_CHUNKS} chunks; restrict --format"
            );
        }
        let (model, identity) = load_model(name)?;
        if identity != stored_identity {
            bail!(
                "embedding model assets changed; run ams embed --model {name} to rebuild vectors"
            );
        }
        let mut needle = model.encode_single(query);
        normalize(&mut needle)?;
        let mut statement=catalog.db.prepare("SELECT v.id,v.vector FROM embeddings v JOIN entries e ON e.id=v.id WHERE v.model=?1 AND e.status='ok' AND v.content_hash=e.content_hash AND (?2 IS NULL OR e.format=?2)")?;
        let mut rows = statement.query(params![name, format.map(|f| f.to_string())])?;
        let mut scores = BTreeMap::<String, f64>::new();
        while let Some(row) = rows.next()? {
            let id: String = row.get(0)?;
            let raw = row.get_ref(1)?.as_blob()?;
            if raw.len() != needle.len() * 4 {
                bail!("embedding dimensions do not match the current model; run ams embed again");
            }
            let mut score = 0f64;
            for (raw, q) in raw.chunks_exact(4).zip(&needle) {
                let value = f32::from_le_bytes(raw.try_into().unwrap());
                if !value.is_finite() {
                    bail!("invalid stored embedding; rebuild catalog embeddings");
                }
                score += value as f64 * *q as f64;
            }
            scores
                .entry(id)
                .and_modify(|old| *old = old.max(score))
                .or_insert(score);
        }
        let mut semantic: Vec<_> = scores.into_iter().collect();
        semantic.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        let ranked = if hybrid {
            let candidates = limit.saturating_mul(8).max(100);
            let lexical = catalog.find(query, format, candidates)?;
            let mut rrf = BTreeMap::<String, f64>::new();
            for (rank, (id, _)) in semantic.iter().take(candidates).enumerate() {
                *rrf.entry(id.clone()).or_default() += 1.0 / (60.0 + rank as f64 + 1.0);
            }
            for (rank, entry) in lexical
                .into_iter()
                .filter(|entry| entry.status == "ok")
                .enumerate()
            {
                *rrf.entry(entry.id).or_default() += 1.0 / (60.0 + rank as f64 + 1.0);
            }
            let mut ranked: Vec<_> = rrf.into_iter().collect();
            ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            ranked
        } else {
            semantic
        };
        ranked
            .into_iter()
            .take(limit)
            .map(|(id, _)| catalog.resolve(&id))
            .collect()
    }
}

#[cfg(feature = "semantic")]
pub use enabled::{index, search};

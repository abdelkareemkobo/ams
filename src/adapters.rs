//! Read-only native readers and explicit, portable-history writers.
//! OpenCode output is a public 1.17.20 import bundle, not an installed database.
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail, ensure};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::model::{Event, Format, Session};

type Losses = BTreeMap<String, usize>;

#[derive(Debug)]
pub struct Rendered {
    pub id: String,
    pub bytes: Vec<u8>,
    pub losses: Losses,
    pub relative_path: PathBuf,
    pub resume_args: Vec<String>,
}

fn loss(losses: &mut Losses, key: impl AsRef<str>) {
    let key = key.as_ref();
    if let Some(count) = losses.get_mut(key) {
        *count += 1;
    } else {
        losses.insert(key.to_owned(), 1);
    }
}
fn s<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}
fn required<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    let text = s(v, key);
    ensure!(!text.is_empty(), "missing or invalid {key}");
    Ok(text)
}
fn timestamp(v: &str) -> Result<DateTime<chrono::FixedOffset>> {
    DateTime::parse_from_rfc3339(v).context("invalid RFC3339 session timestamp")
}
fn metadata(format: Format, id: &str, cwd: &str, started: &str, title: &str) -> Session {
    Session {
        format,
        native_id: id.into(),
        title: title.into(),
        cwd: cwd.into(),
        started_at: started.into(),
        events: Vec::new(),
        losses: BTreeMap::new(),
    }
}

/// Decode one line at a time; never read an entire JSONL journal into a string.
/// Tree formats retain parsed records because their leaf can select an older branch.
fn jsonl(path: &Path) -> Result<Vec<Value>> {
    let reader =
        BufReader::new(File::open(path).with_context(|| format!("opening {}", path.display()))?);
    let mut records = Vec::new();
    for (index, line) in reader.lines().enumerate() {
        let line = line.with_context(|| format!("reading line {}", index + 1))?;
        if line.trim().is_empty() {
            continue;
        }
        let value: Value = serde_json::from_str(&line)
            .with_context(|| format!("invalid JSON at line {}", index + 1))?;
        ensure!(value.is_object(), "line {} is not a JSON object", index + 1);
        records.push(value);
    }
    ensure!(!records.is_empty(), "empty session");
    Ok(records)
}

pub fn load(path: &Path, format: Option<Format>) -> Result<Session> {
    // Probe only the first nonempty line. Pretty-printed public bundles are JSON,
    // whereas every supported journal has a complete first-line JSON record.
    if let Some(selected) = format {
        return load_selected(path, selected);
    }
    let mut reader = BufReader::new(File::open(path)?);
    let mut first = String::new();
    loop {
        ensure!(reader.read_line(&mut first)? != 0, "empty session");
        if !first.trim().is_empty() {
            break;
        }
        first.clear();
    }
    let probe: Option<Value> = serde_json::from_str(first.trim()).ok();
    let detected = match probe.as_ref().map(|v| s(v, "type")) {
        Some("title") => Some(Format::Omp),
        Some("session") => Some(
            if probe
                .as_ref()
                .is_some_and(|v| v.get("titleSource").is_some() || v.get("title").is_some())
            {
                Format::Omp
            } else {
                Format::Pi
            },
        ),
        Some("session_meta") => Some(Format::Codex),
        Some(
            "user"
            | "assistant"
            | "system"
            | "queue-operation"
            | "file-history-snapshot"
            | "summary"
            | "custom-title"
            | "last-prompt"
            | "progress",
        ) => Some(Format::Claude),
        _ => None,
    };
    let selected = detected.unwrap_or(Format::Opencode);
    load_selected(path, selected)
}

fn load_selected(path: &Path, selected: Format) -> Result<Session> {
    match selected {
        Format::Omp | Format::Pi => read_pi(jsonl(path)?, selected),
        Format::Codex => read_codex(jsonl(path)?),
        Format::Claude => read_claude(jsonl(path)?),
        Format::Opencode => read_opencode(
            serde_json::from_reader(BufReader::new(File::open(path)?)).context(
                "invalid OpenCode public JSON bundle (live databases require native export)",
            )?,
        ),
    }
}

fn content_text(content: &Value, losses: &mut Losses, prefix: &str) -> Result<String> {
    if let Some(text) = content.as_str() {
        return Ok(text.into());
    }
    let blocks = content
        .as_array()
        .ok_or_else(|| anyhow!("{prefix} content is neither text nor blocks"))?;
    let mut text = Vec::new();
    for block in blocks {
        match s(block, "type") {
            "text" | "input_text" | "output_text" => text.push(
                block["text"]
                    .as_str()
                    .ok_or_else(|| anyhow!("invalid tool-result text"))?
                    .to_owned(),
            ),
            kind => loss(
                losses,
                format!(
                    "{prefix}:{}",
                    if kind.is_empty() {
                        "unknown_block"
                    } else {
                        kind
                    }
                ),
            ),
        }
    }
    Ok(text.join("\n"))
}

fn image_uri(role: &str, uri: &str, session: &mut Session) {
    if let Some((mime, data)) = uri
        .strip_prefix("data:")
        .and_then(|v| v.split_once(";base64,"))
    {
        session.events.push(Event::Image {
            role: role.into(),
            mime_type: mime.into(),
            data: data.into(),
        });
    } else {
        loss(&mut session.losses, "image:external_or_unresolved");
    }
}

fn portable_content(content: &Value, role: &str, session: &mut Session, style: &str) -> Result<()> {
    if let Some(text) = content.as_str() {
        if !text.is_empty() {
            session.events.push(Event::Text {
                role: role.into(),
                text: text.into(),
            });
        }
        return Ok(());
    }
    let blocks = content
        .as_array()
        .ok_or_else(|| anyhow!("invalid {style} message content"))?;
    for block in blocks {
        ensure!(block.is_object(), "invalid message content block");
        match s(block, "type") {
            "text" | "input_text" | "output_text" => {
                let text = block
                    .get("text")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("invalid text block"))?;
                if !text.is_empty() {
                    session.events.push(Event::Text {
                        role: role.into(),
                        text: text.into(),
                    });
                }
            }
            "image" if style == "claude" => {
                let source = &block["source"];
                if s(source, "type") == "base64" {
                    session.events.push(Event::Image {
                        role: role.into(),
                        mime_type: required(source, "media_type")?.into(),
                        data: required(source, "data")?.into(),
                    });
                } else {
                    loss(&mut session.losses, "image:external_or_unresolved");
                }
            }
            "image" => {
                let data = required(block, "data")?;
                if data.starts_with("blob:") {
                    loss(&mut session.losses, "image:external_blob");
                } else {
                    session.events.push(Event::Image {
                        role: role.into(),
                        mime_type: required(block, "mimeType")?.into(),
                        data: data.into(),
                    });
                }
            }
            "input_image" | "image_url" => image_uri(role, s(block, "image_url"), session),
            "toolCall" | "tool_use" => {
                ensure!(role == "assistant", "tool call in non-assistant message");
                let arguments = if style == "claude" {
                    &block["input"]
                } else {
                    &block["arguments"]
                };
                ensure!(arguments.is_object(), "tool arguments are not an object");
                session.events.push(Event::ToolCall {
                    id: required(block, "id")?.into(),
                    name: required(block, "name")?.into(),
                    arguments: arguments.clone(),
                });
            }
            "tool_result" if style == "claude" => {
                let text = content_text(&block["content"], &mut session.losses, "tool_result")?;
                session.events.push(Event::ToolResult {
                    id: required(block, "tool_use_id")?.into(),
                    name: String::new(),
                    text,
                    is_error: block["is_error"] == true,
                });
            }
            "thinking" | "reasoning" | "redacted_thinking" => {
                loss(&mut session.losses, "reasoning")
            }
            kind => loss(
                &mut session.losses,
                format!(
                    "{style}:block:{}",
                    if kind.is_empty() { "unknown" } else { kind }
                ),
            ),
        }
    }
    Ok(())
}

fn read_pi(records: Vec<Value>, format: Format) -> Result<Session> {
    let slot = records
        .first()
        .filter(|record| s(record, "type") == "title");
    ensure!(
        !(format == Format::Pi && slot.is_some()),
        "OMP title-slot journal is not Pi"
    );
    let header_index = usize::from(slot.is_some());
    let header = records
        .get(header_index)
        .ok_or_else(|| anyhow!("missing session header"))?;
    ensure!(
        s(header, "type") == "session" && header["version"] == 3,
        "unsupported Pi/OMP schema (requires v3)"
    );
    let started = required(header, "timestamp")?;
    timestamp(started)?;
    let mut session = metadata(
        format,
        required(header, "id")?,
        required(header, "cwd")?,
        started,
        s(header, "title"),
    );
    if let Some(title) = slot.as_ref() {
        ensure!(
            title["v"] == 1 && title["title"].is_string(),
            "unsupported OMP title slot"
        );
        session.title = s(title, "title").into();
    }
    if header.get("parentSession").is_some_and(|v| !v.is_null()) {
        loss(&mut session.losses, "session:parent_lineage");
    }
    let entries = &records[header_index + 1..];
    let mut ids = HashMap::new();
    for (i, entry) in entries.iter().enumerate() {
        let id = required(entry, "id")?;
        ensure!(!ids.contains_key(id), "duplicate entry id {id}");
        if let Some(parent) = entry.get("parentId").filter(|v| !v.is_null()) {
            let parent = parent.as_str().ok_or_else(|| anyhow!("invalid parentId"))?;
            ensure!(
                ids.contains_key(parent),
                "entry {id} references missing or future parent {parent}"
            );
        }
        timestamp(required(entry, "timestamp")?)?;
        ids.insert(id, i);
        match s(entry, "type") {
            "session_info" if entry["name"].is_string() => {
                session.title = s(entry, "name").into();
            }
            "title_change" if entry["title"].is_string() => {
                session.title = s(entry, "title").into();
            }
            _ => {}
        }
    }
    let mut active = Vec::new();
    let mut cursor = entries.len().checked_sub(1);
    while let Some(index) = cursor {
        active.push(index);
        cursor = entries[index]
            .get("parentId")
            .and_then(Value::as_str)
            .map(|p| ids[p]);
    }
    active.reverse();
    for _ in 0..entries.len().saturating_sub(active.len()) {
        loss(&mut session.losses, "history:inactive_branch");
    }
    let reset = active
        .iter()
        .rposition(|&i| s(&entries[i], "type") == "reset_boundary");
    for (position, &index) in active.iter().enumerate() {
        let entry = &entries[index];
        if reset.is_some_and(|r| position < r) {
            loss(&mut session.losses, "history:pre_reset");
            continue;
        }
        loss(&mut session.losses, "event:timestamp");
        match s(entry, "type") {
            "message" => {
                let message = &entry["message"];
                let role = required(message, "role")?;
                match role {
                    "user" | "assistant" => {
                        portable_content(&message["content"], role, &mut session, "pi")?
                    }
                    "toolResult" => {
                        let text =
                            content_text(&message["content"], &mut session.losses, "tool_result")?;
                        session.events.push(Event::ToolResult {
                            id: required(message, "toolCallId")?.into(),
                            name: s(message, "toolName").into(),
                            text,
                            is_error: message["isError"] == true,
                        });
                        if message.get("details").is_some() {
                            loss(&mut session.losses, "tool_result:details");
                        }
                    }
                    _ => loss(&mut session.losses, format!("pi:message_role:{role}")),
                }
                message_losses(message, &mut session.losses);
            }
            "compaction" => {
                let summary = required(entry, "summary")?;
                let kept = required(entry, "firstKeptEntryId")?;
                ensure!(
                    ids.contains_key(kept),
                    "compaction references missing firstKeptEntryId"
                );
                ensure!(
                    active.binary_search(&ids[kept]).is_ok(),
                    "compaction anchor belongs to inactive branch"
                );
                ensure!(
                    entry["tokensBefore"].as_u64().is_some(),
                    "invalid compaction token count"
                );
                session.events.push(Event::Summary {
                    text: summary.into(),
                });
                loss(&mut session.losses, "summary:boundary_metadata");
            }
            "branch_summary" => {
                session.events.push(Event::Summary {
                    text: required(entry, "summary")?.into(),
                });
                loss(&mut session.losses, "summary:branch_metadata");
            }
            "session_info" | "title_change" => {}
            "reset" | "clear" => {
                bail!("unsupported reset schema; refusing to replay pre-reset history")
            }
            kind => loss(&mut session.losses, format!("runtime:{kind}")),
        }
    }
    finish(session)
}

fn message_losses(message: &Value, losses: &mut Losses) {
    if let Some(fields) = message.as_object() {
        for field in fields.keys() {
            if !matches!(
                field.as_str(),
                "role" | "content" | "toolCallId" | "toolName" | "isError" | "details"
            ) {
                loss(losses, format!("message:{field}"));
            }
        }
    }
}
fn finish(mut session: Session) -> Result<Session> {
    // Result names are not present in Claude/Codex. Recover only from actual calls.
    let names: HashMap<String, String> = session
        .events
        .iter()
        .filter_map(|e| match e {
            Event::ToolCall { id, name, .. } => Some((id.clone(), name.clone())),
            _ => None,
        })
        .collect();
    for event in &mut session.events {
        if let Event::ToolResult { id, name, .. } = event
            && name.is_empty()
            && let Some(actual) = names.get(id)
        {
            *name = actual.clone();
        }
    }
    if session.title.is_empty() {
        session.title = session
            .events
            .iter()
            .find_map(|e| match e {
                Event::Text { role, text } if role == "user" => Some(
                    text.lines()
                        .next()
                        .unwrap_or("")
                        .chars()
                        .take(100)
                        .collect(),
                ),
                _ => None,
            })
            .unwrap_or_else(|| "Untitled session".into());
    }
    Ok(session)
}

fn read_codex(records: Vec<Value>) -> Result<Session> {
    let header = records
        .first()
        .ok_or_else(|| anyhow!("empty Codex session"))?;
    ensure!(
        s(header, "type") == "session_meta",
        "Codex journal must begin with session_meta"
    );
    let meta = &header["payload"];
    let mode = match s(meta, "history_mode") {
        "" => "legacy",
        v => v,
    };
    ensure!(
        mode == "legacy" || mode == "paginated",
        "unsupported Codex history mode {mode}"
    );
    ensure!(
        meta.get("history_base").is_none_or(Value::is_null)
            && meta
                .get("subagent_history_start_ordinal")
                .is_none_or(Value::is_null),
        "Codex inherited/subagent history requires unsupported lineage resolution"
    );
    let started = if s(meta, "timestamp").is_empty() {
        required(header, "timestamp")?
    } else {
        s(meta, "timestamp")
    };
    timestamp(started)?;
    let id = if s(meta, "id").is_empty() {
        required(meta, "session_id")?
    } else {
        s(meta, "id")
    };
    let mut session = metadata(Format::Codex, id, required(meta, "cwd")?, started, "");
    let has_responses = records
        .iter()
        .any(|v| s(v, "type") == "response_item" && s(&v["payload"], "type") == "message");
    for (index, record) in records.iter().enumerate() {
        if mode == "paginated" {
            ensure!(
                record["ordinal"].as_u64() == Some(index as u64),
                "Codex paginated ordinals must be contiguous from zero"
            );
        }
        let payload = &record["payload"];
        ensure!(payload.is_object(), "invalid Codex record payload");
        match s(record, "type") {
            "session_meta" => {
                ensure!(index == 0, "multiple Codex session headers unsupported");
                for field in [
                    "cli_version",
                    "originator",
                    "model_provider",
                    "source",
                    "base_instructions",
                    "git",
                ] {
                    if meta.get(field).is_some() {
                        loss(&mut session.losses, format!("session:{field}"));
                    }
                }
            }
            "response_item" => {
                if mode == "paginated" && matches!(s(payload, "type"), "message" | "agent_message")
                {
                    loss(&mut session.losses, "codex:paginated_provider_message");
                } else {
                    codex_item(payload, &mut session)?;
                }
            }
            "event_msg" => match s(payload, "type") {
                "thread_name_updated" => {
                    session.title = if s(payload, "thread_name").is_empty() {
                        s(payload, "name")
                    } else {
                        s(payload, "thread_name")
                    }
                    .into()
                }
                "user_message" | "agent_message" if mode == "legacy" && !has_responses => {
                    session.events.push(Event::Text {
                        role: if s(payload, "type") == "user_message" {
                            "user"
                        } else {
                            "assistant"
                        }
                        .into(),
                        text: required(payload, "message")?.into(),
                    });
                }
                "user_message" | "agent_message" => {}
                "item_completed" if mode == "paginated" => codex_completed(payload, &mut session)?,
                "thread_rolled_back" | "rollback" | "reset" | "reset_boundary" => bail!(
                    "Codex rollback/reset journals are unsupported; refusing to resurrect inactive history"
                ),
                kind => loss(&mut session.losses, format!("runtime:codex:{kind}")),
            },
            "compacted" => {
                if payload
                    .get("replacement_history")
                    .is_some_and(|v| !v.is_null())
                {
                    bail!(
                        "Codex replacement_history compaction unsupported; refusing to resurrect superseded history"
                    );
                }
                if !s(payload, "message").is_empty() {
                    session.events.push(Event::Summary {
                        text: s(payload, "message").into(),
                    });
                } else {
                    loss(&mut session.losses, "summary:empty_boundary");
                }
            }
            kind => loss(&mut session.losses, format!("runtime:codex:{kind}")),
        }
        if index != 0 && record.get("timestamp").is_some() {
            loss(&mut session.losses, "event:timestamp");
        }
    }
    finish(session)
}
fn codex_item(item: &Value, session: &mut Session) -> Result<()> {
    match s(item, "type") {
        "message" => {
            let role = required(item, "role")?;
            ensure!(
                matches!(role, "user" | "assistant" | "system" | "developer"),
                "invalid Codex message role"
            );
            portable_content(&item["content"], role, session, "codex")?;
        }
        "function_call" => {
            let raw = required(item, "arguments")?;
            let arguments: Value =
                serde_json::from_str(raw).context("invalid Codex tool arguments JSON")?;
            session.events.push(Event::ToolCall {
                id: required(item, "call_id")?.into(),
                name: required(item, "name")?.into(),
                arguments,
            });
            if item.get("namespace").is_some() {
                loss(&mut session.losses, "tool_call:namespace");
            }
        }
        "function_call_output" => {
            let text = content_text(&item["output"], &mut session.losses, "tool_result")?;
            session.events.push(Event::ToolResult {
                id: required(item, "call_id")?.into(),
                name: String::new(),
                text,
                is_error: false,
            });
        }
        "reasoning" => loss(&mut session.losses, "reasoning"),
        kind => loss(&mut session.losses, format!("codex:item:{kind}")),
    }
    Ok(())
}
fn codex_completed(payload: &Value, session: &mut Session) -> Result<()> {
    let item = &payload["item"];
    let role = match s(item, "type") {
        "UserMessage" => "user",
        "AgentMessage" => "assistant",
        kind => {
            loss(&mut session.losses, format!("codex:completed:{kind}"));
            return Ok(());
        }
    };
    for block in item["content"]
        .as_array()
        .ok_or_else(|| anyhow!("invalid canonical Codex content"))?
    {
        match s(block, "type") {
            "Text" | "text" => {
                let text = block["text"]
                    .as_str()
                    .ok_or_else(|| anyhow!("invalid canonical Codex text"))?;
                if !text.is_empty() {
                    session.events.push(Event::Text {
                        role: role.into(),
                        text: text.into(),
                    });
                }
                if block.get("text_elements").is_some() {
                    loss(&mut session.losses, "codex:text_elements");
                }
            }
            "image" => image_uri(role, required(block, "image_url")?, session),
            kind => loss(&mut session.losses, format!("codex:canonical_block:{kind}")),
        }
    }
    Ok(())
}

fn read_claude(records: Vec<Value>) -> Result<Session> {
    let mut by_id = HashMap::new();
    let mut candidates = Vec::new();
    let mut title = String::new();
    let mut ai_title = String::new();
    let mut leaf = None;
    for (index, record) in records.iter().enumerate() {
        if !s(record, "uuid").is_empty() {
            ensure!(
                by_id.insert(s(record, "uuid"), index).is_none(),
                "duplicate Claude UUID"
            );
        }
        match s(record, "type") {
            "user" | "assistant" if record["isSidechain"] != true && record["isMeta"] != true => {
                candidates.push(index)
            }
            "last-prompt" if !s(record, "leafUuid").is_empty() => {
                leaf = Some(s(record, "leafUuid"))
            }
            "custom-title" => title = s(record, "customTitle").into(),
            "ai-title" => ai_title = s(record, "aiTitle").into(),
            "summary" => ai_title = s(record, "summary").into(),
            _ => {}
        }
    }
    let candidate = *candidates.last().ok_or_else(|| {
        anyhow!("Claude journal has no main conversation (sidechain-only journals unsupported)")
    })?;
    let leaf = leaf.unwrap_or(s(&records[candidate], "uuid"));
    ensure!(
        !leaf.is_empty(),
        "Claude conversation records require UUID ancestry"
    );
    let mut seen = HashSet::new();
    let mut active = Vec::new();
    let mut cursor = leaf;
    loop {
        ensure!(
            seen.insert(cursor),
            "Claude ancestry cycle (preserved-segment cycles unsupported)"
        );
        let &index = by_id
            .get(cursor)
            .ok_or_else(|| anyhow!("Claude ancestry references missing UUID {cursor}"))?;
        let record = &records[index];
        ensure!(
            record["isSidechain"] != true,
            "Claude selected branch enters sidechain"
        );
        active.push(index);
        // Compaction retains the historical timeline through logicalParentUuid;
        // unlike reset it does not make the earlier conversation an inactive branch.
        let parent = if record.get("parentUuid").is_none_or(Value::is_null)
            && s(record, "type") == "system"
            && s(record, "subtype") == "compact_boundary"
        {
            record.get("logicalParentUuid").filter(|v| !v.is_null())
        } else {
            record.get("parentUuid").filter(|v| !v.is_null())
        };
        match parent {
            Some(value) => {
                let parent = value
                    .as_str()
                    .ok_or_else(|| anyhow!("invalid Claude parent UUID"))?;
                if seen.contains(parent)
                    && s(record, "type") == "system"
                    && s(record, "subtype") == "compact_boundary"
                    && claude_preserved_boundary(record, parent, &seen, &records, &by_id)
                {
                    break;
                }
                cursor = parent;
            }
            None => break,
        }
    }
    active.reverse();
    let first = active
        .iter()
        .map(|&i| &records[i])
        .find(|r| !s(r, "sessionId").is_empty())
        .ok_or_else(|| anyhow!("Claude sessionId missing"))?;
    let started_record = records
        .iter()
        .find(|r| !s(r, "timestamp").is_empty())
        .ok_or_else(|| anyhow!("Claude timestamp missing"))?;
    timestamp(required(started_record, "timestamp")?)?;
    let cwd = records.iter().find_map(|r| r["cwd"].as_str()).unwrap_or("");
    let mut session = metadata(
        Format::Claude,
        required(first, "sessionId")?,
        cwd,
        s(started_record, "timestamp"),
        if title.is_empty() { &ai_title } else { &title },
    );
    let selected: HashSet<usize> = active.iter().copied().collect();
    for &index in &active {
        let record = &records[index];
        if !s(record, "sessionId").is_empty() {
            ensure!(
                s(record, "sessionId") == session.native_id,
                "Claude active graph has mixed session IDs"
            );
        }
        match s(record, "type") {
            "user" | "assistant" if record["isMeta"] != true => {
                let message = &record["message"];
                if record["isCompactSummary"] == true {
                    let text = content_text(&message["content"], &mut session.losses, "summary")?;
                    session.events.push(Event::Summary { text });
                    loss(&mut session.losses, "summary:boundary_metadata");
                } else {
                    let role = required(message, "role")?;
                    ensure!(
                        matches!(role, "user" | "assistant"),
                        "unsupported Claude privileged message role"
                    );
                    portable_content(&message["content"], role, &mut session, "claude")?;
                }
                message_losses(message, &mut session.losses);
                for field in ["toolUseResult", "sourceToolAssistantUUID"] {
                    if record.get(field).is_some() {
                        loss(&mut session.losses, format!("claude:{field}"));
                    }
                }
            }
            kind => loss(&mut session.losses, format!("runtime:claude:{kind}")),
        }
        if record.get("timestamp").is_some() {
            loss(&mut session.losses, "event:timestamp");
        }
        for field in [
            "version",
            "userType",
            "permissionMode",
            "gitBranch",
            "agentId",
            "teamName",
            "thinkingMetadata",
            "requestId",
        ] {
            if record.get(field).is_some() {
                loss(&mut session.losses, format!("claude:{field}"));
            }
        }
    }
    for (i, record) in records.iter().enumerate() {
        if !selected.contains(&i) && matches!(s(record, "type"), "user" | "assistant") {
            loss(&mut session.losses, "history:inactive_or_pre_compaction");
        } else if !selected.contains(&i)
            && !matches!(
                s(record, "type"),
                "custom-title" | "ai-title" | "summary" | "last-prompt"
            )
        {
            loss(
                &mut session.losses,
                format!("runtime:claude:{}", s(record, "type")),
            );
        }
    }
    finish(session)
}

fn claude_preserved_boundary(
    boundary: &Value,
    logical_parent: &str,
    seen: &HashSet<&str>,
    records: &[Value],
    by_id: &HashMap<&str, usize>,
) -> bool {
    let metadata = &boundary["compactMetadata"];
    let segment = &metadata["preservedSegment"];
    let messages = &metadata["preservedMessages"];
    let anchor = s(segment, "anchorUuid");
    let head = s(segment, "headUuid");
    let tail = s(segment, "tailUuid");
    if anchor.is_empty() || head.is_empty() || tail != logical_parent {
        return false;
    }
    let Some(&anchor_index) = by_id.get(anchor) else {
        return false;
    };
    let anchor_record = &records[anchor_index];
    if anchor_record["isCompactSummary"] != true
        || s(anchor_record, "parentUuid") != s(boundary, "uuid")
    {
        return false;
    }
    let Some(declared) = messages
        .get("allUuids")
        .or_else(|| messages.get("uuids"))
        .and_then(Value::as_array)
    else {
        return false;
    };
    if !declared.iter().any(|v| v.as_str() == Some(head))
        || !declared.iter().any(|v| v.as_str() == Some(tail))
        || !declared
            .iter()
            .all(|v| v.as_str().is_some_and(|id| seen.contains(id)))
    {
        return false;
    }
    let mut cursor = tail;
    let mut visited = HashSet::new();
    while cursor != anchor {
        if !seen.contains(cursor) || !visited.insert(cursor) {
            return false;
        }
        let Some(&index) = by_id.get(cursor) else {
            return false;
        };
        cursor = s(&records[index], "parentUuid");
    }
    visited.contains(head)
}

fn read_opencode(bundle: Value) -> Result<Session> {
    let info = &bundle["info"];
    let id = required(info, "id")?;
    ensure!(id.starts_with("ses_"), "invalid OpenCode session ID");
    let created = info["time"]["created"]
        .as_i64()
        .ok_or_else(|| anyhow!("invalid OpenCode creation time"))?;
    let started = DateTime::<Utc>::from_timestamp_millis(created)
        .ok_or_else(|| anyhow!("invalid OpenCode timestamp"))?
        .to_rfc3339();
    if info.get("location").is_some() {
        return read_opencode_v2(&bundle, &started);
    }
    let version = required(info, "version")?;
    ensure!(
        version.starts_with("1."),
        "unsupported OpenCode legacy bundle version {version}"
    );
    let mut session = metadata(
        Format::Opencode,
        id,
        required(info, "directory")?,
        &started,
        required(info, "title")?,
    );
    let messages = bundle["messages"]
        .as_array()
        .ok_or_else(|| anyhow!("OpenCode bundle missing messages"))?;
    let revert_message = info["revert"]["messageID"].as_str();
    let revert_part = info["revert"]["partID"].as_str();
    if info.get("revert").is_some_and(|v| !v.is_null()) {
        required(&info["revert"], "messageID")
            .context("OpenCode revert has unsupported/missing boundary")?;
    }
    let mut message_ids = HashSet::new();
    let mut part_ids = HashSet::new();
    let mut reverted = false;
    let mut revert_found = revert_message.is_none();
    for message in messages {
        let mi = &message["info"];
        let mid = required(mi, "id")?;
        ensure!(
            mid.starts_with("msg_") && message_ids.insert(mid),
            "invalid or duplicate OpenCode message ID"
        );
        ensure!(
            s(mi, "sessionID") == id,
            "OpenCode message session mismatch"
        );
        let role = required(mi, "role")?;
        ensure!(
            matches!(role, "user" | "assistant"),
            "invalid OpenCode role"
        );
        if revert_message == Some(mid) {
            revert_found = true;
            if revert_part.is_none() {
                reverted = true;
            }
        }
        if reverted {
            loss(&mut session.losses, "history:reverted");
            continue;
        }
        let parts = message["parts"]
            .as_array()
            .ok_or_else(|| anyhow!("OpenCode message missing parts"))?;
        let summary = mi["summary"] == true;
        if role == "assistant" {
            let parent = required(mi, "parentID")?;
            ensure!(
                parent != mid && message_ids.contains(parent),
                "OpenCode assistant references missing/future/self parent"
            );
        }
        for part in parts {
            let pid = required(part, "id")?;
            ensure!(
                pid.starts_with("prt_") && part_ids.insert(pid),
                "invalid or duplicate OpenCode part ID"
            );
            ensure!(
                s(part, "sessionID") == id && s(part, "messageID") == mid,
                "OpenCode part identity mismatch"
            );
            if revert_message == Some(mid) && revert_part == Some(pid) {
                reverted = true;
            }
            if reverted {
                loss(&mut session.losses, "history:reverted");
                continue;
            }
            match s(part, "type") {
                "text" if summary => session.events.push(Event::Summary {
                    text: required(part, "text")?.into(),
                }),
                "text" if part["ignored"] != true => session.events.push(Event::Text {
                    role: role.into(),
                    text: required(part, "text")?.into(),
                }),
                "file" if s(part, "mime").starts_with("image/") => {
                    image_uri(role, required(part, "url")?, &mut session)
                }
                "tool" => opencode_tool(part, false, &mut session)?,
                "reasoning" => loss(&mut session.losses, "reasoning"),
                "compaction" => {
                    if !messages
                        .iter()
                        .any(|m| m["info"]["summary"] == true && s(&m["info"], "parentID") == mid)
                    {
                        loss(&mut session.losses, "summary:empty_boundary");
                    }
                }
                kind => loss(&mut session.losses, format!("opencode:part:{kind}")),
            }
            for field in [
                "metadata",
                "synthetic",
                "ignored",
                "time",
                "source",
                "filename",
            ] {
                if part.get(field).is_some() {
                    loss(
                        &mut session.losses,
                        format!("opencode:part_metadata:{field}"),
                    );
                }
            }
        }
        for field in [
            "tokens",
            "cost",
            "model",
            "modelID",
            "providerID",
            "agent",
            "mode",
            "path",
            "time",
            "finish",
            "error",
            "system",
            "tools",
            "format",
        ] {
            if mi.get(field).is_some() {
                loss(&mut session.losses, format!("message:{field}"));
            }
        }
    }
    ensure!(revert_found, "OpenCode revert references missing message");
    if revert_part.is_some() {
        ensure!(reverted, "OpenCode revert references missing part");
    }
    for field in [
        "parentID",
        "summary",
        "revert",
        "share",
        "permission",
        "metadata",
    ] {
        if info.get(field).is_some_and(|v| !v.is_null()) {
            loss(&mut session.losses, format!("session:{field}"));
        }
    }
    finish(session)
}
fn opencode_tool(part: &Value, v2: bool, session: &mut Session) -> Result<()> {
    let state = &part["state"];
    let id = required(part, if v2 { "id" } else { "callID" })?;
    let name = required(part, if v2 { "name" } else { "tool" })?;
    ensure!(state["input"].is_object(), "invalid OpenCode tool input");
    session.events.push(Event::ToolCall {
        id: id.into(),
        name: name.into(),
        arguments: state["input"].clone(),
    });
    let text = match s(state, "status") {
        "completed" if v2 => Some(content_text(
            &state["content"],
            &mut session.losses,
            "tool_result",
        )?),
        "completed" => Some(
            state["output"]
                .as_str()
                .ok_or_else(|| anyhow!("invalid OpenCode tool output"))?
                .into(),
        ),
        "error" if v2 => Some(required(&state["error"], "message")?.into()),
        "error" => Some(
            state["error"]
                .as_str()
                .ok_or_else(|| anyhow!("invalid OpenCode tool error"))?
                .into(),
        ),
        "pending" | "running" => {
            loss(&mut session.losses, "tool_call:incomplete_state");
            None
        }
        status => bail!("unsupported OpenCode tool state {status}"),
    };
    if let Some(text) = text {
        session.events.push(Event::ToolResult {
            id: id.into(),
            name: name.into(),
            text,
            is_error: s(state, "status") == "error",
        });
    }
    for field in ["metadata", "time", "title", "attachments", "raw"] {
        if state.get(field).is_some() {
            loss(&mut session.losses, format!("tool_result:{field}"));
        }
    }
    Ok(())
}
fn read_opencode_v2(bundle: &Value, started: &str) -> Result<Session> {
    let info = &bundle["info"];
    let mut session = metadata(
        Format::Opencode,
        required(info, "id")?,
        required(&info["location"], "directory")?,
        started,
        required(info, "title")?,
    );
    // Unknown reset/revert semantics must not be flattened into replayable text.
    ensure!(
        info.get("revert").is_none_or(Value::is_null),
        "OpenCode v2 reverted bundle unsupported"
    );
    let mut ids = HashSet::new();
    for message in bundle["messages"]
        .as_array()
        .ok_or_else(|| anyhow!("OpenCode v2 messages missing"))?
    {
        ensure!(
            ids.insert(required(message, "id")?),
            "duplicate OpenCode v2 message ID"
        );
        match s(message, "type") {
            "user" => {
                if !s(message, "text").is_empty() {
                    session.events.push(Event::Text {
                        role: "user".into(),
                        text: s(message, "text").into(),
                    });
                }
                if let Some(files) = message["files"].as_array() {
                    for file in files {
                        if s(file, "mime").starts_with("image/")
                            && s(&file["source"], "type") == "inline"
                        {
                            session.events.push(Event::Image {
                                role: "user".into(),
                                mime_type: required(file, "mime")?.into(),
                                data: required(file, "data")?.into(),
                            });
                        } else {
                            loss(&mut session.losses, "opencode:file");
                        }
                    }
                }
            }
            "assistant" => {
                for block in message["content"]
                    .as_array()
                    .ok_or_else(|| anyhow!("OpenCode v2 content missing"))?
                {
                    match s(block, "type") {
                        "text" => session.events.push(Event::Text {
                            role: "assistant".into(),
                            text: required(block, "text")?.into(),
                        }),
                        "reasoning" => loss(&mut session.losses, "reasoning"),
                        "tool" => opencode_tool(block, true, &mut session)?,
                        kind => loss(&mut session.losses, format!("opencode:block:{kind}")),
                    }
                }
            }
            "compaction" if s(message, "status") == "completed" => {
                session.events.push(Event::Summary {
                    text: required(message, "summary")?.into(),
                })
            }
            kind => bail!("unsupported OpenCode v2 message {kind}; refusing history flattening"),
        }
        for field in [
            "time", "tokens", "cost", "model", "agent", "finish", "recent", "reason",
        ] {
            if message.get(field).is_some() {
                loss(&mut session.losses, format!("message:{field}"));
            }
        }
    }
    for field in ["cost", "tokens", "model", "agent", "parentID", "metadata"] {
        if info.get(field).is_some() {
            loss(&mut session.losses, format!("session:{field}"));
        }
    }
    finish(session)
}

fn encode_jsonl(records: &[Value]) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    for value in records {
        serde_json::to_writer(&mut bytes, value)?;
        bytes.push(b'\n');
    }
    Ok(bytes)
}
fn native_bucket(cwd: &str, omp: bool) -> String {
    let encode = |text: &str| text.replace(['/', '\\', ':'], "-");
    if omp {
        if let Some(home) = std::env::var_os("HOME")
            && let Ok(relative) = Path::new(cwd).strip_prefix(Path::new(&home))
        {
            return format!("-{}", encode(&relative.to_string_lossy()));
        }
        if let Ok(relative) = Path::new(cwd).strip_prefix(std::env::temp_dir()) {
            let relative = encode(&relative.to_string_lossy());
            return if relative.is_empty() {
                "-tmp".into()
            } else {
                format!("-tmp-{relative}")
            };
        }
    }
    format!("--{}--", encode(cwd.trim_start_matches(['/', '\\'])))
}
fn usage() -> Value {
    json!({"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}})
}
fn append_pi(
    records: &mut Vec<Value>,
    parent: &mut Option<String>,
    kind: &str,
    timestamp: &str,
    fields: Value,
) {
    let id = Uuid::new_v4().simple().to_string();
    let mut record = json!({"type":kind,"id":id,"parentId":parent,"timestamp":timestamp});
    for (k, v) in fields.as_object().expect("internal entry object") {
        record[k] = v.clone();
    }
    if kind == "compaction" {
        record["firstKeptEntryId"] = json!(id);
    }
    records.push(record);
    *parent = Some(id);
}

pub fn render(session: &Session, target: Format, cwd: &str) -> Result<Rendered> {
    ensure!(
        target != Format::Claude,
        "Claude is searchable/readable; native Claude writing is unsupported"
    );
    ensure!(
        Path::new(cwd).is_absolute() && !cwd.contains('\0'),
        "target cwd must be an absolute path without NUL"
    );
    ensure!(
        !session.events.is_empty(),
        "cannot render empty portable history"
    );
    for event in &session.events {
        match event {
            Event::Text { text, .. } | Event::Summary { text } => {
                ensure!(!text.is_empty(), "cannot render empty text/summary event")
            }
            Event::Image {
                mime_type, data, ..
            } => ensure!(
                mime_type.starts_with("image/") && !data.is_empty(),
                "cannot render invalid image event"
            ),
            _ => {}
        }
    }
    let date = timestamp(&session.started_at)?.with_timezone(&Utc);
    let started = date.to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let id = if target == Format::Opencode {
        format!("ses_{}", Uuid::new_v4().simple())
    } else {
        Uuid::new_v4().to_string()
    };
    let mut losses = session.losses.clone();
    validate_tool_timeline(&session.events)?;
    let (bytes, relative_path, resume_args) = match target {
        Format::Omp | Format::Pi => {
            let mut records =
                vec![json!({"type":"session","version":3,"id":id,"timestamp":started,"cwd":cwd})];
            let mut parent = None;
            for event in &session.events {
                let message = match event {
                    Event::Text { role, text } => {
                        ensure!(
                            matches!(role.as_str(), "user" | "assistant"),
                            "Pi/OMP cannot safely preserve privileged text role {role}"
                        );
                        if role == "user" {
                            json!({"role":role,"content":text,"timestamp":date.timestamp_millis()})
                        } else {
                            json!({"role":role,"content":[{"type":"text","text":text}],"api":"anthropic-messages","provider":"anthropic","model":"unknown","usage":usage(),"stopReason":"stop","timestamp":date.timestamp_millis()})
                        }
                    }
                    Event::ToolCall {
                        id,
                        name,
                        arguments,
                    } => {
                        ensure!(
                            arguments.is_object(),
                            "Pi/OMP require object tool arguments"
                        );
                        json!({"role":"assistant","content":[{"type":"toolCall","id":id,"name":name,"arguments":arguments}],"api":"anthropic-messages","provider":"anthropic","model":"unknown","usage":usage(),"stopReason":"toolUse","timestamp":date.timestamp_millis()})
                    }
                    Event::ToolResult {
                        id,
                        name,
                        text,
                        is_error,
                    } => {
                        json!({"role":"toolResult","toolCallId":id,"toolName":name,"content":[{"type":"text","text":text}],"isError":is_error,"timestamp":date.timestamp_millis()})
                    }
                    Event::Image {
                        role,
                        mime_type,
                        data,
                    } => {
                        ensure!(role == "user", "Pi/OMP cannot preserve non-user images");
                        json!({"role":"user","content":[{"type":"image","mimeType":mime_type,"data":data}],"timestamp":date.timestamp_millis()})
                    }
                    Event::Summary { text } => {
                        append_pi(
                            &mut records,
                            &mut parent,
                            "compaction",
                            &started,
                            json!({"summary":text,"tokensBefore":0}),
                        );
                        continue;
                    }
                };
                append_pi(
                    &mut records,
                    &mut parent,
                    "message",
                    &started,
                    json!({"message":message}),
                );
            }
            let bytes = if target == Format::Omp {
                // A complete slot is 256 bytes, so longer titles cannot fit.
                // Bound work before escaping instead of trimming an unbounded title.
                let mut title: String = session.title.chars().take(256).collect();
                let slot = loop {
                    let slot = json!({"type":"title","v":1,"title":title,"source":"user","updatedAt":started,"pad":""});
                    let mut bytes = encode_jsonl(std::slice::from_ref(&slot))?;
                    if bytes.len() <= 256 {
                        let mut slot = slot;
                        slot["pad"] = json!(" ".repeat(256 - bytes.len()));
                        bytes = encode_jsonl(&[slot])?;
                        ensure!(bytes.len() == 256, "invalid fixed-width OMP title");
                        break bytes;
                    }
                    ensure!(
                        !title.is_empty(),
                        "OMP title metadata cannot fit title slot"
                    );
                    title.pop();
                };
                if title != session.title {
                    loss(&mut losses, "session:title_truncated");
                }
                records[0]["title"] = json!(title);
                records[0]["titleSource"] = json!("user");
                let mut bytes = slot;
                bytes.extend(encode_jsonl(&records)?);
                bytes
            } else {
                append_pi(
                    &mut records,
                    &mut parent,
                    "session_info",
                    &started,
                    json!({"name":session.title}),
                );
                encode_jsonl(&records)?
            };
            let path = PathBuf::from("sessions")
                .join(native_bucket(cwd, target == Format::Omp))
                .join(format!(
                    "{}_{}.jsonl",
                    date.format("%Y-%m-%dT%H-%M-%S%.3fZ"),
                    id
                ));
            let args = vec![
                if target == Format::Omp {
                    "--resume"
                } else {
                    "--session"
                }
                .into(),
                path.to_string_lossy().into_owned(),
            ];
            (bytes, path, args)
        }
        Format::Codex => {
            let mut records = vec![
                json!({"timestamp":started,"type":"session_meta","payload":{"id":id,"session_id":id,"timestamp":started,"cwd":cwd,"originator":"ams","cli_version":"0.144.4","source":"cli","model_provider":"openai","history_mode":"legacy"}}),
            ];
            for event in &session.events {
                let (kind, payload) = match event {
                    Event::Text { role, text } => {
                        ensure!(
                            matches!(role.as_str(), "user" | "assistant"),
                            "Codex writer cannot safely preserve privileged role {role}"
                        );
                        let content_type = if role == "user" {
                            "input_text"
                        } else {
                            "output_text"
                        };
                        records.push(json!({"timestamp":started,"type":"event_msg","payload":{"type":if role == "user" { "user_message" } else { "agent_message" },"message":text}}));
                        (
                            "response_item",
                            json!({"type":"message","role":role,"content":[{"type":content_type,"text":text}]}),
                        )
                    }
                    Event::ToolCall {
                        id,
                        name,
                        arguments,
                    } => (
                        "response_item",
                        json!({"type":"function_call","call_id":id,"name":name,"arguments":serde_json::to_string(arguments)?}),
                    ),
                    Event::ToolResult {
                        id, text, is_error, ..
                    } => {
                        if *is_error {
                            loss(&mut losses, "tool_result:is_error");
                        }
                        (
                            "response_item",
                            json!({"type":"function_call_output","call_id":id,"output":text}),
                        )
                    }
                    Event::Image {
                        role,
                        mime_type,
                        data,
                    } => {
                        ensure!(
                            role == "user",
                            "Codex writer cannot preserve non-user images"
                        );
                        (
                            "response_item",
                            json!({"type":"message","role":"user","content":[{"type":"input_image","image_url":format!("data:{mime_type};base64,{data}")}]}),
                        )
                    }
                    Event::Summary { text } => ("compacted", json!({"message":text})),
                };
                records.push(json!({"timestamp":started,"type":kind,"payload":payload}));
            }
            if !session.title.is_empty() {
                loss(&mut losses, "session:title");
            }
            let path = PathBuf::from("sessions")
                .join(date.format("%Y/%m/%d").to_string())
                .join(format!(
                    "rollout-{}-{id}.jsonl",
                    date.format("%Y-%m-%dT%H-%M-%S")
                ));
            (
                encode_jsonl(&records)?,
                path,
                vec!["resume".into(), id.clone()],
            )
        }
        Format::Opencode => {
            let bytes = render_opencode(session, &id, cwd, date.timestamp_millis(), &mut losses)?;
            let path = PathBuf::from("exports").join(format!("{id}.json"));
            (bytes, path, vec!["--session".into(), id.clone()])
        }
        Format::Claude => unreachable!(),
    };
    Ok(Rendered {
        id,
        bytes,
        losses,
        relative_path,
        resume_args,
    })
}

fn validate_tool_timeline(events: &[Event]) -> Result<()> {
    let mut calls = HashMap::new();
    let mut results = HashSet::new();
    for event in events {
        match event {
            Event::ToolCall { id, name, .. } => {
                ensure!(
                    !id.is_empty() && !name.is_empty(),
                    "tool call missing ID/name"
                );
                ensure!(
                    calls.insert(id.as_str(), name.as_str()).is_none(),
                    "duplicate tool call ID {id}"
                );
            }
            Event::ToolResult { id, name, .. } => {
                let call_name = calls.get(id.as_str()).ok_or_else(|| {
                    anyhow!("orphan tool result {id}; refusing fabricated tool call")
                })?;
                ensure!(
                    name.is_empty() || name.as_str() == *call_name,
                    "tool result name does not match call {id}"
                );
                ensure!(results.insert(id.as_str()), "duplicate tool result {id}");
            }
            _ => {}
        }
    }
    Ok(())
}

fn render_opencode(
    session: &Session,
    id: &str,
    cwd: &str,
    ms: i64,
    _losses: &mut Losses,
) -> Result<Vec<u8>> {
    let mut messages = Vec::<Value>::new();
    let mut latest_user = None::<String>;
    let mut counter = 0_u64;
    let mut native_id = |prefix: &str| {
        counter += 1;
        format!(
            "{prefix}_{counter:012x}{}",
            &Uuid::new_v4().simple().to_string()[..14]
        )
    };
    let mut index = 0;
    while index < session.events.len() {
        let event = &session.events[index];
        let (role, summary, part) = match event {
            Event::Text { role, text } => {
                ensure!(
                    matches!(role.as_str(), "user" | "assistant"),
                    "OpenCode cannot preserve privileged text"
                );
                (role.as_str(), false, json!({"type":"text","text":text}))
            }
            Event::Image {
                role,
                mime_type,
                data,
            } => {
                ensure!(role == "user", "OpenCode writer requires user image");
                (
                    "user",
                    false,
                    json!({"type":"file","mime":mime_type,"url":format!("data:{mime_type};base64,{data}")}),
                )
            }
            Event::Summary { text } => {
                // Public legacy compaction is a user marker + summary assistant.
                let mid = native_id("msg");
                let pid = native_id("prt");
                messages.push(json!({"info":{"id":mid,"sessionID":id,"role":"user","time":{"created":ms},"agent":"build","model":{"providerID":"anthropic","modelID":"unknown"}},"parts":[{"id":pid,"sessionID":id,"messageID":mid,"type":"compaction","auto":true}]}));
                latest_user = Some(mid);
                ("assistant", true, json!({"type":"text","text":text}))
            }
            Event::ToolCall {
                id: call,
                name,
                arguments,
            } => {
                ensure!(
                    arguments.is_object(),
                    "OpenCode requires object tool arguments"
                );
                let state = if let Some(Event::ToolResult {
                    id: result_id,
                    text,
                    is_error,
                    ..
                }) = session.events.get(index + 1)
                {
                    ensure!(
                        result_id == call,
                        "OpenCode tool result must immediately follow its call; refusing timeline reordering"
                    );
                    index += 1;
                    if *is_error {
                        json!({"status":"error","input":arguments,"error":text,"time":{"start":ms,"end":ms}})
                    } else {
                        json!({"status":"completed","input":arguments,"output":text,"title":name,"metadata":{},"time":{"start":ms,"end":ms}})
                    }
                } else {
                    ensure!(
                        !session
                            .events
                            .iter()
                            .skip(index + 1)
                            .any(|e| matches!(e, Event::ToolResult { id, .. } if id == call)),
                        "OpenCode tool result association would reorder history; unsupported"
                    );
                    json!({"status":"pending","input":arguments,"raw":serde_json::to_string(arguments)?})
                };
                (
                    "assistant",
                    false,
                    json!({"type":"tool","callID":call,"tool":name,"state":state}),
                )
            }
            Event::ToolResult { .. } => bail!("OpenCode orphan/nonadjacent result unsupported"),
        };
        let mid = native_id("msg");
        let pid = native_id("prt");
        let info = if role == "user" {
            latest_user = Some(mid.clone());
            json!({"id":mid,"sessionID":id,"role":"user","time":{"created":ms},"agent":"build","model":{"providerID":"anthropic","modelID":"unknown"}})
        } else {
            let parent = latest_user.as_ref().ok_or_else(|| {
                anyhow!(
                    "OpenCode assistant requires earlier user message; refusing synthetic parent"
                )
            })?;
            let mut info = json!({"id":mid,"sessionID":id,"role":"assistant","time":{"created":ms,"completed":ms},"parentID":parent,"modelID":"unknown","providerID":"anthropic","mode":"build","agent":"build","path":{"cwd":cwd,"root":cwd},"cost":0,"tokens":{"input":0,"output":0,"reasoning":0,"cache":{"read":0,"write":0}},"finish":if s(&part,"type") == "tool" {"tool-calls"} else {"stop"}});
            if summary {
                info["summary"] = json!(true);
            }
            info
        };
        let mut part = part;
        part["id"] = json!(pid);
        part["sessionID"] = json!(id);
        part["messageID"] = json!(mid);
        messages.push(json!({"info":info,"parts":[part]}));
        index += 1;
    }
    let bundle = json!({"info":{"id":id,"slug":format!("ams-{}", &id[4..16]),"projectID":"global","directory":cwd,"title":if session.title.is_empty() {"Imported session"} else {&session.title},"version":"1.17.20","time":{"created":ms,"updated":ms}},"messages":messages});
    let mut bytes = serde_json::to_vec_pretty(&bundle)?;
    bytes.push(b'\n');
    Ok(bytes)
}

/// Independently reparse staged bytes and compare every portable event, including
/// ordering. Only a documented target limitation can change the expected events.
pub fn verify(session: &Session, rendered: &Rendered, target: Format) -> Result<()> {
    let mut temp = tempfile::NamedTempFile::new()?;
    temp.write_all(&rendered.bytes)?;
    temp.flush()?;
    let parsed = load(temp.path(), Some(target))?;
    ensure!(
        parsed.native_id == rendered.id && parsed.native_id != session.native_id,
        "rendered identity is not fresh"
    );
    ensure!(
        parsed.events.len() == session.events.len(),
        "native round-trip changed portable event count"
    );
    for (actual, expected) in parsed.events.iter().zip(&session.events) {
        let equal = match (target, actual, expected) {
            (
                Format::Codex,
                Event::ToolResult {
                    id: a,
                    name: an,
                    text: at,
                    is_error: false,
                },
                Event::ToolResult {
                    id: b,
                    name: bn,
                    text: bt,
                    ..
                },
            ) => a == b && an == bn && at == bt,
            _ => actual == expected,
        };
        ensure!(equal, "native round-trip changed portable event timeline");
    }
    Ok(())
}

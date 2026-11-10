```text
   __ _ _ __ ___  ___
  / _` | '_ ` _ \/ __|
 | (_| | | | | | \__ \
  \__,_|_| |_| |_|___/
```

# ams — أمس

**Find the session. Pick up where you left off.**

You remember the conversation—not whether it happened in Claude, Codex, OMP, Pi, or OpenCode. `ams` searches those local stores together and opens the right session in its original agent and project.

- **Search words or meaning.** SQLite full-text search by default; optional local Model2Vec embeddings when the wording escapes you.
- **Browse, preview, resume.** Live interactive search, compact results, and matching conversation previews.
- **Stay local.** Rust CLI, one private SQLite catalog, no server or Python runtime.
- **Move only when you need to.** Session migration is optional; finding and continuing sessions comes first.

The name **أمس** means **yesterday** in Arabic.

[Quickstart](#find-it-then-continue) · [Search modes](#find-ask-and-pick) · [Supported agents](#supported-search-stores) · [Semantic search](#optional-model2vec-meaning-search) · [Privacy](#privacy) · [Benchmarks](#verification-and-benchmarks)

## Install

Requires **Rust 1.88+**. Linux is the exercised platform. Install [`fzf`](https://github.com/junegunn/fzf#installation) for the interactive picker; ordinary search does not need it.

```sh
cargo install --git https://github.com/abdelkareemkobo/ams --locked
```

Or build from a checkout:

```sh
git clone https://github.com/abdelkareemkobo/ams.git
cd ams
cargo install --path . --locked
```

Embedding support is compiled in by default, but **ordinary search never downloads a model**. For a smaller word-search-only build, add `--no-default-features` to either Cargo install command. Python is needed only for the comparison benchmark.

## Find it, then continue

```sh
ams                              # recent sessions; first run builds the index
ams find "oauth refresh"         # literal words across history, titles, projects
ams find "oauth refresh" --format codex
ams find "release" --titles-only
ams --refresh find "pool timeout" # incrementally refresh before searching
ams scan                         # explicit incremental refresh
ams show SESSION_ID              # full location, native identity, readable preview
ams resume SESSION_ID --print    # inspect a quoted command; do not launch
ams resume SESSION_ID            # launch the original agent in its recorded cwd
ams pick                         # live indexed word search, with a separate preview
ams pick "oauth" --resume        # search the full index and continue a selection
ams ask "expiring logins" --pick # live meaning search; preserve semantic ranking
```

`find` aliases: `search`, `f`. `ls` aliases: `list`. IDs accept unique catalog/native prefixes; ambiguous IDs fail instead of picking an arbitrary session. Default human output is a compact, terminal-width-aware table: ID, agent, complete date, abbreviated project, and title. It does not dump native paths or conversation snippets into every row. Use `--details` or `show ID` for structured locations and query-centered previews. `--json` preserves complete structured data and full IDs, including for `show`.

### `find`, `ask`, and `pick`

| Command | Search method | Requires embeddings? |
|---|---|---|
| `ams find "codetrail"` | Literal words in titles, readable history, project paths, and native IDs | No |
| `ams ask "where we debugged expiring logins"` | Model2Vec meaning search fused with literal word search; returns sessions, not generated answers | Yes |
| `ams pick` or `ams find "codetrail" --pick` | Live indexed word search as you type, with a separate preview | No |
| `ams pick --semantic` or `ams ask "expiring logins" --pick` | Live hybrid meaning search without fuzzy-filtering its ranked candidates | Yes |

Lexical queries are literal Unicode words, combined with AND, case-insensitive under SQLite's Unicode tokenizer. They are **whole-token**, not substring or typo matching; punctuation separates tokens. Search ignores word order. Use fewer words or `ask` when you remember the subject rather than its original wording.

The picker uses fzf as its interface, **not as a title-only search engine**. Each query reloads matching sessions from the full catalog; a project or a phrase deep in a conversation can match even when neither appears in the displayed title. An empty query shows recent sessions. Each live query displays up to 100 ranked matches; changing the query searches the whole index again. Enter selects, Esc cancels. The preview shows matching context before the full native identity/path. While a query reloads, Enter is disabled so it cannot accidentally select a result from the previous query.

Picker preview panes use the right side on wide terminals and the bottom on narrow ones. Human output clips/wraps by Unicode display cells, preserves emoji clusters, and escapes unsafe terminal controls. A one-time lexical index upgrade adds project/native-ID search without re-reading source sessions or invalidating existing embeddings.

The index covers stored readable user/assistant history, including older/inactive history retained in a journal. It is an archive finder, not a reconstruction of the current model context. Migration separately resolves active ancestry and reset boundaries.

First search/list automatically scans existing default stores. Subsequent searches use the existing catalog; refresh with `scan` or `--refresh` to see new/changed sessions. A failed root keeps its last indexed lexical location labeled `unavailable`; unavailable sessions cannot be resumed/migrated and are excluded from semantic results. A confirmed deleted file is marked `missing` and removed from search.

## Supported search stores

| Agent | Automatically discovered source | Resume |
|---|---|---|
| Claude Code | `$CLAUDE_CONFIG_DIR/projects`, otherwise `~/.claude/projects` | `claude --resume ID` |
| Codex | `$CODEX_HOME/sessions` and `archived_sessions`, otherwise `~/.codex/...` | `codex resume ID` |
| OMP | `~/.omp/agent/sessions`, older `~/.omp/sessions`, named profile stores | `omp --resume PATH` |
| Pi | `~/.pi/agent/sessions` | `pi --session PATH` |
| OpenCode | `$XDG_DATA_HOME/opencode`, otherwise `~/.local/share/opencode`; live legacy/v2 SQLite plus public JSON exports | `opencode --session ID` for installed sessions |

`PI_CODING_AGENT_DIR/sessions` is classified as Pi or OMP from its native head rather than indexed twice. `ams doctor` shows existing default roots and feature availability. OpenCode databases are opened read-only; `ams` never edits their schema, messages, or parts. An exported bundle is searchable but must be imported by the matching native CLI before resume.

This is **not** an 18-format replacement for [session-migrate](https://github.com/xhluca/session-migrate). Search and migration support are deliberately listed separately.

## Extra roots and scripts

A root can be a transcript directory, one transcript, or an OpenCode native data directory/database. Roots added by `scan` persist in this catalog.

```sh
ams scan --root omp=/backup/omp/agent/sessions --root codex=/archive/codex/sessions
ams --db /tmp/private-ams.sqlite3 scan --no-defaults --root pi=/archive/pi/sessions
ams roots
ams roots remove pi=/archive/pi/sessions # forget indexed data, never delete native files
ams find "oauth" --json | jq -r '.[] | [.id, .format, .path] | @tsv'
ams --jobs 8 scan
ams completions zsh > ~/.zfunc/_ams
```

`--no-defaults` avoids **adding** default roots; already registered roots remain active. Default stores removed with `roots remove` may be rediscovered by a later ordinary `scan`; use a separate catalog with explicit roots for a permanently isolated index.

Global `--db`, `--json`, `--details`, `--jobs`, and `--refresh` work before or after subcommands. `AMS_DB` selects a catalog. Default: `$XDG_STATE_HOME/ams/catalog.sqlite3`, otherwise `~/.local/state/ams/catalog.sqlite3`. `--jobs 0` selects available CPUs capped at eight. A partial explicit scan emits its report and exits nonzero; healthy indexed sessions remain searchable. JSON diagnostics go to stderr, not into the result stream. `transfer` always emits JSON manifests.

## Optional Model2Vec meaning search

No model is loaded or downloaded by ordinary search. Semantic commands return ranked sessions, **not generated answers**.

```sh
ams embed                                     # explicit download/index opt-in
ams ask "where we debugged expiring logins"    # lexical + vector reciprocal-rank fusion
ams ask "authentication retries" --semantic-only
ams ask "where we debugged expiring logins" --pick # the same ranked search, interactive
ams pick --semantic                            # meaning search as you type
ams embed --model /path/to/local/model
ams ask "an old conversation" --model /path/to/local/model
```

Default model: [`minishlab/potion-base-8M`](https://huggingface.co/minishlab/potion-base-8M), via [`model2vec-rs`](https://github.com/MinishLab/model2vec-rs). For Arabic/multilingual conversations, choose a compatible multilingual Model2Vec model using the **same** `--model` on `embed` and `ask`; the default model is English-oriented.

Readable history is divided into overlapping 1,800-character chunks (200-character overlap), plus a title vector. Similarity uses normalized cosine, taking the best chunk per session; hybrid search fuses lexical and semantic ranks. Exact model asset hashes, chunking policy, and content hashes prevent mixing incompatible vectors. New/edited sessions require another `embed` after `scan`; stale or missing vectors cause a clear error, not silent partial search. Unchanged sessions are not embedded again. Semantic search streams vector rows from SQLite and rejects brute-force inventories above two million chunks; restrict the agent with `--format` at that scale.

Model assets are cached locally. Once cached, local inference does not upload conversation text. Use a downloaded local model directory for strictly offline semantic operation. To omit the embedding dependency entirely:

```sh
cargo install --path . --locked --no-default-features
```

## Privacy

Unlike session-migrate's metadata-only catalog, **this catalog contains conversation text and searchable previews**, which can include secrets the user pasted into a conversation. Keep it private; do not commit or share it. New catalog files are mode `0600` on Unix and newly created catalog directories are `0700`. Existing non-private catalog files are rejected; permissions of unrelated existing directories are not changed.

The extractor does not recursively index tool arguments/results, private thinking, provider credentials, or runtime configuration. This is not a secret-redaction guarantee for ordinary conversation text. `--json` and `show` can expose previews. Deleting an index should also account for SQLite journal files and filesystem backups; native sources remain authoritative.

## Optional migration

```sh
ams transfer SOURCE_ID --to pi --dry-run
ams transfer /path/session.jsonl --from omp --to codex --home /tmp/codex-home --cwd "$PWD"
ams transfer SOURCE_ID --to omp --strict --dry-run
printf '%s\n' ID_A ID_B | ams --jobs 4 transfer - --to pi --home /tmp/pi-agent
ams transfer SOURCE_ID --to opencode --output /tmp/import-bundle.json
```

`mv` is an alias for portable migration, **not deletion**. Sources are never modified. `--home` is the target **native agent home** (`sessions/` is created beneath it), not a replacement for the user's HOME. Default targets honor `CODEX_HOME`, `PI_CODING_AGENT_DIR`, and OMP's named `OMP_PROFILE`. `--output` writes an artifact instead of installing a picker entry. Existing files are never overwritten; installed artifacts are atomically published with private permissions. `--dry-run` still parses, renders, and reparses to validate the portable timeline, but writes no target artifact. Temporary validation files may be created outside the target home.

| Source | Native target installation | Artifact-only target |
|---|---|---|
| OMP/Pi v3, supported Codex journals, Claude UUID journals, OpenCode public legacy/v2 bundles | OMP/Pi v3; Codex legacy rollouts | OpenCode **1.17.20** public import bundle |

Claude writing is unsupported. Live OpenCode database migration requires a native export first. OpenCode output must be imported using a matching 1.17.20 native CLI; it is **not** a validated v2 import artifact. Do not install a JSON file into an exports directory and assume it is resumable.

Every omission is counted in the manifest. Private reasoning and runtime state do not migrate; OMP external blob images are reported as omitted. `--strict` rejects any reported loss **before installation**, including discarded timing/model/usage metadata. Unsupported inherited/rollback Codex history, nonportable privileged roles, malformed trees, orphan tools, and OpenCode tool sequences that require reordering fail explicitly. Source format changes can require updated readers; the search extractor is more permissive than the migration validators.

## Verification and benchmarks

Measured local results, raw samples, optimization evidence, and exercised workflows: [benchmarks/README.md](benchmarks/README.md).

```sh
cargo test --locked
cargo test --locked --no-default-features
cargo build --release --locked
uv venv /tmp/session-migrate-venv
uv pip install --python /tmp/session-migrate-venv/bin/python /path/to/pinned/session-migrate
python3 scripts/benchmark.py \
  --ams target/release/ams \
  --baseline /tmp/session-migrate-venv/bin/session-migrate \
  --baseline-repo /path/to/pinned/session-migrate \
  --sessions 300 --repeats 7 --migration \
  --output benchmark-local/results.json --markdown benchmark-local/results.md
```

The harness requires baseline commit `45351ff95ce79684c23377920b92026ecbe9759b` (0.12.0), generates a deterministic OMP/Pi/Codex corpus, isolates HOME/XDG/targets, alternates implementations, and validates exact native ID sets outside timing. It reports median, p95, min/max and raw successful samples. Failed commands never count as fast runs. A fresh database is **not a cold filesystem cache**. Indexing covers the same source files but does more work in `ams` (conversation text, not just metadata). Body search is reported as an ams-only feature, not a baseline speedup. Optional large-session migration checks both outputs through the baseline parser and compares exact user/assistant text before timing.

## License

[MIT](LICENSE). Format research and sanitized adapter fixtures derive from [session-migrate](https://github.com/xhluca/session-migrate) under MIT; attribution is retained in `LICENSE` and [the fixture license](tests/fixtures/adapters/LICENSE).

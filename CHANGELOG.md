# Changelog

## 0.1.1

- Replace wrapping path/transcript dumps with a compact Unicode-width-aware table. Add explicit `--details` and human-readable `show`; JSON records retain full metadata.
- Replace the static title/project fuzzy picker with live full-catalog query reloads, matching snippets in a separate preview pane, and optional hybrid semantic mode.
- Add `find --pick`, `ask --pick`, and `pick --semantic` while preserving ranked meaning-search results instead of fuzzy-filtering them away.
- Disable selection during query reloads to prevent accepting stale results; preview matching context before long native IDs and paths.
- Index project paths and native IDs lexically; migrate existing catalogs without discarding conversation text or embeddings.
- Explain literal `find` versus meaning-based `ask` directly in CLI help and usage documentation.

## 0.1.0

- Search-first Rust CLI for finding forgotten sessions across Claude Code, Codex, OMP, Pi, and OpenCode stores.
- Incremental parallel indexing of readable conversation history in private SQLite FTS5; cross-agent search, native titles, centered previews, source locations, and safe ambiguous-ID handling.
- Stable integer FTS row identities and one durable transaction per scan eliminate repeated unindexed delete scans and per-file commits; catalog schema is versioned.
- Read-only OpenCode live legacy/v2 SQLite inventory, plus public exports; explicit unavailable/missing/error source states and persistent root management.
- Native resume commands, optional fzf selection, JSON output, shell completions, worker controls, isolated catalogs, and first-search discovery.
- Optional Model2Vec chunked semantic retrieval and lexical/vector rank fusion, with model/content identity checks and incremental embedding.
- Optional validated portable migration into OMP, Pi, and Codex; OpenCode 1.17.20 artifact export, explicit loss accounting, strict/dry-run modes, atomic no-clobber installation, and parallel batches.
- Correct native title precedence: current OMP title slot, trailing Pi names, Codex thread renames. Canonical Codex message history takes precedence over mirrored UI events.
- Reproducible search-first session-migrate comparison harness with deterministic native corpus, exact-ID correctness gates, isolated targets, raw timing samples, and separate body-search capability measurements.

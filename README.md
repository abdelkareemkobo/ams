```text
   __ _ _ __ ___  ___
  / _` | '_ ` _ \/ __|
 | (_| | | | | | \__ \
  \__,_|_| |_| |_|___/
```

# ams (أمس)

A Rust CLI for finding old coding-agent sessions and opening them in the original agent. It searches Claude Code, Codex, OMP, Pi and OpenCode stores.

أمس means "yesterday" in Arabic.

## Install

Requires Rust 1.88+. Tested on Linux. The interactive picker also needs [fzf](https://github.com/junegunn/fzf#installation).

```sh
cargo install --git https://github.com/abdelkareemkobo/ams --locked
```

For a build without embedding support, add `--no-default-features`.

## Usage

```sh
ams find "connection pool"       # search words in session text and metadata
ams pick                        # search interactively, with previews
ams pick "connection pool" --resume
ams resume SESSION_ID           # open in the original agent and project
ams show SESSION_ID             # full path, metadata and a preview
ams scan                        # update the index
```

The first search builds a local index. After that, run `ams scan` to pick up new sessions, or use `ams --refresh find "query"`.

`find` matches whole words, in any order. Every query word must appear in the title, readable conversation text, project path or native ID. It does not do substring or typo matching.

The picker searches the full index as you type, not just the titles on screen. Enter selects a session; Esc cancels. It shows up to 100 matches for each query.

Use `--format codex` to restrict an agent, `--details` for paths and excerpts, or `--json` for scripts. Session IDs accept unique prefixes.

## Search by meaning

`find` matches words. `ask` combines word search with Model2Vec similarity, so it can find a discussion without the exact wording. It returns sessions, not generated answers.

```sh
ams embed                                    # download the model and build vectors
ams ask "where we debugged expiring logins"
ams ask "expiring logins" --pick              # browse those results interactively
ams pick --semantic                          # meaning search as you type
```

Ordinary search never loads a model. The default embedding model is [potion-base-8M](https://huggingface.co/minishlab/potion-base-8M), loaded through [model2vec-rs](https://github.com/MinishLab/model2vec-rs). Inference runs locally.

For another model, including a multilingual one, use the same `--model` value when indexing and searching:

```sh
ams embed --model /path/to/model
ams ask "an old conversation" --model /path/to/model
```

The default model is English-oriented. History is embedded in overlapping chunks, not just session titles. After scanning changed sessions, run `ams embed` again. Unchanged sessions are skipped; stale vectors are rejected.

## Supported stores

| Agent | Default location |
|---|---|
| Claude Code | `~/.claude/projects` |
| Codex | `~/.codex/sessions` and `archived_sessions` |
| OMP | `~/.omp/agent/sessions` and named profiles |
| Pi | `~/.pi/agent/sessions` |
| OpenCode | `~/.local/share/opencode`, including live legacy/v2 databases |

Native environment overrides are respected. `ams doctor` lists the detected roots. OpenCode databases are read-only; exported JSON bundles are also searchable, but must be imported before they can be resumed.

Search includes older history still stored in journals. Migration uses the active history instead.

## Other roots and scripts

```sh
ams scan --root omp=/backup/omp/agent/sessions
ams roots
ams roots remove omp=/backup/omp/agent/sessions
ams find "oauth" --json | jq -r '.[] | [.id, .format, .path] | @tsv'
ams --jobs 8 scan
ams completions zsh
```

Added roots persist. Removing a root only removes its index entries, not its files. For an isolated catalog:

```sh
ams --db /tmp/ams.sqlite3 scan --no-defaults --root pi=/archive/pi/sessions
```

`--no-defaults` skips adding default stores; previously registered roots remain active. The default catalog is `$XDG_STATE_HOME/ams/catalog.sqlite3`, or `~/.local/state/ams/catalog.sqlite3`. Override it with `--db` or `AMS_DB`.

## Privacy

The catalog contains conversation text, including anything sensitive you pasted into a session. Keep it out of Git and do not share it. New catalog files have mode `0600` on Unix.

Tool arguments, tool results, private thinking and agent configuration are not indexed. This is not secret redaction: ordinary messages can still contain credentials. `show` and JSON output may include excerpts.

Native session files are not modified. Unavailable stores keep their last indexed locations, marked `unavailable`; confirmed deleted files are removed from search.

## Migration

Migration is optional. Sources are never deleted, including when using the `mv` alias.

```sh
ams transfer SESSION_ID --to pi --dry-run
ams transfer SESSION_ID --to codex --home /tmp/codex-home --cwd "$PWD"
ams transfer SESSION_ID --to omp --strict --dry-run
```

Native targets are OMP, Pi and Codex. `--home` means the target agent's native home, not the user's home directory. Existing files are never overwritten. Losses are listed in the JSON manifest; `--strict` rejects any loss before installation, including discarded timing or model metadata.

Claude is readable but not a target. Live OpenCode migration requires a native export first. OpenCode output is a **1.17.20 import bundle**, not a validated v2 artifact:

```sh
ams transfer SESSION_ID --to opencode --output /tmp/import.json
```

Private reasoning and runtime state do not migrate. OMP external blob images are omitted and reported. Unsupported histories or tool sequences fail rather than being silently reordered. This does not cover all 18 formats supported by [session-migrate](https://github.com/xhluca/session-migrate).

## Development and benchmarks

```sh
cargo test --locked
cargo test --locked --no-default-features
cargo clippy --locked --all-targets --all-features -- -D warnings
```

[Benchmark results and methodology](benchmarks/README.md) include raw samples and the comparison with session-migrate. Those timings are for ams 0.1.0; the 0.1.1 picker changes have not been re-benchmarked. Fresh-database runs used a warm filesystem cache. Body search has no equivalent operation in the baseline.

Run `python3 scripts/benchmark.py --help` for benchmark options. Python is only needed for this script.

## License

[MIT](LICENSE). Format research and sanitized fixtures derive from [session-migrate](https://github.com/xhluca/session-migrate); attribution is retained in `LICENSE` and [the fixture license](tests/fixtures/adapters/LICENSE).

# Local benchmark evidence

These timing reports are for **ams 0.1.0** (exact executable versions/hashes are in the JSON). The 0.1.1 terminal/picker redesign is verified separately and has not been re-benchmarked here.

- [300-session final run](results-300.md), [raw samples](results-300.json): search/catalog operations plus optional large OMP→Pi conversion.
- [3,000-session final run](results-3000.md), [raw samples](results-3000.json): larger-catalog scaling.
- [3,000-session pre-optimization run](results-3000-before.md), [raw samples](results-3000-before.json): retained evidence of the indexing bottleneck, not the shipped result.

Hardware: AMD Ryzen 9 9950X, 32 logical CPUs, Linux x86-64. Both tools run as independent CLI processes; ams release build, one scan worker. Nine alternating repetitions per operation. Baseline: session-migrate 0.12.0, commit `45351ff95ce79684c23377920b92026ecbe9759b`. Exact executable hashes, corpus hashes/bytes, commands, machine details, raw samples, medians and nearest-rank p95 are in each JSON report.

For public publication, the workstation-specific absolute path to the ams executable is normalized to `target/release/ams` in the JSON reports. Measurements, executable/corpus hashes, and all timing samples are unchanged.

The generated corpus is deterministic, uses sanitized native OMP/Pi/Codex fixture structures, and is shared by both tools. Native ID sets, incremental counts, late-conversation body retrieval, and unchanged source files are checked outside timing. Each fresh scan uses a new database; source files and OS caches are warm. No filesystem-cache drop or network-filesystem experiment was performed. These are process wall times including startup and JSON output generation, not in-process microbenchmarks. Memory/RSS was not measured.

Indexing is not identical work: ams indexes readable conversations as well as titles; session-migrate indexes metadata. Metadata/title queries have identical expected session sets. The baseline does not support equivalent body or semantic queries, so body search has no relative speedup. A Rust rewrite of all baseline adapters is not claimed.

The larger initial run revealed repeated FTS deletion scans on an unindexed text ID, plus a transaction per file. The shipped index uses stable integer primary keys for FTS row identity and one transaction per scan. Native sources are still read-only; durable SQLite synchronization is not disabled. Pre-optimization and final data are both retained rather than hiding the regression. An identity regression covers root removal, SQLite VACUUM, source edits, and removal of stale search hits.

Additional exercised behavior, separate from these synthetic timings:

- Read-only default discovery and known-session recall were exercised across all five native stores, including OMP profiles and OpenCode's shared database. Private source catalogs, conversation contents, and personal store inventories are not included in this repository.
- For each of the five real agent stores, a remembered body fragment found the original indexed session, and `resume --print` produced the correct native program. Real agents were not launched against private sessions.
- The actual interactive fzf picker was opened, filtered, and selected a synthetic session, returning its catalog ID with exit status zero.
- Actual Model2Vec inference used `potion-base-8M` revision `bf8b056651a2c21b8d2565580b8569da283cab23` from a local directory. A meaning-only query with **zero lexical hits** ranked the expected database discussion first. Editing a source rejected stale vectors, then re-embedded exactly the changed session and skipped the other two.
- An OMP fixture was installed into an isolated Pi home; the real Pi CLI successfully parsed it and exported HTML. Large-session benchmark conversion independently reparses both writers through session-migrate and checks exact user/assistant text.

Run `python3 scripts/benchmark.py --help` for options. Install session-migrate from the pinned commit above and pass its executable with `--baseline`. Models and private source catalogs are not part of the benchmark corpus.

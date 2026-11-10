# ams benchmark

Status: passed
Baseline revision: `45351ff95ce79684c23377920b92026ecbe9759b`

Fresh database is **not** cold filesystem cache. All corpus files are generated
and read before timing. Body search is an ams-only feature, not a speedup.

| Operation | Tool | Samples | Median ms | p95 ms | Min ms | Max ms |
|---|---|---:|---:|---:|---:|---:|
| cli_startup_help | ams | 9 | 0.715 | 0.943 | 0.478 | 0.943 |
| cli_startup_help | baseline | 9 | 46.321 | 47.365 | 43.003 | 47.365 |
| fresh_database_scan_warm_filesystem | ams | 9 | 1661.293 | 2653.480 | 1642.117 | 2653.480 |
| fresh_database_scan_warm_filesystem | baseline | 9 | 347.959 | 351.501 | 343.790 | 351.501 |
| warm_incremental_scan | ams | 9 | 30.889 | 32.346 | 29.627 | 32.346 |
| warm_incremental_scan | baseline | 9 | 103.461 | 107.944 | 101.201 | 107.944 |
| metadata_title_search | ams | 9 | 8.848 | 9.814 | 8.173 | 9.814 |
| metadata_title_search | baseline | 9 | 56.527 | 57.936 | 52.754 | 57.936 |
| conversation_body_search_ams_only | ams | 9 | 5.655 | 6.182 | 5.350 | 6.182 |

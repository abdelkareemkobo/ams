# ams benchmark

Status: passed
Baseline revision: `45351ff95ce79684c23377920b92026ecbe9759b`

Fresh database is **not** cold filesystem cache. All corpus files are generated
and read before timing. Body search is an ams-only feature, not a speedup.

| Operation | Tool | Samples | Median ms | p95 ms | Min ms | Max ms |
|---|---|---:|---:|---:|---:|---:|
| cli_startup_help | ams | 9 | 0.693 | 0.806 | 0.477 | 0.806 |
| cli_startup_help | baseline | 9 | 44.599 | 48.024 | 43.023 | 48.024 |
| fresh_database_scan_warm_filesystem | ams | 9 | 179.019 | 184.500 | 175.450 | 184.500 |
| fresh_database_scan_warm_filesystem | baseline | 9 | 348.722 | 351.276 | 345.784 | 351.276 |
| warm_incremental_scan | ams | 9 | 19.297 | 20.290 | 18.280 | 20.290 |
| warm_incremental_scan | baseline | 9 | 103.627 | 105.995 | 101.481 | 105.995 |
| metadata_title_search | ams | 9 | 8.771 | 9.255 | 8.091 | 9.255 |
| metadata_title_search | baseline | 9 | 55.656 | 60.642 | 54.152 | 60.642 |
| conversation_body_search_ams_only | ams | 9 | 5.904 | 6.905 | 5.608 | 6.905 |

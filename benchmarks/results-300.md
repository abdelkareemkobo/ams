# ams benchmark

Status: passed
Baseline revision: `45351ff95ce79684c23377920b92026ecbe9759b`

Fresh database is **not** cold filesystem cache. All corpus files are generated
and read before timing. Body search is an ams-only feature, not a speedup.

| Operation | Tool | Samples | Median ms | p95 ms | Min ms | Max ms |
|---|---|---:|---:|---:|---:|---:|
| optional_large_source_dry_run_omp_to_pi | ams | 9 | 37.697 | 39.773 | 36.983 | 39.773 |
| optional_large_source_dry_run_omp_to_pi | baseline | 9 | 131.802 | 134.183 | 130.763 | 134.183 |
| cli_startup_help | ams | 9 | 0.681 | 0.969 | 0.470 | 0.969 |
| cli_startup_help | baseline | 9 | 45.439 | 48.271 | 44.531 | 48.271 |
| fresh_database_scan_warm_filesystem | ams | 9 | 20.002 | 21.385 | 19.102 | 21.385 |
| fresh_database_scan_warm_filesystem | baseline | 9 | 77.450 | 81.062 | 75.106 | 81.062 |
| warm_incremental_scan | ams | 9 | 3.080 | 3.593 | 2.688 | 3.593 |
| warm_incremental_scan | baseline | 9 | 51.987 | 55.575 | 49.051 | 55.575 |
| metadata_title_search | ams | 9 | 1.964 | 2.214 | 1.555 | 2.214 |
| metadata_title_search | baseline | 9 | 48.871 | 50.298 | 47.896 | 50.298 |
| conversation_body_search_ams_only | ams | 9 | 1.553 | 1.757 | 1.431 | 1.757 |

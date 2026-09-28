# Farhand Benchmarks

Numbers produced by the criterion suites in `crates/fileset/benches/sync.rs`
and `crates/workspace/benches/cas_cow.rs`. Every number in the README must
be reproducible from this table: run `scripts/run_benchmarks.sh`
(optionally `--fast`) and commit the result together with the claim.

- **CPU:** Intel(R) Core(TM) Ultra 7 256V (8 cores)
- **Filesystem:** /tmp on tmpfs — CoW paths exercise `clonefile` (APFS) / `FICLONE` (Linux) / hardlink / copy, whichever the host supports
- **Mode:** `cargo bench --bench sync` / `--bench cas_cow` (release profile, criterion defaults)
- **Notes:** `scan_and_hash` is dominated by SHA-256 over warm page cache;
  CoW/CAS materialize speeds depend on the host filesystem (APFS `clonefile`,
  Linux `FICLONE`, hardlink or copy fallback) — see the note on each benchmark.

## Fileset / sync pipeline

| Benchmark | Mean | ± |
| :--- | ---: | ---: |
| Scan + SHA-256 hash of 320 files (~2.4 MiB) | `3.08 ms` | `±233.6 µs` |
| Pack delta tar with gzip (~2.4 MiB payload) | `311.81 ms` | `±2.15 ms` |
| Pack delta tar with zstd-3 (~2.4 MiB) | `73.77 ms` | `±695.7 µs` |
| Unpack gzip tar (~2.4 MiB) | `57.33 ms` | `±317.8 µs` |
| Unpack zstd tar (~2.4 MiB) | `18.32 ms` | `±156.1 µs` |
| SHA-256 of 320 files in isolation (no walk) | `6.17 ms` | `±116.1 µs` |
| Scan with cold digest cache: walk + full re-hash (parallel) | `3.11 ms` | `±234.5 µs` |
| Scan with warm digest cache: walk + stat only, no content read | `1.13 ms` | `±64.9 µs` |

## Workspace / CAS, CoW, diff

| Benchmark | Mean | ± |
| :--- | ---: | ---: |
| CAS store: ingest 64 KiB file (hash + atomic rename) | `169.3 µs` | `±7.8 µs` |
| CAS store: ingest 256 KiB file | `406.6 µs` | `±21.5 µs` |
| CAS hydrate 64 KiB via CoW reflink/hardlink | `24.5 µs` | `±1.1 µs` |
| CAS hydrate 256 KiB via CoW reflink/hardlink | `23.6 µs` | `±1.1 µs` |
| Workspace CoW clone: 100 files / ~200 KiB tree | `4.04 ms` | `±350.3 µs` |
| Manifest diff, empty remote workspace | `42.4 µs` | `±3.4 µs` |
| Manifest diff, warm workspace (nothing changed) | `1.08 ms` | `±45.0 µs` |

## Derived claims used in the README

- **zstd vs gzip pack throughput**: computed from `pack_tar_gzip_2mib` vs
  `pack_tar_zstd_2mib` means above.
- **CAS hydration is effectively free** (nanoseconds-per-KiB): see
  `cas_materialize_*` — reflink/hardlink cost is independent of payload size.
- **Parallel hashing**: `hash_file_320_files` is the same SHA-256 work
  done one file at a time; `scan_full_rehash` is that work spread across
  cores, so the gap between the two is the rayon speedup.
- **The stat gate**: `scan_full_rehash` vs `scan_stat_only` is what the
  digest index saves — the warm scan stats every file and reads none, so
  the residual cost is pure walk, not I/O.
- **Warm-workspace diff cost** is the price of the manifest protocol: the
  agent still has to walk the workspace to prove nothing changed. What it
  no longer does is re-read every byte to do it.

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
| Scan + SHA-256 hash of 320 files (~2.4 MiB) | `7.34 ms` | `±302.6 µs` |
| Pack delta tar with gzip (~2.4 MiB payload) | `322.28 ms` | `±9.16 ms` |
| Pack delta tar with zstd-3 (~2.4 MiB) | `77.16 ms` | `±2.49 ms` |
| Unpack gzip tar (~2.4 MiB) | `59.70 ms` | `±2.11 ms` |
| Unpack zstd tar (~2.4 MiB) | `19.32 ms` | `±740.1 µs` |

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
- **Warm-workspace diff cost** is the price of the manifest protocol: the
  agent rescans the workspace to verify hashes every run.


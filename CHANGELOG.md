# Changelog

All notable changes to Farhand are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Engineering
- **MSRV corrected to 1.88.0 and now enforced by CI.** The previous `1.75`
  claim was never true: the dependency graph requires 1.88 (`time`), and our
  own code uses `is_multiple_of` (stabilized in 1.87). Verified with
  `cargo +1.88.0 check --workspace --all-targets` + library tests on 1.88.0.
- **CI credibility suite**: coverage via `cargo-llvm-cov` + Codecov badge,
  cargo-deny policy (`deny.toml`: advisories/bans/licenses/sources — currently
  green with zero exceptions), RustSec audit, an MSRV job, nightly
  ThreadSanitizer and benchmark-trend jobs, Dependabot, and all third-party
  GitHub Actions pinned by commit SHA.
- Fuzz-lite property tests (nightly-free cargo-fuzz equivalent): deterministic
  pseudo-random byte streams and bit-flipped archives run through the frame
  reader, wire-path parser, and tar unpacker — crash-safety and zip-slip
  invariants verified in CI on all platforms (nightly cargo-fuzz targets
  planned as a follow-up).

### Fixed
- **CAS housekeeping**: new `--cas-ttl-days` (default 30) / `--cas-max-gb`
  eviction for content-addressable objects (previously unbounded growth);
  hydration touches object mtimes so eviction is LRU-by-use; `put_file` tmp
  names are unique per call (concurrent stores of the same hash no longer
  race); the `cas/` directory no longer counts as a workspace in quota
  accounting.
- **Collision-free run IDs**: run identifiers were `millis ^ pid` — two runs
  starting in the same millisecond collided, clobbering STATUS entries and
  history files. IDs are now nanosecond-timestamp + process counter.
- **Synchronous active-build guard drop**: finished runs no longer linger in
  STATUS (and no detached task panics at runtime shutdown).
- **Panic hygiene**: `fhd --shell " "` (empty invocation) is rejected at
  startup instead of panicking the connection task; `fh top` / `fh history`
  truncate long commands on UTF-8 char boundaries (multibyte command names
  used to panic); TLS SNI handles bracketed IPv6 literals (`[::1]:9876`);
  malformed `--forward` specs and unknown `--compression` values are hard
  errors instead of being silently ignored.
- **Queued-connection disconnect detection**: a queued run whose client
  disconnects now frees its slot immediately (a watchdog owns the read half
  during queue waits) instead of holding it until the lock frees.
- **Bounded queue**: new `--max-queued-runs` (default 16) rejects runs beyond
  the cap instead of accepting unbounded memory growth.
- **Blocking I/O off the async runtime**: workspace diff, delta unpack, and
  history saves run via `spawn_blocking`; PTY child exit is awaited through a
  blocking `wait()` task instead of a 20 ms poll loop.
- **Client safety**: `fh init --token` warns about the plaintext write and a
  new `--token-env` flag writes the `${FARHAND_TOKEN}` interpolation form;
  watch mode uses a bounded event channel (drop-on-full coalescing); agent
  pool selection no longer relies on an `unwrap()`.

### Security
- **`fhd` now requires an authentication token by default** and refuses to start
  without one unless `--allow-unauthenticated` is passed explicitly. Empty tokens
  are rejected as a misconfiguration. Loud warnings are emitted for
  non-loopback binds: unauthenticated mode, and cleartext tokens without TLS.
- Tokens are now compared in **constant time** (SHA-256 digests,
  `protocol::secure::ct_eq_tokens`), across HELLO/RUN, STATUS, HISTORY, and
  CLEAN — no length or prefix leakage.
- **Framing hardening**: payload memory grows only as bytes actually arrive
  (64 KiB incremental reads) instead of trusting the header's claimed length;
  a new 1 MiB pre-authentication frame cap (`MAX_PRE_AUTH_PAYLOAD`) rejects
  oversized first frames before auth.
- **Connection cap**: new `fhd --max-connections` (default 32, `0` = unlimited);
  excess connections are closed immediately.
- **Environment forwarding denylist expanded**: infrastructure credential
  prefixes (`AWS_`, `AZURE_`, `GCP_`, `GITHUB_`, `GITLAB_`, `DOCKER_`, `NPM_`,
  `PYPI_`, `DATABASE_`, `INFISICAL_`, …) and credential suffixes (`*_TOKEN`,
  `*_SECRET`, `*_PASSWORD`, `*_API_KEY`, `*_ACCESS_KEY`, `*_PRIVATE_KEY`, …)
  are never forwarded implicitly. New `fh --print-env` dry-run lists exactly
  which variable names would be forwarded (values are never shown). Explicit
  `-e VAR=value` overrides always win.
- **CoW cloning is now pure Rust**: the `cp -c -R` / `cp --reflink=auto -a`
  subprocesses are gone; the recursive clone preserves permission modes,
  modification times, and symlinks; FICLONE clones now carry permission bits
  over; and the hardlink fallback was removed — a hardlinked file modified in
  place used to silently corrupt the shared CAS object or seed workspace
  behind it (data-correctness fix).
- **GC/CLEAN lock coordination**: background GC, emergency disk GC, and
  `fh clean` never delete or trim workspaces that have an active run; `clean`
  reports busy workspaces instead of racing them, and deletion failures are
  logged instead of reported as success.
- **mTLS e2e coverage**: end-to-end test proving a valid client certificate
  completes a full run roundtrip and that clients without a client certificate
  are rejected.
- **Fuzz-lite property tests**: deterministic pseudo-random byte streams and
  bit-flipped archives run through the frame reader, wire-path parser, and
  tar unpacker — crash-safety and zip-slip invariants verified in CI on all
  platforms.
- **Real libFuzzer targets** (`fuzz/`): `fuzz_read_frame`, `fuzz_unpack_tar`,
  `fuzz_wire_paths` assert protocol invariants (payload-cap respect, incremental
  allocation, zip-slip containment, traversal rejection). A 60-second smoke pass
  per target runs on every PR; a 10-minute pass runs nightly. Committed seed
  corpora capture known attack vectors (traversal tar, absolute-path tar, lying
  frame header).
- New `fhd` flag: `--max-connections`.

## [1.7.0] - 2026-09-22

### Added
- `fh top` — interactive live TUI dashboard (CPU, RAM, disk, active builds) and `fh top --once` snapshot mode
- `fh agent info` — formatted host telemetry with `--json` machine-readable output
- `fh lsp` — Language Server Protocol offloading (`rust-analyzer`, `pyright`, `gopls`, `clangd`) with local↔remote URI translation and per-file sync on save
- Documentation: LSP integration guide

### Fixed
- Windows type inference in toolchain wrapping; clippy warnings across the workspace; canonicalized paths in init tests

## [1.6.0] - 2026-09-22

### Added
- Native zero-config TLS via `rustls` (no OpenSSL): `fhd --tls-auto` self-signed generation, `fh --tls-fingerprint <sha256>` TOFU pinning, `--tls-ca` CA verification, and mutual TLS client certificates (`--tls-cert`/`--tls-key`)
- Declarative toolchain manager hooks: `-T rust=nightly` / `.farhand.yaml` toolchain blocks inject `RUSTUP_TOOLCHAIN`, `PYENV_VERSION`, `NODE_VERSION` and wrap invocations with `nvm`/`fnm`/`pyenv`/`goenv`

## [1.5.0] - 2026-09-22

### Added
- `fh init` — auto-detects project type and generates `.farhand.yaml` (and optionally customizable template definitions via `--with-template`)

### Changed
- Git pre-push hook enforcing `cargo fmt`, `cargo clippy -D warnings`, and workspace tests

## [1.4.0] - 2026-09-22

### Added
- Zstandard (zstd) wire compression with automatic handshake negotiation (zstd / gzip / none)
- Global content-addressable storage (CAS): SHA-256-deduplicated object store shared across all projects and branches, hydrated via zero-copy reflinks (`clonefile` / `FICLONE`)

## [1.3.0] - 2026-09-22

### Added
- Pre-flight disk-space guard with emergency GC (`--min-disk-gb`)
- `fh shell` — interactive remote PTY shell inside the project workspace
- `fh exec` — ad-hoc remote command execution bypassing hooks and artifact downloads
- Dynamic output matrix

### Fixed
- Robust host shell resolution and login-shell allocation; graceful spawn error handling
- Watch mode infinite rebuild loop and clean Ctrl+C handling; TTY command spawning

## [1.2.0] - 2026-09-21

### Added
- `fh watch` — continuous mode: debounced local file watching, delta sync, automatic rebuilds
- Interactive PTY allocation (`-t/--tty`) with SIGWINCH resize forwarding
- Reverse port forwarding (`-L/--forward`)

### Fixed
- Windows cross-platform `select!` unit futures
- Homebrew formula sha256 checksums for release tarballs

## [1.1.0] - 2026-09-15

### Added
- Ambient environment variable forwarding with Infisical support, `--no-env`, and `-e VAR=value` overrides
- `--version` flag and enriched Cargo package metadata

## [1.0.0] - 2026-09-15

### Added
- Initial release: two static binaries (`fh` client, `fhd` daemon) with zero external system binaries
- Custom length-prefixed TCP framing protocol (JSON payloads, SHA-256 manifests)
- Fileset scanning, ignore engine, tar/gzip pack & unpack with zip-slip protection
- Persistent per-project remote workspaces, manifest diffing, delta sync, and Section 5.1 deletion safety
- Artifact retrieval (`--output`, `--out-dir`) with per-language presets
- `.farhand.yaml` configuration with env var interpolation, exit-code standard (125 = infra), and verbose telemetry
- Cross-platform hardening (Windows `cmd /C` vs Unix `/bin/sh`, slash normalization) and CI on 3 OSes
- Pluggable template system (project/user/builtin resolution, `go:embed`-style `include_str!` defaults)
- Concurrency limits, job queuing (`QUEUED` frames), process-group cancellation
- Multi-agent pool with health probing, tag routing, and load balancing
- Dependency caching hooks via template hints and lockfile hashing
- Run observability (`fh history`) and packaging/distribution (release workflow, Homebrew, install scripts, systemd/launchd units)
- APFS Copy-on-Write workspace branching, two-tier LRU + emergency disk GC, `fh clean`

[Unreleased]: https://github.com/Rayrsn/farhand/compare/v1.7.0...HEAD
[1.7.0]: https://github.com/Rayrsn/farhand/compare/v1.6.0...v1.7.0
[1.6.0]: https://github.com/Rayrsn/farhand/compare/v1.5.0...v1.6.0
[1.5.0]: https://github.com/Rayrsn/farhand/compare/v1.4.0...v1.5.0
[1.4.0]: https://github.com/Rayrsn/farhand/compare/v1.3.0...v1.4.0
[1.3.0]: https://github.com/Rayrsn/farhand/compare/v1.2.0...v1.3.0
[1.2.0]: https://github.com/Rayrsn/farhand/compare/v1.1.0...v1.2.0
[1.1.0]: https://github.com/Rayrsn/farhand/compare/v1.0.0...v1.1.0
[1.0.0]: https://github.com/Rayrsn/farhand/releases/tag/v1.0.0
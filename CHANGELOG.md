# Changelog

All notable changes to Farhand are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Planned
- Benchmark suite (criterion) with published numbers backing README performance claims
- Content-defined chunking for sub-file delta sync
- Opt-in Prometheus metrics endpoint on `fhd`

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
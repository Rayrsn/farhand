# Contributing to Farhand

Thanks for helping improve Farhand! This document covers everything needed to
get a patch merged.

## Development Setup

Farhand is a pure-Rust Cargo workspace. There are no system dependencies
beyond a Rust toolchain.

```bash
# Clone & build
git clone https://github.com/Rayrsn/farhand.git
cd farhand
cargo build

# Run the full suite (unit + e2e)
cargo test --workspace
```

The workspace builds seven crates: `fh` (client), `fhd` (daemon), and the
libraries `protocol`, `fileset`, `workspace`, `templates`, `config`. See
[docs/architecture.md](docs/architecture.md) for how they fit together and
which crate may import which.

## Code Style & Checks

All three checks run automatically on push via `.githooks/pre-push`:

```bash
# One-time hook setup
git config core.hooksPath .githooks

# Manual equivalents
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

House rules:

- **Zero external system binaries** — never invoke `ssh`, `rsync`, `tar`,
  `gzip`, or `cp`; use the std library / existing internal crates.
- **Wire paths always use forward slashes** (`to_wire_path` / `from_wire_path`
  in `crates/protocol/src/path.rs`) — never pass raw OS paths into frames.
- **Archives must reject traversal** — any new unpack path goes through the
  containment checks in `crates/fileset/src/tar.rs`.
- **Never delete ignored directories** (`node_modules/`, `target/`, …) during
  workspace sync — see the §5.1 deletion-safety test in
  `crates/workspace/src/lib.rs`.
- Wrap errors with `%w`-style context (`anyhow`/`thiserror` patterns), use
  `tracing` for structured logs, and avoid `unwrap()`/`expect()` outside tests.

## Commit & PR Style

- Conventional Commits: `feat:`, `fix:`, `docs:`, `test:`, `chore:`, `bench:` —
  with optional scopes, e.g. `fix(watch): prevent infinite rebuild loop`.
- One logical change per PR. Update `CHANGELOG.md` under **Unreleased** for
  user-visible changes.
- Bump `[workspace.package].version` in `Cargo.toml` for feature releases
  (minor) or fixes (patch) — the release workflow publishes on `v*` tags.

## Testing Expectations

- **Unit tests**: every library crate has table-driven tests next to the code.
  New logic needs new tests — especially security-sensitive paths (escaping,
  traversal, auth).
- **E2E**: integration tests spin up `fhd::run_server` in-process on
  `127.0.0.1:0` and drive it with real clients; see
  `crates/fh/tests/e2e_pipeline.rs` for the established patterns.
- Cross-platform: use `cfg!(windows)` duals rather than Unix-only commands in
  tests.

## Reporting Bugs & Vulnerabilities

- Bugs: open a [bug report](https://github.com/Rayrsn/farhand/issues/new?template=bug_report.md)
- Security issues: **never** in public issues — see [SECURITY.md](SECURITY.md)

## Questions

Open a GitHub Discussion or a draft PR early — small PRs reviewed early move
faster than large ones reviewed late.
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
cargo clippy --workspace --all-targets -- -D warnings -D clippy::undocumented_unsafe_blocks
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

The local gate above is not the whole story: CI additionally type-checks the
**musl** target, because that is what the release artifacts are built from and
glibc does not agree with musl about libc types (`ioctl`'s request parameter is
`c_ulong` on glibc, `c_int` on musl — a real bug that shipped green locally and
on a glibc-only matrix). If you touch FFI, check both.

## Code layout

Where things live, so you can find the right file before grepping:

**`fhd` (agent daemon)** — `src/lib.rs` holds the accept loop, TLS dispatch,
and the per-connection run pipeline. The rest is split by responsibility:
`active.rs` (active-build registry; entries are removed by a guard so a
cancelled run cannot leak a slot), `exec.rs` (argv → OS process: shell
selection, escaping, toolchain wiring, process-group kill), `stream.rs`
(stdout/stderr streaming, PTY sessions, reverse port forwarding), and
`session.rs` (server configuration, startup validation, agent identity).

**`fh` (client)** — `src/cli.rs` is the entire CLI contract (every flag and
subcommand, as clap derives). `src/main.rs` holds the runtime: `run_build`
(one remote invocation, taking a `RunParams` struct), `run_watch` (the
change → rebuild loop), `perform_handshake` (HELLO/HELLO_ACK), and subcommand
dispatch. Shared client logic lives in the library modules beside it
(`client.rs`, `pool.rs`, `history.rs`, `init.rs`, …) so the binary stays a
thin coordinator.

**Libraries** — `protocol` is the pure wire layer and imports nothing from
the other crates; `fileset` is filesystem/archiving only; `workspace` owns
agent-side storage (CAS/CoW, locks, GC, history). Cross-imports between
those three are a bug, not a style question.

## Commit & PR Style

- Conventional Commits: `feat:`, `fix:`, `docs:`, `test:`, `chore:`, `bench:` —
  with optional scopes, e.g. `fix(watch): prevent infinite rebuild loop`.
- One logical change per PR. Update `CHANGELOG.md` under **Unreleased** for
  user-visible changes.
- Bump `[workspace.package].version` in `Cargo.toml` for feature releases
  (minor) or fixes (patch) — the release workflow publishes on `v*` tags.

## Unsafe code

farhand is `#![forbid(unsafe_code)]` everywhere *except* a small, audited set
of FFI calls in `fhd` (hostname, kill(2), load/memory probes) and
`workspace` (clonefile, FICLONE, statvfs). Every one of those blocks carries a
`// SAFETY:` comment explaining the preconditions it relies on, and CI runs

```bash
cargo clippy --workspace --all-targets -- -D warnings -D clippy::undocumented_unsafe_blocks
```

so a new undocumented `unsafe` block fails the build. If you need to add
unsafe code, the bar is: prove the preconditions in the SAFETY comment, keep
the FFI surface as small as possible, and prefer a `std`-only alternative when
one exists. Miri is not part of CI — it cannot follow FFI, so it would only
cover the non-unsafe majority.

## Fuzzing

The wire protocol parsers are fuzzed with libFuzzer (requires a nightly
toolchain and `cargo-fuzz`):

```bash
rustup toolchain install nightly
cargo install --locked cargo-fuzz

cargo +nightly fuzz list                       # available targets
cargo +nightly fuzz run fuzz_read_frame        # frame reader
cargo +nightly fuzz run fuzz_unpack_tar        # tar unpacker (zip-slip)
cargo +nightly fuzz run fuzz_wire_paths        # wire-path parser

# Reproduce a crash from CI artifacts:
cargo +nightly fuzz run fuzz_read_frame fuzz/artifacts/fuzz_read_frame/crash-*
```

Targets live in `fuzz/fuzz_targets/` and assert *invariants*, not just
"no panic": the frame reader never exceeds its payload cap and allocates only
as bytes arrive; the unpacker never writes outside the destination; the
wire-path parser never accepts traversal, backslashes, or absolute paths.
Committed seed corpora (`fuzz/corpus/*/seed_*`) encode known attack vectors
(traversal tar, absolute-path tar, lying frame header); generated coverage
corpora are gitignored. CI runs a 60-second smoke pass per target on every PR
and a 10-minute pass nightly.

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
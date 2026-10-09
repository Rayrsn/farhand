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

## Publishing to crates.io

The crates publish under the `farhand-*` namespace (`farhand` itself is taken
on crates.io by an unrelated project). Each package keeps its library name and
its `fh` / `fhd` executable names, so nothing a user types changes.

Publish in dependency order — a crate cannot be packaged until the ones it
depends on exist on the registry:

```bash
for c in farhand-protocol farhand-fileset farhand-config \
         farhand-templates farhand-workspace farhand-agent farhand-cli; do
  cargo publish -p "$c"    # or --dry-run to check first
done
```

`cargo publish --dry-run` only fully succeeds for crates whose dependencies
are already published, so expect `no matching package named farhand-protocol
found` for everything after the first until the chain exists. That is
expected, not a packaging defect.

**crates.io rate-limits new crates per time window.** Publishing a new version
of an already-published crate is not affected, but the *first* publish of each
crate is. If you hit `429 Too Many Requests`, wait for the timestamp the error
gives you and resume from the crate that failed — the ones before it are
already live and must not be re-published.

The release workflow builds the binaries from the tag; publishing is
deliberately a separate, manual step so a bad package cannot ship on its own.

### Traps that cost a whole chain

- **An `include_str!` must not reach above its own crate directory.** Cargo
  packages only the files under the crate, then verifies by building that
  tarball. A crate that reads a repository-level file compiles fine in-tree and
  passes every test, then fails for every user who installs it. The builtin
  templates used to live in a root-level `templates/` and would have broken
  exactly this way; a test in `farhand-templates` now prevents it.
- **`cargo install` takes crate names, not binary names.** The binaries are `fh`
  and `fhd`; the packages are `farhand-cli` and `farhand-agent`.
- **A published version is permanent.** It can be yanked but never deleted or
  replaced, so a metadata mistake ships as-is until someone notices.

## Commit & PR Style

- Conventional Commits: `feat:`, `fix:`, `docs:`, `test:`, `chore:`, `bench:` —
  with optional scopes, e.g. `fix(watch): prevent infinite rebuild loop`.
- One logical change per PR. Update `CHANGELOG.md` under **Unreleased** for
  user-visible changes.
- Bump the version as described in [Versioning](#versioning).

## Versioning

Farhand follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
There is exactly **one** place the version is written by hand:

```
Cargo.toml → [workspace.package] → version
```

Every crate inherits it with `version.workspace = true`, and the published
package names (`farhand-cli`, `farhand-protocol`, …) never change across
releases — only the version moves.

### What a bump touches

| File | On a version bump |
| :--- | :--- |
| `Cargo.toml` | Edit by hand. The single source of truth. |
| `crates/*/Cargo.toml`, `fuzz/Cargo.toml` | Every intra-workspace **path dependency** must carry the same version. CI enforces this at `major.minor` and fails on drift. |
| `Cargo.lock` | Never edit. `cargo check` regenerates it. |
| `flake.nix`, `default.nix` | Never edit. Both read the version out of `Cargo.toml` via `builtins.fromTOML`. |
| `Formula/farhand.rb` | **Release-time only** — see below. |
| `SECURITY.md` | Move the "latest" row up to the new line. |
| `CHANGELOG.md` | Add the release heading and its compare link at tag time. |

### Choosing the bump

- **minor** — a new capability or stage lands: a new subcommand, a new flag, a
  new protocol feature. The middle number goes up and the patch number resets
  to `0`.
- **patch** — a fix or hardening with no new surface. Only the patch number
  goes up.

Deliberately avoid concrete version numbers in prose and in examples. A literal
like "pin `--version 1.2.3`", or "`1.2` vs `1.2.0`", is true on the day it is
written and a lie the next, and nobody re-reads it. Write `<tag>`,
`<major>.<minor>`, or `X.Y.Z` instead, and point at the file that actually
holds the value.

Bump the version when you *complete* the change, not when you open the PR.
The workspace version is therefore allowed to sit one ahead of the newest
published release; that is intentional, so release artifacts built from a tag
are already labelled correctly.

### Releasing

The order matters, because several checks compare against the **published**
release rather than the working tree:

1. Land the bump and the changelog entry under **Unreleased**, with CI green.
2. **Wait for a fully green CI matrix.** Never tag on a red matrix — a release
   cut from a partial matrix ships a broken artifact.
3. Tag `vX.Y.Z`. The release workflow builds the binaries and publishes to
   crates.io.
4. Verify the shipped artifacts (checksums, and actually run the binaries).
   A green workflow is not evidence that the artifacts work.
5. **Only now** update `Formula/farhand.rb` — its version and the `sha256`
   checksums of the artifacts that now exist. The nightly
   `homebrew-formula` job compares the formula against the latest GitHub
   release tag and fails on a mismatch, so the formula must never be bumped
   ahead of an actual release. A formula that lags silently serves a stale
   version; one that runs ahead breaks the build.

`SECURITY.md` lists only the newest line as supported. When a release lands,
the previous line moves to the "upgrade" row so the table never advertises
support for something nobody patches.

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
corpora are gitignored. Add `--build-std` when running on a toolchain that
requires an instrumented standard library.

Fuzzing runs nightly on CI as a **blocking** job for that workflow. It is
deliberately not a PR gate — a nightly fuzzer gates nothing downstream, so a
failure there is information rather than noise — but it is no longer
`continue-on-error`. The earlier note here claimed the job was red forever
because "the hosted runner links the CRT statically, which libFuzzer refuses";
that diagnosis was wrong. It had three independent causes, all fixed: the
`--build-std` build compiles `compiler_builtins` with `crt-static`, which
libFuzzer's sanitizer rejects (rustc names the fix in the error), `musl-tools`
was missing for the C shims, and `cc-rs` was reaching for a musl C++ toolchain
on a glibc runner, so the target triple is now pinned. See the job comment in
`.github/workflows/nightly.yml`. Per-PR fuzz coverage comes from the fuzz-lite
property tests in the 3-OS test matrix.

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
# Changelog

All notable changes to Farhand are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- **The agent no longer re-hashes the whole workspace on every request.** Every
  delta sync re-scanned the remote tree and re-read every byte of it, so a
  warm no-op sync was O(total workspace bytes) — the largest latency cost in
  the product, and entirely avoidable. `fileset::scan` now walks and stats
  first, then hashes only what changed, and the digests are persisted to a
  `.farhand-hashindex.json` in the workspace root. A warm sync is now
  stat-only. Reuse is gated on `(size, mtime, change token)`: the kernel change
  time is included because a `(size, mtime)` gate is blind to a same-size
  overwrite with a preserved mtime, which is precisely what `rsync -a`, `tar
  -p`, and `cp -p` produce. The remaining hole in the gate — a modification
  landing in the same filesystem timestamp tick as the cached `stat` — is
  closed with git's "racily clean" rule: any entry whose mtime is not strictly
  older than the moment the index was written is re-hashed.
- **The change token works on Windows too.** `std::os::windows::fs::MetadataExt`
  exposes no change time on stable, so a straight port of the gate would have
  silently degraded to `(size, mtime)` there — and Windows is exactly where
  `robocopy` and archive extraction preserve timestamps. The agent now reads
  the NTFS change time through `GetFileInformationByHandleEx`, which the
  filesystem maintains and `SetFileTime` cannot forge. It costs one
  `CreateFileW` per file requesting only `FILE_READ_ATTRIBUTES` with full
  sharing, so it neither blocks nor is blocked by a concurrent build. The
  probe lives in `workspace::change_token` because `fileset` is
  `#![forbid(unsafe_code)]`; `workspace` already carries the audited-FFI
  pattern used by the CoW and `statvfs` sites. The `windows-sys` dependency is
  target-gated to Windows and pulls in no new crates on other platforms.
- **Cold scans hash in parallel.** The walk and the hashing are now separate
  phases, and the hashing phase runs across all cores via `rayon`.
- **`bench_scan_stat_gate` and `bench_hash_file`.** The existing
  `scan_and_hash_320_files` blended traversal with SHA-256 and could not
  attribute a regression to either. The new pair isolates hashing from
  walking, and puts a stat-only scan next to a full re-hash so the gate's
  effect is a number rather than a claim.
- **A complete CLI reference (`docs/cli.md`)** covering every subcommand and
  global flag, plus the exit-code contract and the `fhd` options. Eight global
  flags were documented nowhere at all: `--insecure-skip-token`, `--log-level`,
  `--log-format`, `--branch`, `--no-branch-scope`, `--out-dir`, `--no-cache`,
  and `--force`.
- **A templates guide (`docs/templates.md`)** for the `fh templates`
  subcommands — `list`, `show`, `init`, `push` — which shipped without a
  single mention in any user-facing document, along with the template YAML
  schema, the project/user/builtin resolution order, and the distinction
  between `ignoreExtra`, `outputs`, and `outputsIgnore`.

- **An agent reference (`docs/agent.md`)** covering every `fhd` flag — resource
  limits, disk and CAS tuning, pool tags, and TLS — together with an explicit
  statement of the security posture: a valid token is equivalent to shell
  access as the user running the daemon, and TLS is off by default.
- **An ignore-rules guide (`docs/ignores.md`)** documenting the four sources
  that decide what is synced, the built-in defaults that cannot be turned off,
  `.gitignore` and `.farhand-ignore` handling, negation, and how to ask
  `fh why`.
- **The digest index** (`.farhand-hashindex.json`) is now documented in the
  storage guide: why it exists, the `(size, mtime, change token)` gate and why
  the change time is part of it, the racy-stamp rule, and that deleting it is
  safe and costs only one re-hash.
### Fixed
- **Documented reverse port forwarding, and fixed its config validation.** `-L/--forward`
  has shipped since 1.0.0 and appears in no user-facing documentation: not the
  README, and not one of the guides. The only mention anywhere was in a
  gitignored roadmap file that described it as a *proposed* capability, so a
  feature that works was both invisible and documented as unbuilt. Added
  `docs/port-forwarding.md` and wired it into the README's feature list,
  quickstart, comparison table, and documentation index.
  While writing the guide, a related gap turned up: `-L` validated its spec
  strictly, but a `forward:` list in `.farhand.yaml` was never checked, so a
  typo there meant the tunnel silently did not exist and surfaced as a
  baffling "my dev server is unreachable". Both are now validated against the
  effective list, with the error naming which source was at fault.

  Verified end to end against a live agent: bidirectional traffic including
  POST bodies, multiple simultaneous forwards with independent port mappings,
  the `.farhand.yaml` form, and re-establishment across `fh watch` rebuilds.
- **Seven `.farhand.yaml` keys were undocumented**, including `tls` — a
  headline security feature with no entry in the configuration reference at
  all. The field table now covers `forward`, `toolchain`, `tty`,
  `compression`, `tls`, `agents`, and `tags`, with a new Transport Security
  section covering certificate pinning, custom CAs, and mutual TLS.

  All of it was verified against the built binary rather than written from the
  source: the template commands were run live (including pushing to a running
  agent), and the documented configuration parses, with `doctor` confirming
  the TLS and toolchain fields resolve.

- **Corrected the exit-code table**, which had been copied from `AGENTS.md`
  without checking the code. A build killed by a signal reports `1`, not
  `128 + N` — the agent maps a process that died without an exit status to a
  plain failure — and `126` is never produced at all, though both `AGENTS.md`
  and the first version of the CLI reference claimed otherwise. Verified
  against a live agent: `exit 3` → `3`, a nonexistent command → `127`, a
  non-executable file → `127`, `kill -TERM $$` → `1`, `kill -KILL $$` → `1`.
- **The Homebrew formula shipped a published token.** `Formula/farhand.rb`
  carried a `service do` block that started `fhd --listen 0.0.0.0:9876` with
  `FARHAND_TOKEN: "replace-with-your-token"`, so a plain
  `brew services start farhand` produced an agent on all interfaces whose only
  credential was readable in this repository — and an fhd token authorises
  arbitrary command execution in every workspace. The `service` block is gone:
  a formula service starts with no prompt and no way to supply a secret, so
  any token baked into it is a published one. The daemon now also refuses its
  own documentation placeholders outright, which covers the launchd plist,
  hand-written units, and anything copy-pasted from the README.
- **Non-UTF-8 build output silently truncated the log.** Both non-PTY paths
  used `AsyncBufReadExt::lines()`, which returns `Err` on invalid UTF-8 — and
  `while let Ok(Some(line))` treats that as end-of-stream, so one bad byte from
  a compiler discarded the rest of the output with no error anywhere. The
  reader now splits fixed-size chunks on `\n` and decodes lossily, and holds no
  more than 64 KiB for a line, so a child emitting one enormous unterminated
  line can no longer exhaust the agent's memory.
- **GC could delete a workspace with an active run.** Locks are keyed by the
  client's project name, but GC compared the on-disk *directory* name, which
  `resolve_workspace_dir` sanitises and hash-suffixes. The check could never
  match, so the documented "never destroy a running build" guarantee was
  silently false. The project name is now persisted in the workspace state
  file and read back by `scan_workspaces`; the same bug affected `fh clean`.
  The existing test passed a predicate matching the *directory* name, which is
  why it never caught this.
- **An ignored path was re-uploaded on every run.** The client scans with no
  extra ignores while the agent scans with the template's `ignoreExtra`, so a
  file present in both trees was missing from the agent's scan, fell into the
  "not present" arm of the diff, uploaded, stayed excluded from the next scan,
  and was uploaded again — a sync that never reaches zero bytes. The agent now
  skips any path its own ignore rules exclude.
- **`fh lsp` always exited 0.** The remote language server's exit code was bound
  to a variable, the statement dropped, and `Ok(0)` returned, so an editor was
  told the server had exited cleanly even when it crashed.
- **`install.sh` installed unverified binaries, and could install the wrong
  ones.** It never checked the SHA-256 it publishes beside every release, and
  if the download failed it silently `cargo build`ed whatever was in the
  current directory and installed that as Farhand. It now verifies the
  published checksum and refuses to install without it, and the implicit
  build-whatever-is-here fallback is gone with a pointer to `cargo install`.
- **Agent bookkeeping could ship back as a build artifact.**
  `.farhand-state.json`, `.farhand-runs/`, and the new digest index live at the
  workspace root, so an output pattern broad enough to cover the root streamed
  them to the client. `resolve_artifact_paths` now excludes them.
- **`release.yml` built with an unpinned `cross` and no `--locked`.** The
  release pipeline installed `cross` from `main`, contradicting this
  repository's own `unknown-git = "deny"`, and no build passed `--locked`, so
  the committed lockfile — and the RustSec audit of it — was advisory rather
  than binding. `cross` is pinned to a tag and every build, test, and bench
  now uses `--locked`. A new CI job asserts every path dependency carries the
  workspace version, which is what lets the lockfile be trusted.
- **`specs.md` shipped a duplicated, cut-off paragraph** inside the release
  tarball, **`CONTRIBUTING.md` still described nightly fuzzing as advisory**
  with a diagnosis the changelog records as wrong and fixed, and
  **`LICENSE-MIT` / `LICENSE-APACHE` were not packaged** into any artifact.
- **`README.md`'s install table pointed at v1.7.0** artifacts while the crate
  was at 1.9.0, and **`SECURITY.md` listed `1.7.x` as supported**, so a visitor
  installed three releases of bugs and a current reporter was told their
  version was unsupported. The table now uses `releases/latest` links, which
  cannot go stale.
- **The nightly fuzz job now runs.** It had been red on every nightly run for
  weeks and was annotated as unfixable from a workflow, which was wrong on both
  counts. Three separate causes: `--build-std` compiles `compiler_builtins` with
  `crt-static`, which libFuzzer's sanitizer rejects (rustc names the fix in the
  error); `musl-tools` was missing for the C shims; and `cc-rs` was reaching
  for a musl C++ toolchain on a glibc runner, so the target triple is now pinned
  rather than inferred. It is no longer `continue-on-error` — a nightly fuzzer
  gates nothing downstream, so a failure here is information, not noise. First
  green run: ~230 million executions across the frame parser, the tar unpacker
  and wire-path handling, no crashes.
- **The e2e suite was not hermetic.** `FARHAND_HOST` and `FARHAND_TOKEN`
  deliberately outrank `.farhand.yaml`, so a developer who has them exported —
  which the README documents — had every config-resolution test silently
  exercise their own shell instead of the fixture. The client would ignore the
  test's config, try the real default port, and fail in a way that reads like a
  code bug. All 27 client spawns now go through a helper that clears those two
  variables; the eight that drive the client synchronously use a blocking
  variant of the same. The suite passes with or without them set.

### Removed
- **The Codecov upload and its badge.** No coverage service is configured for
  this repository, so the upload was tokenless, could not fail the build
  (`fail_ci_if_error: false`), and produced a badge nobody was reading — which
  is worse than no badge, because it looks like coverage is being tracked. The
  coverage job still runs: it now prints the summary into the job log and
  archives the full `lcov.info` as a 14-day artifact, so the number is
  readable and the data is downloadable. The dead `codecov/codecov-action`
  ignore was dropped from `dependabot.yml` along with it.

## [1.9.0] - 2026-09-26

### Added
- **`fh sync` / `fh why` — see exactly what crosses the wire.** A build reports
  its sync as a side effect; these make the transfer the subject.
  - `fh sync --dry-run` connects, exchanges the manifest, and reports what
    *would* move — file count, bytes, what the agent already has, and the share
    of the project that would cross the network — without sending a single
    file. `--list` additionally names every path in the transfer set.
  - `fh sync` performs the sync without running a build, warming the agent's
    workspace for the next run.
  - `fh why <path>` explains a single path: it will be uploaded, it is already
    on the agent as a content-addressed hit, or it is excluded — naming the
    `.gitignore` pattern or built-in directory responsible.
  - Both are built on the real manifest/NEED exchange, so the answers come from
    the agent's actual state rather than a local guess, and the ignore rules
    applied are the ones `scan` itself uses.
  - The daemon now treats a client that stops after NEED (a dry run) or after
    FILES (a sync with no command) as a completed session instead of logging a
    protocol error.
- **Nix packaging**: a `flake.nix` exposing `fh` and `fhd` plus a dev shell
  with `rust-analyzer`, `cargo-audit`, and `cargo-deny`, and a `default.nix` for
  non-flake consumers. Both read the version from `Cargo.toml` rather than
  carrying a copy that can go stale.
- **crates.io publishing is ready**: the crates publish under the `farhand-*`
  namespace (the name `farhand` is taken on crates.io by an unrelated project)
  while the executables remain `fh` and `fhd`. See CONTRIBUTING.md for the
  publish order — a dependency chain can only be published leaf-first.
- **Opt-in Prometheus metrics on `fhd`** (`--metrics-port`): run/slot
  gauges, queue depth, active builds per project, disk, load, and memory, plus
  `/healthz` for container probes. Hand-rolled over a `TcpListener` — this is
  one static text endpoint, and a web framework in the daemon's dependency
  tree would be a poor trade. Binds separately from the agent port, and a
  metrics port that cannot bind logs an error instead of taking the agent
  down. Run ids are deliberately not labels, so finished runs leave no stale
  series. Documented in [docs/observability.md](docs/observability.md) with
  alert rules and an importable Grafana dashboard.
- **Progress reporting while the delta is packed**: the client draws a live
  bar (`[====    ]  42% 17/40 packing delta  0.4s`) driven by real per-entry
  callbacks from the packer, not a timer. It renders only when stdout is a
  terminal, so redirected output and CI logs stay clean — verified by running
  under a pty and confirming silence without one.
- **Watch mode now honours each template's `ignoreExtra`**, so a template can
  declare paths that must not trigger a rebuild (a vendored tree, snapshot
  files), and the debounce is configurable with `--watch-debounce`
  (default 150 ms) for editors that save in bursts or on network filesystems.
  The built-in feedback-loop guards (`target/`, `node_modules/`, …) stay
  unconditional: a negated template pattern cannot un-ignore them, which would
  otherwise rebuild on our own output forever.
- **Shell completions and man pages**: `fh completions <shell>` prints a
  completion script for bash, zsh, fish, elvish, or PowerShell, and
  `fh man --dir <dir>` writes a man page per subcommand. Both are generated
  from the same clap definitions the binary uses, so they cannot describe a
  stale interface. Both need no host, config, or network.
- **`fh doctor` no longer nags on a loopback target**: a token sent to
  `127.0.0.1` never leaves the host, so there is nothing to encrypt, and the
  warning it used to print contradicted its own advice ("for anything but
  loopback"). It now reports a pass with the reasoning, and still warns for a
  real remote host. A test that had been asserting the old behaviour — its name
  promised a loopback special case the code never implemented — now pins both
  halves.
- **`fh sync` reports the transfer in the right tense**: a completed sync said
  "would cross the network", and a real sync that had nothing to do was
  indistinguishable from a dry-run plan. The wording now follows whether the
  bytes actually moved.
- **`fh doctor`** — a read-only diagnosis of the things that actually break
  remote builds: where farhand is pointed, whether a token is configured and
  whether it is stored in plaintext, whether the transport is encrypted,
  declared toolchains, and the agent's reachability, disk, queue depth, and
  load. The probe is deliberately two-phase, because a single failed request
  cannot distinguish "the daemon is not running" from "the daemon is there and
  rejected your token" — and those need completely different fixes. Nothing is
  synced and no run slot is used; the only remote traffic is the same STATUS
  request `fh top` makes. Exits 125 when something is actually broken, so it is
  usable from a script.

### Published
- **The crates are on crates.io** as `farhand-cli` and `farhand-agent` (plus the
  five library crates `farhand-protocol`, `farhand-fileset`, `farhand-config`,
  `farhand-templates`, and `farhand-workspace`). Install with
  `cargo install farhand-cli` and `cargo install farhand-agent`; the executables
  are still `fh` and `fhd`. The name `farhand` is unavailable on crates.io — an
  unrelated project owns it.

### Fixed
- **The published packages would not have built.** `farhand-templates` embedded
  the builtin templates from a repository-level `templates/builtin/` directory,
  but a crate published to crates.io contains only the files inside its own
  directory. Every in-tree build and the whole test suite passed, and
  `cargo install farhand-cli` would have failed for everyone — *after* the six
  crates below it were already published, where a version can never be
  removed. The templates now live in `crates/templates/builtin/`, with a test
  that fails if they move back out or if a duplicate appears at the old path.
- **`brew install farhand` served v1.7.0 while v1.8.0 and v1.8.1 had shipped.**
  The formula's URLs interpolate its own version, so it quietly pointed at
  1.7.0's artifacts with a version string that matched its own checksum — no
  error, just old binaries, including the Windows stack overflow that made the
  client crash before printing its version. Bumped to 1.8.1 with checksums read
  from the published `.sha256` files. A nightly job now compares the formula
  against the latest release so this cannot go unnoticed again.
- **`fh --help` described `sync` as "Clean remote project workspaces or
  caches."** The subcommand had been inserted between `Clean` and its doc
  comment, so clap attached the wrong description. Each subcommand now carries
  its own, and two tests assert none is undocumented or described as another
  command.
- **The Prometheus endpoint emitted duplicate `# HELP`/`# TYPE` pairs** for the
  load-average family, which is a scrape-time parse error rather than a
  cosmetic one. An exposition builder now declares each family exactly once,
  and label values escape quotes, backslashes, and newlines — a project name
  comes from a directory name, and Unix allows newlines in those.

## [1.8.1] - 2026-09-26

### Changed
- **Dependency maintenance, reviewed rather than bulk-merged.** Every Dependabot
  proposal was checked instead of merged on a green matrix:
  - `rcgen` 0.13 → 0.14 required migrating the mutual-TLS client PKI to the
    new explicit `Issuer` model (`signed_by(key, &Issuer)` replaces
    `signed_by(key, &ca_cert, &ca_key)`). The wire format and the generated
    PKI are unchanged, and the mTLS e2e test passes unmodified.
  - `webpki-roots` 0.26 → 1.0, which also collapsed a duplicate in the lockfile
    — `deny.toml`'s skip entry for it is now obsolete and removed, leaving
    cargo-deny green with one fewer exemption.
  - `zstd` 0.13 → 0.14, `criterion` 0.5 → 0.8, `clap` 4.6.6 → 4.6.7.
  - `actions/checkout` v4 → v7 and `actions/upload-artifact` v4 → v7, each
    pinned SHA verified against its tag before merging.
- **Dependabot is now scoped to what CI actually exercises.** Three action
  bumps are ignored with written reasons: `dtolnay/rust-toolchain` (Dependabot
  collapses the three per-toolchain pins — stable, the 1.88 MSRV job, nightly —
  into the `stable` commit while the comments keep claiming otherwise, which
  would have quietly turned the MSRV gate into a duplicate of the main build);
  `codecov/codecov-action` (no token, so uploads are tokenless, and v5+ changed
  that path — with `fail_ci_if_error: false` a silent failure would freeze the
  badge while CI stayed green); and `softprops/action-gh-release` (runs only on
  a tag, so no PR check ever exercises it — it will be validated during a real
  release).

### Fixed
- **A concurrency test no longer depends on a 100 ms sleep.** The
  project-locking test waited a fixed interval for the first client to take the
  workspace lock, which on Windows was sometimes not enough — the test then read
  `Need` where it expected `Queued`. It now polls the running agent's actual
  lock (the manager is `Clone` over an `Arc`, so the test can hold a handle), so
  no timing assumption remains.

## [1.8.0] - 2026-09-25

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
- Fuzz-lite property tests: deterministic pseudo-random byte streams and
  bit-flipped archives run through the frame reader, wire-path parser, and
  tar unpacker — crash-safety and zip-slip invariants verified in CI on all
  platforms.

### Fixed
- **`fh` could not start on Windows at all.** Windows gives the main thread a
  1 MiB stack (Linux gives 8 MiB), and the client needed more than that before
  doing any work, so it aborted with `STATUS_STACK_OVERFLOW`
  (`0xC00000FD`) — even `fh --version` crashed. The work now runs on a thread
  with an explicit 16 MiB stack, the build/upload/download futures are boxed so
  they do not sit in one enormous frame chain, and CI runs both binaries under
  `ulimit -s 1024` so this cannot come back unnoticed.
- **Release artifacts build on every target again.** The Linux CoW ioctl
  constant was typed `c_ulong`, but musl's `ioctl` takes its request as
  `c_int` — so both musl artifacts (x86_64 and aarch64) failed to compile
  while every glibc check stayed green. The Windows disk probe was also
  missing the allowance the new crate-level `deny(unsafe_code)` requires.
  Neither was visible to the previous CI matrix, which covered only glibc,
  macOS, and Windows; CI now type-checks the musl release target on every PR.
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
  allocation, zip-slip containment, traversal rejection). Committed seed corpora
  capture known attack vectors (traversal tar, absolute-path tar, lying frame
  header). The per-PR smoke job was removed and the nightly deep run made
  advisory: libFuzzer needs an instrumented std and a non-static CRT, and the
  hosted runner links the CRT statically. Fuzzing remains verified locally
  (millions of executions per target) and, on every PR, through the fuzz-lite
  property tests in the 3-OS test matrix.
- **Unsafe audit**: all 11 `unsafe` blocks (FFI only) now carry `// SAFETY:`
  contracts, and CI enforces `clippy::undocumented_unsafe_blocks` so new
  unsafe code cannot land undocumented. The undocumented Linux `FICLONE`
  magic number became a documented constant, `statvfs` moved from `mem::zeroed`
  to `MaybeUninit` (initialized only on syscall success), and agent identity
  now prefers `HOSTNAME`/`COMPUTERNAME` over `gethostname(2)` — an operator
  can relabel a pool agent — with the libc call as fallback. Miri is
  intentionally not in CI: it cannot follow FFI, so it would cover only the
  non-unsafe majority of the code.
- **Cancellation is now proven, not assumed.** The old disconnect e2e test
  started a command, dropped the connection, slept 500ms, and asserted
  nothing. It is replaced by a test that runs a two-level process tree
  remotely, verifies both PIDs are alive, drops the client, and then requires
  both the direct child *and* the grandchild to die (SIGTERM to the process
  group, SIGKILL after 3s), plus a follow-up run that proves the cancelled
  run released its concurrency permit. The test was validated by mutating the
  daemon to kill only the direct child — it fails with the grandchild
  surviving, which is exactly the orphan-process bug the invariant forbids.
- **`fh exec` end-to-end coverage**: the ad-hoc command path (workspace
  sync, streamed stdout, mirrored remote exit code) is now exercised through
  the real binary, not just the protocol layer.
- **Architecture: the two monoliths are decomposed.** `fhd/src/lib.rs` was
  2,200+ lines containing the accept loop, a 950-line connection handler,
  command construction, PTY handling, port forwarding, and state
  management; it is now `lib.rs` (accept loop + run pipeline) plus `active`,
  `exec`, `stream`, and `session` modules. `fh/src/main.rs` was 1,883 lines
  mixing the entire CLI surface with the runtime; the clap definitions moved
  to `cli.rs`, `run_build`'s 18 positional arguments became a `RunParams`
  struct, the duplicated HELLO handshake became one `perform_handshake`, and
  watch mode became its own `run_watch`. All code moved verbatim — no logic
  changes — and the per-suite test counts were verified after every step,
  which caught an orphaned `#[cfg(windows)]` that had silently skipped all
  nine `fhd` unit tests on Linux while the build stayed green.
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

[Unreleased]: https://github.com/Rayrsn/farhand/compare/v1.9.0...HEAD
[1.9.0]: https://github.com/Rayrsn/farhand/compare/v1.8.1...v1.9.0
[1.8.1]: https://github.com/Rayrsn/farhand/compare/v1.8.0...v1.8.1
[1.8.0]: https://github.com/Rayrsn/farhand/compare/v1.7.0...v1.8.0
[1.7.0]: https://github.com/Rayrsn/farhand/compare/v1.6.0...v1.7.0
[1.6.0]: https://github.com/Rayrsn/farhand/compare/v1.5.0...v1.6.0
[1.5.0]: https://github.com/Rayrsn/farhand/compare/v1.4.0...v1.5.0
[1.4.0]: https://github.com/Rayrsn/farhand/compare/v1.3.0...v1.4.0
[1.3.0]: https://github.com/Rayrsn/farhand/compare/v1.2.0...v1.3.0
[1.2.0]: https://github.com/Rayrsn/farhand/compare/v1.1.0...v1.2.0
[1.1.0]: https://github.com/Rayrsn/farhand/compare/v1.0.0...v1.1.0
[1.0.0]: https://github.com/Rayrsn/farhand/releases/tag/v1.0.0
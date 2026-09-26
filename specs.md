# farhand — Design & Implementation Spec (Rust Edition)

## 1. Purpose

A CLI tool that offloads building/testing a project from a weak local
laptop to a stronger remote machine over the network, with live log
streaming and automatic retrieval of build artifacts.

Example usage:

```bash
fh --host 192.168.1.50:9876 --token mysecret -- npm run test:ci
fh --host tunnel.example.com:443 --token mysecret -- cargo build --release
```

The remote machine only needs the `fhd` binary running
and a port reachable — either directly on the LAN, or tunneled in from
the internet via `ssh -L`/`-R` or `cloudflared tunnel`. The remote machine only needs the `fhd` binary running
and a port reachable — either directly on the LAN, over a private network
(Tailscale/WireGuard), or tunneled in from the internet via `ssh -L` or
`cloudflared tunnel`. TLS is built in (`rustls`): `fhd --tls-auto` can
generate self-signed certificates for TOFU fingerprint pinning, or operators
can supply a CA and client certificates for mutual TLS. Where a tunnel already
provides transport security, raw TCP keeps the setup one-flag simple. Either
way, the binaries remain a single, dependency-free static binary that behaves
identically on Windows, macOS, and Linux — no reliance on system
`ssh`/`rsync`/`tar` binaries.

## 2. High-Level Architecture

```
┌─────────────────┐  TCP socket (raw, TLS via rustls,  ┌──────────────────────┐
│       fh         │  or tunneled via ssh -L /          │         fhd          │
│  (client, laptop) │  cloudflared)                     │  (agent, beefy box)  │
│                   │                                   │                      │
└─────────────────┘ ─────────────────────────────────► └──────────────────────┘
        │                                                        │
        │ 1. scan local project dir, hash files                 │
        │ 2. send manifest ──────────────────────────────────►  │ compare vs
        │                                                        │ workspace cache
        │ 3. ◄────────────────────────── "send me these paths"  │
        │ 4. send only changed files (tar+gzip) ──────────────► │ extract into
        │                                                        │ persistent
        │                                                        │ workspace dir
        │ 5. send RUN {argv} ──────────────────────────────────►│ exec command
        │ 6. ◄──────────────────────── live stdout/stderr lines │ stream output
        │ 7. ◄─────────────────────────────────── exit code     │
        │ 8. ◄────────────────── artifact tar (if exit==0) ─────│ tar output paths
        │ 9. extract artifacts locally                          │
```

Key property: the **agent keeps a persistent workspace per project**
(keyed by a project name/hash), so `node_modules`, `target/`,
`.venv`, etc. accumulate naturally on the remote side across runs
instead of being re-uploaded or reinstalled every time. Only the
delta of source files is transferred each invocation.

## 3. Components (Rust Crates)

The project is structured as a Cargo workspace with distinct crates:

### 3.1 `crates/fh` (client binary)

Responsibilities:
- Parse CLI flags (`clap`) + optional config file (`crates/config`).
- Walk the local project directory respecting ignore rules, and hash
  every file (SHA-256 content hash, plus size/mtime as a fast pre-check).
- Talk to the agent over async TCP (`tokio`): handshake, manifest exchange,
  delta upload, run request, live log rendering, artifact download.
- Exit with the same exit code the remote command produced.

### 3.2 `crates/fhd` (server / agent daemon binary)

Responsibilities:
- Listen on an async TCP port via `tokio`.
- Authenticate incoming connections via shared token.
- Maintain a workspace directory per project under
  `~/.farhand/workspaces/<project-name>-<hash>/`.
- Compare an incoming manifest against its own workspace scan; report
  back which files are missing/changed and which files should be
  deleted (present remotely but no longer present locally).
- Receive and extract the delta tar.
- Execute the requested command inside the workspace using the host
  shell, streaming combined stdout/stderr back to the client
  line-by-line (or in small chunks) in real-time as it's produced.
- On success, resolve output paths (explicit `--output` flags from the
  client, or preset/template defaults) to files that actually exist,
  tar+gzip them, and send them back.
- Send a final result message with the exit code.

### 3.3 `crates/protocol`

Shared wire format and strongly-typed message definitions (`serde`)
used by both binaries (see §5). Zero dependencies on filesystem or
workspace logic.

### 3.4 `crates/fileset`

Directory walking, ignore-pattern matching, SHA-256 hashing, tar/gzip
packing and unpacking using pure Rust (`tar`, `flate2`, `sha2`). Used by
both client (packing deltas) and agent (packing artifacts, scanning
workspace).

### 3.5 `crates/workspace`

Agent-side project workspace resolution, locking, manifest diffing, and
Section 5.1 deletion safety verification.

### 3.6 `crates/templates`

YAML template loader, embedded defaults (`include_str!`), and match
evaluator.

### 3.7 `crates/config`

`.farhand.yaml` parser, environment variable expansion, and CLI flag
merging.

## 4. Default Ignore Rules (fileset walking)

Applied when the client scans the local project directory to build
its manifest, so heavy/regeneratable directories are never uploaded:

```
.git/
node_modules/
target/
dist/
build/
.next/
out/
__pycache__/
.venv/
venv/
vendor/
.DS_Store
```

In addition, the walker parses a `.gitignore` in the project
root (glob/prefix matching) and applies those patterns too. A
project-local `.farhand-ignore` file (same syntax) allows
project-specific overrides/additions.

## 5. Wire Protocol

Simple length-prefixed binary framing over a single TCP connection,
one exchange per invocation (connection closes when the run
completes).

**Frame format:**

```
[1 byte  MsgType]
[4 bytes uint32 BE  PayloadLength]
[PayloadLength bytes  Payload]
```

Control messages (`Payload` is JSON via `serde_json`):
`HELLO`, `HELLO_ACK`, `MANIFEST`, `NEED`, `RUN`, `LOG`, `RESULT`, `ERROR`,
`PUT_TEMPLATE`, `QUEUED`, `STATUS`, `STATUS_RESP`, `HISTORY`, `HISTORY_RESP`.

Binary messages (`Payload` is raw tar.gz bytes): `FILES`, `ARTIFACTS`.

**Message sequence:**

1. **Client → Agent: `HELLO`**
   `{ "token": string, "project": string, "protocolVersion": int }`
2. **Agent → Client: `HELLO_ACK`**
   `{ "ok": bool, "error": string? }` — closes connection if token
   invalid or version mismatch.
3. **Client → Agent: `MANIFEST`**
   `{ "files": [ { "path": string, "hash": string, "size": int64, "mode": uint32 } ] }`
4. **Agent → Client: `NEED`**
   `{ "want": [string paths], "deleteExtraneous": [string paths] }`
   — agent scans its cached workspace, diffs against the manifest, and
   asks for exactly what it's missing; also flags workspace files no
   longer present locally so the client can confirm deletion (see
   §5.1 for the safety rule around deletions).
5. **Client → Agent: `FILES`**
   Raw tar.gz payload containing only the requested paths.
6. **Client → Agent: `RUN`**
   `{ "argv": [string], "outputs": [string]?, "cwd": string?, "template": string?, "noCache": bool? }`
7. **Agent → Client: `LOG`** (repeated, streamed live)
   `{ "stream": "stdout"|"stderr", "data": string }`
8. **Agent → Client: `RESULT`**
   `{ "exitCode": int, "error": string? }`
9. **Agent → Client: `ARTIFACTS`** (only if `exitCode == 0` and any
   output paths resolved to existing files)
   Raw tar.gz payload of the resolved output paths, relative to the
   workspace root.
10. Connection closes.

### 5.1 Deletion safety rule

Never silently delete files in the persistent workspace based on
`.gitignore` overlap accidents. Only delete a workspace file if it:
(a) is inside the tracked source tree (not inside a directory the
client explicitly excluded, like `node_modules/`), and (b) is absent
from the client's manifest. This distinction matters because excluded
directories (dependencies, build output) are *expected* to exist
remotely without ever appearing in the manifest — only paths that
*would* have been included had they existed locally are candidates
for deletion.

## 6. CLI

### 6.1 Client: `fh`

```bash
fh [flags] -- <command> [args...]

Flags:
  --host <string>        agent address, host:port (required, or from config)
  --token <string>       shared auth token (required, or from config/env FARHAND_TOKEN)
  --name <string>        project name / workspace key (default: local dir basename)
  --dir <string>         local project directory to sync (default: cwd)
  --output <string>      explicit path(s) to fetch back after a successful run
                          (repeatable; overrides preset auto-detection)
  --out-dir <string>     local directory to extract artifacts into
                          (default: ./farhand-out)
  --config <string>      path to config file (default: ./.farhand.yaml)
  --insecure-skip-token  allow connecting to an agent with no token configured
  --verbose              print sync stats, timing, etc.
  --template <string>    force specific template by name
  --agent-tag <string>   pin to agent with tag (multi-agent mode)
  --no-cache             bypass lockfile dependency caching hooks
```

Exit code mirrors the remote command's exit code. Network/protocol
errors use reserved exit code `125`.

### 6.2 Agent: `fhd`

```bash
fhd [flags]

Flags:
  --listen <string>              address to listen on (default: "0.0.0.0:9876")
  --token <string>               required shared token (or env FARHAND_TOKEN)
  --workdir <string>             root for persistent workspaces (default: ~/.farhand/workspaces)
  --shell <string>               shell to invoke commands with (default: /bin/sh -c on unix, cmd /C on windows)
  --max-concurrent-runs <usize>  max parallel runs across projects (default: num_cpus)
  --log-level <string>           debug, info, warn, error
  --log-format <string>          text, json
```

### 6.3 Config file (`.farhand.yaml`, optional, project-local)

```yaml
host: 192.168.1.50:9876
token: ${FARHAND_TOKEN}   # env var interpolation
name: my-app
outputs:
  - dist/
  - stats.json
```

## 7. Security Model

- Shared bearer token authentication on every connection.
- Transport security assumed to be handled by the tunnel (`ssh -L`, `cloudflared tunnel`, WireGuard, or private LAN).
- Zip-Slip / path traversal protection: Reject any archive entry resolving outside the target directory.
- Process group isolation: Subprocesses run in dedicated process groups (`setpgid`) and are terminated with `SIGTERM`/`SIGKILL` on client disconnection.

## 8. Repository Layout (Cargo Workspace)

```
farhand/
├── Cargo.toml                # Workspace manifest
├── specs.md                  # Specification
├── AGENTS.md                 # Agent guidelines
├── plan/                     # Multi-stage implementation roadmap
│   ├── README.md
│   └── stage-*.md
├── crates/
│   ├── fh/                   # Client binary
│   │   ├── Cargo.toml
│   │   └── src/main.rs
│   ├── fhd/                  # Agent daemon binary
│   │   ├── Cargo.toml
│   │   └── src/main.rs
│   ├── protocol/             # Wire codec & message structs
│   │   ├── Cargo.toml
│   │   └── src/lib.rs
│   ├── fileset/              # Scan, hash, tar/gzip pack/unpack
│   │   ├── Cargo.toml
│   │   └── src/lib.rs
│   ├── workspace/            # Persistent workspace management & diffing
│   │   ├── Cargo.toml
│   │   └── src/lib.rs
│   ├── templates/            # YAML template engine & embedded defaults
│   │   ├── Cargo.toml
│   │   └── src/lib.rs
│   └── config/               # .farhand.yaml parsing & env var interpolation
│       ├── Cargo.toml
│       └── src/lib.rs
├── templates/
│   └── builtin/
│       ├── npm.yaml
│       ├── rust.yaml
│       ├── go.yaml
│       ├── python.yaml
│       ├── maven.yaml
│       └── gradle.yaml
└── testdata/
    ├── sample-npm-project/
    └── sample-rust-project/
```

## 9. Implementation Phases (Rust)

### Phase 0 — Scaffolding
- Root `Cargo.toml` workspace definition with crates: `fh`, `fhd`, `protocol`.
- `crates/protocol`: length-prefixed frame encoder/decoder using `tokio::io::AsyncReadExt` and `AsyncWriteExt`, strongly-typed message structs with `serde`.
- Round-trip framing unit tests with `tokio_test` / memory buffers.
- **Acceptance:** `cargo build --workspace` and `cargo test --workspace` pass.

### Phase 1 — Fileset: scan, hash, ignore
- `crates/fileset`:
  - Scanning: Directory tree traversal with ignore rules (`.gitignore`, `.farhand-ignore`, built-in defaults).
  - Hashing: `sha2::Sha256` content digest for each regular file.
  - Archiving: `tar` and `flate2` for in-memory `tar.gz` packing and unpacking with strict Zip-Slip path sanitization.
- **Acceptance:** Unit tests covering nested fixtures, ignore exclusions, and Zip-Slip attack rejection.

### Phase 2 — Minimal end-to-end (no delta, no presets)
- `crates/fhd`: Async TCP listener on Tokio, `HELLO`/`HELLO_ACK` token validation, receive full `FILES` tar.gz, extract to temporary directory, spawn command with `tokio::process::Command`, stream live `LOG` frames, send `RESULT`.
- `crates/fh`: Dial agent, handshake, pack full directory into `FILES`, send `RUN`, stream `LOG` to stdout/stderr, return remote exit code.
- **Acceptance:** `cargo run -p farhand-cli -- --host 127.0.0.1:9876 --token secret -- echo "pipe works"` streams output live and exits cleanly.

### Phase 3 — Persistent workspaces + delta sync
- `crates/workspace`: Deterministic workspace directory hashing (`~/.farhand/workspaces/<project>-<hash>/`).
- Agent diffing: Scan existing workspace, diff against client `MANIFEST`, generate `NEED` (`want` + `deleteExtraneous`).
- Section 5.1 Deletion Safety Rule: Never delete un-tracked directories (`node_modules/`, `target/`).
- Client: Send `MANIFEST`, only pack files in `want`.
- **Acceptance:** Subsequent run with unchanged files uploads 0 source bytes; deleted files are cleaned up; dependency directories survive.

### Phase 4 — Artifact retrieval + basic presets
- Hardcoded preset table (npm, rust, go, python).
- Agent resolves output candidates post-build (`exitCode == 0`), tars existing paths, sends `ARTIFACTS`.
- Client CLI `--output` and `--out-dir` flags; unpacks `ARTIFACTS` locally.
- **Acceptance:** Running against sample fixtures fetches `dist/` or `target/release/` automatically.

### Phase 5 — Config file + polish
- `crates/config`: Parse `.farhand.yaml` via `serde_yaml`, env var expansion (`${VAR}`), merge precedence: Flags > Env > Config > Defaults.
- Reserved exit code `125` for infrastructure/network errors.
- `--verbose` execution statistics (scan time, delta bytes, build time, artifact bytes).
- **Acceptance:** Fresh repo with `.farhand.yaml` builds with `fh -- <cmd>`.

### Phase 6 — Cross-platform hardening
- Invariant forward-slash (`/`) wire paths across Windows, macOS, and Linux.
- Shell abstraction: Unix `/bin/sh -c` vs Windows `cmd.exe /C`.
- Cross-compilation validation for `x86_64-unknown-linux-gnu`, `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-gnu`, `x86_64-apple-darwin`, `aarch64-apple-darwin`, and `x86_64-pc-windows-msvc`.

### Phase 7 — Pluggable template system
- `crates/templates`: YAML template definitions, `include_str!` for embedded defaults, resolution order: project (`.farhand/templates/*.yaml`) > user (`~/.farhand/templates/*.yaml`) > built-in.
- Match evaluation (`anyFile`, `allFiles`), monorepo output union.
- `PUT_TEMPLATE` protocol frame, `fh templates list|show|init` subcommands.

### Phase 8 — Concurrency & per-agent job queue
- Agent per-project `tokio::sync::Mutex` and global concurrency semaphore (`--max-concurrent-runs`).
- `QUEUED` protocol frame for waiting clients.
- Subprocess group termination (`libc::killpg` or `nix`) on client socket disconnection.

### Phase 9 — Multi-agent support (scale out)
- Multi-agent config in `.farhand.yaml` (`agents:` list with tags).
- Parallel `STATUS` querying (`MsgStatus`, `MsgStatusResp`).
- Selection algorithms: `--agent-tag` pinning or automatic least-busy dispatch.

### Phase 10 — Build/dependency caching hooks
- Lockfile SHA-256 tracking in `<workspace>/.farhand-state.json`.
- Automatic pre-run execution of `hints.installCommand` when lockfiles change.
- Distinct streamed log headers for the install phase; `--no-cache` override.

### Phase 11 — Observability & run history
- Persistent JSON run logs in `<workspace>/.farhand-runs/<timestamp>.json`.
- `fh history` subcommand querying agent history (`MsgHistory`, `MsgHistoryResp`).
- Structured logging using `tracing` and `tracing-subscriber` (with `--log-level` and `--log-format json`).

### Phase 12 — Packaging & distribution
- Cross-platform release builds via `cargo-dist` or GitHub Actions matrix.
- `install.sh` script, Homebrew formula, systemd unit, and macOS launchd plist.
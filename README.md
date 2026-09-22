<p align="center">
  <img src="assets/logo.png" alt="Farhand Logo" width="220" style="border-radius: 24px;" />
</p>

<h1 align="center">Farhand (fh)</h1>

<p align="center">
  <strong>Zero-dependency remote build and test offloader.</strong>
</p>

<p align="center">
  <i>
    Farhand or fh (pronounced <strong>FAAAAAAAH</strong>)
  </i>
  <br/><br/>
  <a href="https://cdn.jsdelivr.net/gh/Rayrsn/farhand@main/assets/pronunciation.mp3" target="_blank" title="Click to listen to pronunciation audio">
    <img src="assets/pronunciation_player.png" alt="Listen to Pronunciation (FAAAAAAAH)" width="380" />
  </a>
  <br/>
  <small>
    <a href="https://cdn.jsdelivr.net/gh/Rayrsn/farhand@main/assets/pronunciation.mp3" target="_blank">🔊 Click to listen to pronunciation (.mp3)</a>
  </small>
</p>

<p align="center">
  <a href="https://github.com/Rayrsn/farhand/actions"><img src="https://img.shields.io/badge/build-passing-brightgreen?style=flat-square" alt="Build Status" /></a>
  <a href="https://github.com/Rayrsn/farhand/releases/tag/v1.7.0"><img src="https://img.shields.io/badge/version-1.7.0-orange?style=flat-square" alt="Version" /></a>
  <a href="https://raw.githubusercontent.com/Rayrsn/farhand/main/LICENSE"><img src="https://img.shields.io/badge/license-MIT%2FApache--2.0-blue?style=flat-square" alt="License" /></a>
  <img src="https://img.shields.io/badge/dependencies-zero-success?style=flat-square" alt="Zero Dependencies" />
  <img src="https://img.shields.io/badge/platform-linux%20%7C%20macos%20%7C%20windows-lightgrey?style=flat-square" alt="Platforms" />
</p>

---

## What is Farhand?

Modern software projects have heavy compilation, bundling, and testing pipelines. Running `tsc -p .`, `cargo build --release`, `vitest run`, or `docker build` on a thin laptop or MacBook Air drains battery, spins loud fans, and throttles your system.

**Farhand** (`fh` + `fhd`) allows you to keep editing code locally in your favorite editor (VS Code, Neovim, Zed) while offloading heavy compilation to a powerful remote machine (such as an Apple Silicon Mac Mini, a Linux workstation, or an internal build server). 

Logs stream directly into your terminal in real time, and build artifacts (like `./dist` or `./target/release`) are automatically synced back to your local project directory.

---

## Why Farhand?

| Feature | Farhand (`fh`) | `ssh` + `rsync` scripts | Remote Desktop / SSH VSCode |
| :--- | :---: | :---: | :---: |
| **Zero Runtime Dependencies** | **Yes** (pure static Rust) | No (requires `rsync`, `ssh`, `tar`) | No (heavy daemon) |
| **Zstandard (zstd) Wire Compression** | **Yes** (negotiated, 3–5x faster) | No (gzip or none) | N/A |
| **Global Content-Addressable Storage (CAS)** | **Yes** (zero-copy CoW hydration) | No | No |
| **Persistent Dependency Cache** | **Yes** (`node_modules` stays remote) | Often wipes or conflicts | Local to remote box |
| **Multi-Branch APFS CoW Forking** | **Yes** (< 100ms, 0-byte duplicate) | No (duplicates entire folder) | No |
| **Delta Source Sync** | **Yes** (SHA-256 manifests over TCP) | Yes (rsync delta) | N/A (entire edit remote) |
| **Clean Process Cancellation** | **Yes** (kills remote process tree) | No (orphans compiler processes) | Yes |
| **Offline Multi-Agent Failover**| **Yes** (automatic load-balancing) | No | No |
| **Works with Any Local Editor** | **Yes** (pure CLI wrapper) | Yes | No |

---

## Core Features

- ⚡ **Zero External Dependencies**: Pure Rust binaries. Does not invoke or depend on system `ssh`, `rsync`, `tar`, or `gzip`.
- 🚀 **High-Speed Zstandard (zstd) Wire Compression**: Automatic handshake negotiation chooses `zstd` (level 3) for delta and artifact transfers, delivering 3–5× faster compression throughput than gzip with minimal CPU overhead.
- 🗄️ **Global Content-Addressable Storage (CAS)**: Files with matching SHA-256 hashes are deduplicated globally on the agent host across all branches and projects, hydrated instantly via zero-copy CoW reflinks (`clonefile` on macOS / `FICLONE` ioctl on Linux). Zero-byte uploads for known files!
- 📁 **Persistent Workspace Cache**: Remote dependencies (`node_modules/`, `target/`, `.venv/`) remain on the agent host across runs. Only changed source files are transferred.
- 🍏 **Instant APFS Copy-on-Write (CoW) Forking**: When working across different branches on shared hosts, new branch workspaces are cloned from canonical seeds (`main`/`master`) in **< 100ms using 0 additional disk blocks**.
- 🧹 **Automated Two-Tier LRU & Emergency GC**: Daemon automatically soft-prunes intermediate caches, performs pre-flight emergency GC when disk space is tight (`--min-disk-gb`), and evicts stale branch workspaces.
- 👁️ **Continuous Watch Mode (`fh watch`)**: Automatically debounces local file changes, syncs source deltas, and re-triggers remote builds with zero manual intervention.
- 🖥️ **Interactive Shell & Ad-Hoc Exec (`fh shell`, `fh exec`)**: Drop into an interactive remote PTY shell inside your project workspace or run diagnostic commands without triggering hooks.
- 🔀 **Branch-Aware Project Addressing**: Automatically detects git branches and scopes workspaces as `<repo>__<branch>` so multiple developers never collide.
- 🛡️ **Section 5.1 Deletion Safety**: Strictly protects remote dependencies and build outputs from being deleted during manifest synchronization.
- 🛑 **Process Group Isolation**: Spawns compilation inside isolated process groups (`setpgid`). If you `Ctrl+C` locally, the entire remote compiler hierarchy is gracefully terminated.
- 🌐 **Multi-Agent Pool & Tag Routing**: Automatically discovers, health-checks, and load-balances jobs across a cluster of build agents.
- 🔒 **Native Zero-Config TLS & Mutual TLS (mTLS)**: Pure-Rust, memory-safe TLS via `rustls` (zero OpenSSL / C library dependencies). Supports automatic self-signed cert generation (`fhd --tls-auto`), SHA-256 fingerprint verification (`fh --tls-fingerprint <sha256>`), CA verification (`--tls-ca`), and mutual TLS client certificates (`--tls-cert`, `--tls-key`).
- 🛠️ **Declarative Toolchain Manager Hooks**: Declare language versions per project in `.farhand.yaml` or via CLI (`-T rust=nightly`). Farhand automatically configures `RUSTUP_TOOLCHAIN`, `PYENV_VERSION`, `NODE_VERSION`, and wraps remote invocations with `nvm`, `fnm`, `pyenv`, or `goenv`.
- 📊 **Run Observability**: Query execution history, exit codes, synced bytes, and duration using `fh history` and host status via `fh status`.

---

## The Two Binaries

1. **`fh` (Client)**: Scans the local project directory, hashes files, uploads deltas, requests remote command execution, streams live logs, and retrieves generated artifacts.
2. **`fhd` (Daemon)**: Listens on TCP (port `9876`), authenticates connections via token, maintains per-project workspaces, unpacks deltas, runs commands in process groups, streams `stdout`/`stderr`, and returns artifacts.

---

## Quickstart

### 1. Installation

#### ⚡ One-Liner Install

**Linux & macOS** (Terminal):
```bash
curl -fsSL https://raw.githubusercontent.com/Rayrsn/farhand/main/scripts/install.sh | bash
```

**Windows** (PowerShell — *works whether MSVC / Visual Studio is installed or not*):
```powershell
irm https://raw.githubusercontent.com/Rayrsn/farhand/main/scripts/install.ps1 | iex
```

**Windows** (Command Prompt / `cmd.exe`):
```cmd
powershell -ExecutionPolicy Bypass -Command "irm https://raw.githubusercontent.com/Rayrsn/farhand/main/scripts/install.ps1 | iex"
```

> **Note for Windows Users**: Windows binaries are compiled with static C-runtime linking (`+crt-static`). They are 100% self-contained and run on any clean Windows machine out of the box without requiring Visual Studio, MSVC build tools, or the Microsoft Visual C++ Redistributable.

---

#### 📦 Pre-Built Release Packages (v1.7.0)

Pre-compiled static release packages and checksums are available on the [**Farhand v1.7.0 Release**](https://github.com/Rayrsn/farhand/releases/tag/v1.7.0):

| Platform | Architecture | Package Archive |
| :--- | :--- | :--- |
| **Linux** | x86_64 (64-bit) | [`farhand-v1.7.0-x86_64-unknown-linux-musl.tar.gz`](https://github.com/Rayrsn/farhand/releases/download/v1.7.0/farhand-v1.7.0-x86_64-unknown-linux-musl.tar.gz) |
| **Linux** | aarch64 (ARM64) | [`farhand-v1.7.0-aarch64-unknown-linux-musl.tar.gz`](https://github.com/Rayrsn/farhand/releases/download/v1.7.0/farhand-v1.7.0-aarch64-unknown-linux-musl.tar.gz) |
| **macOS** | Apple Silicon (M1/M2/M3/M4) | [`farhand-v1.7.0-aarch64-apple-darwin.tar.gz`](https://github.com/Rayrsn/farhand/releases/download/v1.7.0/farhand-v1.7.0-aarch64-apple-darwin.tar.gz) |
| **macOS** | Intel x86_64 | [`farhand-v1.7.0-x86_64-apple-darwin.tar.gz`](https://github.com/Rayrsn/farhand/releases/download/v1.7.0/farhand-v1.7.0-x86_64-apple-darwin.tar.gz) |
| **Windows** | x86_64 (Standalone Static) | [`farhand-v1.7.0-x86_64-pc-windows-msvc.zip`](https://github.com/Rayrsn/farhand/releases/download/v1.7.0/farhand-v1.7.0-x86_64-pc-windows-msvc.zip) |

---

#### 🍺 Via Homebrew (macOS)
```bash
brew tap Rayrsn/farhand https://github.com/Rayrsn/farhand.git
brew install farhand
```

#### 🦀 Via Cargo (From Source)
```bash
cargo install --git https://github.com/Rayrsn/farhand.git fh fhd
```

---

### 2. Start the Daemon (`fhd`)

On your remote build machine or Mac Mini:

```bash
# Generate a secret token
export FARHAND_TOKEN="super-secret-token"

# Run the daemon
fhd --listen 0.0.0.0:9876 --token "${FARHAND_TOKEN}" --workdir /var/farhand/workspaces
```

*(For production background services on macOS or Linux, see the [Apple Silicon Mac Mini Setup Guide](docs/mac-build-server-setup.md) or [systemd service units](dist/services/fhd.service)).*

---

### 3. Run Builds Remotely (`fh`)

In your local project directory, initialize Farhand configuration automatically:

```bash
# Auto-detect project type and generate .farhand.yaml
fh init

# Or optionally generate customizable template definitions (.farhand/templates/<name>.yaml)
fh init --with-template
```

This generates a `.farhand.yaml` tailored to your project:

```yaml
# .farhand.yaml
host: "192.168.254.68:9876"  # Remote agent address or Tailscale name
token: "${FARHAND_TOKEN}"

# Unpack artifacts directly into the project directory (e.g. ./dist)
outDir: "."

# Outputs to pull back from the agent upon success
outputs:
  - "dist"
```

Now execute any command remotely by prefixing it with `fh`:

```bash
# Offload TypeScript compilation
fh npm run build

# Run unit tests on the remote machine
fh npm test

# Compile Rust binaries
fh cargo build --release

# Continuous watch mode: sync and rebuild on local file saves
fh watch cargo check

# Interactive remote workspace shell (allocated PTY inside remote repo)
fh shell

# Ad-hoc command execution (bypasses dependency hooks and artifact downloads)
fh exec -- git status

# Override or suppress artifact downloads on the fly
fh -o target/release/my-bin -- cargo build --release
fh --no-output -- cargo test

# Run with secrets from Infisical (env vars forwarded automatically)
infisical run -- fh npm run build

# Or disable ambient env forwarding / pass explicit variables
fh --no-env -e DATABASE_URL=postgres://remote/app -- npm run build

# Zero-config TLS with SHA-256 fingerprint verification
fh --tls --tls-fingerprint "7f9a8b1c2d3e4f50..." -- cargo check

# Select language toolchain version on the fly
fh -T rust=nightly -T node=22 -- npm run build

# Inspect execution history and agent status (including available remote disk space)
fh history

# Live terminal resource dashboard & host telemetry
fh top                  # Interactive live TUI (CPU, RAM, Disk, Active Builds)
fh top --once           # Print snapshot and exit
fh agent info           # Formatted host specs, cores, load averages, memory
fh agent info --json    # Machine-readable JSON telemetry

# Remote Language Server Protocol (LSP) offloading (rust-analyzer, pyright, gopls, etc.)
fh lsp -- rust-analyzer

# Free remote disk space for the current branch
fh clean
```

---

## Daily Developer Workflow

```bash
# 1. Start working on a new feature branch locally
git checkout -b feat/payments

# 2. Trigger build remotely
# Farhand automatically APFS-clones the seed workspace from main in < 100ms (0 bytes duplicate storage)
fh npm run build

# 3. Modify source code locally
nano src/index.ts

# 4. Re-run build
# Farhand only uploads the single changed file (0-byte delta sync for everything else)
fh npm run build

# 5. Finished with the branch? Free remote workspace space
fh clean
```

---

## Detailed Documentation

Deep-dive guides covering architecture, server setup, and configuration:

- 📖 **[Internal Architecture & Wire Protocol](docs/architecture.md)** — Binary framing specs, frame layouts, state machines, and Section 5.1 deletion safety.
- 🍏 **[Apple Silicon Mac Mini Setup Guide](docs/mac-build-server-setup.md)** — Production step-by-step guide for turning a Mac Mini into a multi-developer build server (`launchd`, firewalls, network access).
- 💾 **[Storage Optimization & Caching Guide](docs/storage-and-caching.md)** — Deep dive into APFS Copy-on-Write cloning, LRU garbage collection, and shared toolchain caches (`sccache`).
- ⚙️ **[Configuration Guide (`.farhand.yaml`)](docs/configuration.md)** — Complete reference for config discovery, field definitions, and environment variable interpolation.
- 🌐 **[Multi-Agent Pool & Dynamic Load Balancing](docs/multi-agent-pool.md)** — Setup guide for multi-agent clusters, health probing, and hardware tag routing (`--agent-tag`).
- ☁️ **[Remote Access via Cloudflare Tunnel](docs/cloudflared-tunnel.md)** — Connect securely over the internet with `cloudflared access tcp` without opening inbound router ports.

---

## License

Dual-licensed under either:
- **MIT License** ([LICENSE-MIT](LICENSE-MIT) or [http://opensource.org/licenses/MIT](http://opensource.org/licenses/MIT))
- **Apache License, Version 2.0** ([LICENSE-APACHE](LICENSE-APACHE) or [http://www.apache.org/licenses/LICENSE-2.0](http://www.apache.org/licenses/LICENSE-2.0))

at your option.

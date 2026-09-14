<p align="center">
  <img src="assets/logo.png" alt="Farhand Logo" width="220" style="border-radius: 24px;" />
</p>

<h1 align="center">Farhand (fh)</h1>

<p align="center">
  <strong>Zero-dependency remote build and test offloader.</strong>
</p>

<p align="center">
  <i>
    Farhand or fh (pronounced FAAAAAAAH)
  </i>
    <br/>
    <audio controls>
      <source src="https://www.myinstants.com/media/sounds/fahhh_KcgAXfs.mp3" type="audio/mpeg" />
    </audio>
</p>

<p align="center">
  <a href="https://github.com/Rayrsn/farhand/actions"><img src="https://img.shields.io/badge/build-passing-brightgreen?style=flat-square" alt="Build Status" /></a>
  <a href="https://github.com/Rayrsn/farhand/releases"><img src="https://img.shields.io/badge/version-0.8.0-orange?style=flat-square" alt="Version" /></a>
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
| **Persistent Dependency Cache** | **Yes** (`node_modules` stays remote) | Often wipes or conflicts | Local to remote box |
| **Multi-Branch APFS CoW Forking** | **Yes** (< 100ms, 0-byte duplicate) | No (duplicates entire folder) | No |
| **Delta Source Sync** | **Yes** (SHA-256 manifests over TCP) | Yes (rsync delta) | N/A (entire edit remote) |
| **Clean Process Cancellation** | **Yes** (kills remote process tree) | No (orphans compiler processes) | Yes |
| **Offline Multi-Agent Failover**| **Yes** (automatic load-balancing) | No | No |
| **Works with Any Local Editor** | **Yes** (pure CLI wrapper) | Yes | No |

---

## Core Features

- ⚡ **Zero External Dependencies**: Pure Rust binaries. Does not invoke or depend on system `ssh`, `rsync`, `tar`, or `gzip`.
- 📁 **Persistent Workspace Cache**: Remote dependencies (`node_modules/`, `target/`, `.venv/`) remain on the agent host across runs. Only changed source files are transferred.
- 🍏 **Instant APFS Copy-on-Write (CoW) Forking**: When working across different branches on shared hosts, new branch workspaces are cloned from canonical seeds (`main`/`master`) in **< 100ms using 0 additional disk blocks**.
- 🧹 **Automated Two-Tier LRU Garbage Collection**: Daemon automatically soft-prunes intermediate caches and evicts stale branch workspaces according to disk quotas (`--max-disk-gb`) and inactivity TTL (`--workspace-ttl-days`).
- 🔀 **Branch-Aware Project Addressing**: Automatically detects git branches and scopes workspaces as `<repo>__<branch>` so multiple developers never collide.
- 🛡️ **Section 5.1 Deletion Safety**: Strictly protects remote dependencies and build outputs from being deleted during manifest synchronization.
- 🛑 **Process Group Isolation**: Spawns compilation inside isolated process groups (`setpgid`). If you `Ctrl+C` locally, the entire remote compiler hierarchy is gracefully terminated.
- 🌐 **Multi-Agent Pool & Tag Routing**: Automatically discovers, health-checks, and load-balances jobs across a cluster of build agents.
- 📊 **Run Observability**: Query execution history, exit codes, synced bytes, and duration using `fh history`.

---

## The Two Binaries

1. **`fh` (Client)**: Scans the local project directory, hashes files, uploads deltas, requests remote command execution, streams live logs, and retrieves generated artifacts.
2. **`fhd` (Daemon)**: Listens on TCP (port `9876`), authenticates connections via token, maintains per-project workspaces, unpacks deltas, runs commands in process groups, streams `stdout`/`stderr`, and returns artifacts.

---

## Quickstart

### 1. Installation

#### Via Cargo (From Source)
```bash
cargo install --git https://github.com/Rayrsn/farhand.git fh fhd
```

#### Via Homebrew (macOS)
```bash
brew tap Rayrsn/farhand https://github.com/Rayrsn/farhand.git
brew install farhand
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

In your local project directory, configure `.farhand.yaml`:

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

# Inspect execution history
fh history

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

---

## License

Dual-licensed under either:
- **MIT License** ([LICENSE-MIT](LICENSE-MIT) or [http://opensource.org/licenses/MIT](http://opensource.org/licenses/MIT))
- **Apache License, Version 2.0** ([LICENSE-APACHE](LICENSE-APACHE) or [http://www.apache.org/licenses/LICENSE-2.0](http://www.apache.org/licenses/LICENSE-2.0))

at your option.

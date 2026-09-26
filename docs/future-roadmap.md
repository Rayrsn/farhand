# Farhand — Future Feature Roadmap & Architecture Explorations

This document outlines proposed future capabilities and architectural enhancements for **Farhand** (`fh` client and `fhd` agent). These ideas build upon the core no-external-system-binaries, persistent-workspace foundation (Stages 00–12) to expand Farhand from a remote compiler runner into a complete remote development acceleration platform.

---

## Priority 1: Game-Changing Developer Experience (High ROI)

### 1.1 Watch Mode & Continuous Build Loop (`fh watch`)
* **Concept**: Instead of running one-off invocations (`fh -- cargo test`), keep an active local file watcher that continuously syncs changes and re-triggers remote builds with sub-second latency.
* **Mechanism**:
  - Use `notify` to monitor local directory modifications (honoring `.gitignore` / `.farhand-ignore`).
  - Maintain a persistent TCP multiplexed session with `fhd` rather than reconnecting from scratch.
  - On file change, immediately compute delta, push missing chunks, and trigger the remote build command.
  - Ideal for `cargo watch`, `tsc --watch`, Next.js/Vite dev loops, and test runners (`vitest`, `pytest`).

### 1.2 Interactive Terminal & Pseudo-Terminal (PTY) Forwarding (`fh -t`)
* **Concept**: Enable running interactive CLI commands, TUI applications (e.g. `btop`, `htop`), REPLs (`python`, `node`, `psql`), and CLI setup wizards directly on the remote agent.
* **Mechanism**:
  - Allocate a remote pseudo-terminal (PTY) via `portable-pty` or POSIX `openpty`.
  - Stream `stdin` chunks from local terminal to remote process in real time.
  - Forward ANSI escape sequences, raw keyboard input (e.g. Ctrl-C, arrow keys, function keys), and terminal resize signals (`SIGWINCH`).
  - Support a `-t` / `--tty` flag on `fh` to request interactive PTY allocation.

### 1.3 Automatic Reverse Port Forwarding (`fh -L` / `--forward`)
* **Concept**: When running a remote development server or web API (e.g. `fh -- npm run dev`), automatically tunnel the remote port back to the developer's laptop.
* **Mechanism**:
  - Add `-L <local_port>:<remote_port>` syntax (e.g. `fh -L 3000:3000 -- npm run dev`).
  - `fh` opens a local TCP listener on `localhost:3000`.
  - Inbound local TCP connections are multiplexed over the existing Farhand framing socket and forwarded to `127.0.0.1:3000` on the remote agent.
  - Eliminates the need for external SSH tunnels or separate Cloudflare tunnel setups just to preview dev servers in the browser.

---

## Priority 2: Operational Robustness & Host Safeguards

### 2.1 Pre-Flight Host Resource & Disk Guard
* **Concept**: Prevent midway build aborts and obscure tool failures (such as `npm error ENOTEMPTY` / `No space left on device`) caused by exhausted disk space on the remote host.
* **Mechanism**:
  - Before accepting a build or running dependency hooks, `fhd` checks available filesystem space on its workspace volume.
  - If free space falls below a configurable threshold (e.g. `< 2.5 GB`):
    1. Automatically trigger an emergency pass of the two-tier LRU garbage collector (`trim_workspace_caches`).
    2. If space remains critically low, reject the run early with a clear warning: `Remote agent disk low (<threshold> free). Run 'fh clean' or free host disk.`
  - Return host health metrics in the `STATUS` frame.

### 2.2 Interactive Remote Workspace Shell (`fh shell` / `fh exec`)
* **Concept**: Provide a first-class command to inspect or troubleshoot a project's remote workspace without needing manual SSH configuration.
* **Mechanism**:
  - `fh shell` connects to `fhd`, resolves the persistent workspace for the current project/branch, and spawns an interactive shell (`$SHELL` / `/bin/zsh` / `/bin/bash` or `cmd.exe`).
  - Automatically loads forwarded environment variables and project state.
  - Makes inspecting generated files, debugging missing native libraries, or checking compiler caches instantaneous.

### 2.3 Dynamic Multi-Target Output Matrix
* **Concept**: Allow `.farhand.yaml` to specify conditional build artifacts depending on build flags, environment variables, or build profiles.
* **Mechanism**:
  - Support pattern interpolation in `outputs`:
    ```yaml
    outputs:
      - path: target/${BUILD_PROFILE:-release}/my-app
      - glob: dist/**/*.js
    ```
  - Allow client flags to append or override outputs without editing config files.

---

## Priority 3: Performance & Network Acceleration

### 3.1 Zstandard (zstd) Wire Compression
* **Concept**: Replace or complement `gzip` (`flate2`) with `zstd` for delta archiving.
* **Mechanism**:
  - Zstandard delivers 3–5x faster compression and decompression throughput compared to gzip with lower CPU overhead.
  - Can optionally negotiate compression algorithm during handshake (`zstd`, `gzip`, or `none` for ultra-fast 10Gbps local LANs).

### 3.2 Global Content-Addressable Storage (CAS)
* **Concept**: Deduplicate dependencies and build objects across different projects and branches on the remote host.
* **Mechanism**:
  - Store files by their SHA-256 hash in `/var/farhand/cas/objects/`.
  - Workspace directories hardlink or reflink (APFS / Btrfs) to the CAS store.
  - Reduces disk usage dramatically when multiple projects or branches share the same large dependency trees (e.g. node_modules, Python wheels, Cargo crates).

---

## Priority 4: Security & Enterprise Readiness

### 4.1 Native Zero-Config TLS / mTLS (`rustls`)
* **Concept**: Provide optional built-in encryption directly in `fh` and `fhd` without relying on external tunnels or system OpenSSL.
* **Mechanism**:
  - Integrate `rustls` for pure-Rust, memory-safe TLS.
  - Support `--tls-cert` and `--tls-key` flags on `fhd`.
  - Client supports CA certificate pinning or auto-generated certificate fingerprint verification for secure direct WAN / VPN connectivity.

### 4.2 Declarative Toolchain Manager Hooks (`toolchain`)
* **Concept**: Ensure remote agent uses matching runtime/compiler versions (e.g. Node 22, Go 1.23, Rust 1.82).
* **Mechanism**:
  - Allow declarative toolchain versions in `.farhand.yaml`:
    ```yaml
    toolchain:
      node: "22"       # Auto-runs 'nvm use 22' or 'fnm use 22'
      rust: "stable"   # Uses 'rustup run stable'
    ```
  - Avoids build failures caused by engine version mismatches on multi-user agent hosts.

---

## Priority 5: Observability & Advanced Integrations

### 5.1 Remote Agent Dashboard (`fh top` / `fh agent info`)
* **Concept**: Provide visibility into remote agent system utilization, running builds, and disk consumption from the client CLI.
* **Mechanism**:
  - Query agent health, load averages, memory usage, disk quotas, active workspaces, and queue depth.
  - Display a concise terminal dashboard to help developers monitor their homelab or build servers.

### 5.2 Language Server Protocol (LSP) Remote Offloader (`fh lsp`)
* **Concept**: Offload heavy language servers (`rust-analyzer`, `typescript-language-server`, `clangd`) from developer laptops to the remote agent.
* **Mechanism**:
  - Pipe LSP JSON-RPC traffic over a dedicated Farhand connection to an LSP server running against the remote persistent workspace.
  - Developer editors (VS Code, Neovim, Zed) get instant completions, diagnostics, and indexing without consuming gigabytes of local RAM and battery.

# Farhand Configuration Guide (`.farhand.yaml`)

This document provides a comprehensive specification, field reference, and example catalog for configuring the **Farhand** (`fh`) client using `.farhand.yaml`.

---

## 1. Overview & File Discovery

When executing commands with `fh`, the client searches for configuration to avoid passing long command-line flags on every invocation.

### Discovery Order
1. **Explicit Flag**: If `--config <path>` is provided, `fh` loads that exact file. If the file does not exist, `fh` immediately terminates with exit code `125`.
2. **Project-Local Default**: If `--config` is not specified, `fh` looks for `.farhand.yaml` in the root of the project directory being synced (by default, the current working directory).
3. **No Config**: If `.farhand.yaml` does not exist, `fh` proceeds with default settings, requiring `--host` and `--token` (via CLI flags or environment variables).

```bash
# Runs using .farhand.yaml located in current directory
fh -- cargo test

# Runs using an explicit configuration file
fh --config ./deploy/farhand.staging.yaml -- cargo build --release
```

---

## 2. Complete Field Reference

Both **`camelCase`** and **`snake_case`** keys are supported.

| Field | Type | Default | Description |
| :--- | :--- | :--- | :--- |
| `host` | `string` | *(None)* | Remote agent address in `host:port` format. Required if `--host` and `FARHAND_HOST` are unset. |
| `token` | `string` | *(None)* | Shared authentication token. Supports `${VAR}` expansion. Required unless `insecureSkipToken: true`. |
| `name` | `string` | Local directory basename | Project identifier and persistent workspace key on the agent daemon. |
| `outputs` | `list of strings` | `[]` | Explicit relative paths or globs to retrieve back after exit code `0`. If omitted, preset auto-detection is used. |
| `outDir` / `out_dir` | `string` | `./farhand-out` | Local directory where retrieved build artifacts will be extracted. |
| `insecureSkipToken` / `insecure_skip_token` | `boolean` | `false` | If `true`, allows connecting to an agent without a token. |
| `verbose` | `boolean` | `false` | If `true`, prints detailed scan, sync, transfer timing, and byte metrics on every run. |
| `template` | `string` | *(None)* | Explicit template name to enforce build environment presets (e.g. `rust`, `npm`, `python`). |
| `agentTag` / `agent_tag` | `string` | *(None)* | Agent selection tag for multi-agent pools (e.g. `gpu`, `linux-x64`, `apple-silicon`). |
| `noCache` / `no_cache` | `boolean` | `false` | If `true`, instructs agent to bypass dependency caching hooks. |

---

## 3. Environment Variable Interpolation

Values inside `.farhand.yaml` can dynamically reference environment variables at runtime.

### Syntax Rules
- **`${VARIABLE}`**: Replaced with the environment variable value. If the variable is unset, it evaluates to an empty string `""`.
- **`${VARIABLE:-default}`**: Replaced with the variable value. If unset or empty, it evaluates to `default`.
- **`$VARIABLE`**: Shorthand syntax for alphanumeric and underscore identifiers.
- **`$$`**: Escapes the `$` symbol, producing a single literal `$` character.

### Example
```yaml
host: ${REMOTE_BUILD_HOST:-192.168.1.100:9876}
token: ${FARHAND_TOKEN}
name: my-app-${USER:-dev}
```

---

## 4. Parameter Precedence Hierarchy

When the same configuration option is defined in multiple places, Farhand evaluates them according to this strict hierarchy:

```
┌─────────────────────────────────────────────────────────┐
│ 1. CLI Flags (e.g. --host, --token, --output, --verbose)│  (Highest Priority)
└────────────────────────────┬────────────────────────────┘
                             ▼
┌─────────────────────────────────────────────────────────┐
│ 2. Environment Variables (FARHAND_HOST, FARHAND_TOKEN)  │
└────────────────────────────┬────────────────────────────┘
                             ▼
┌─────────────────────────────────────────────────────────┐
│ 3. Config File (.farhand.yaml or --config <file>)       │
└────────────────────────────┬────────────────────────────┘
                             ▼
┌─────────────────────────────────────────────────────────┐
│ 4. Built-in Defaults (./farhand-out, directory name)    │  (Lowest Priority)
└─────────────────────────────────────────────────────────┘
```

> [!NOTE]
> Command-line flags always take precedence. For example, if `.farhand.yaml` specifies `verbose: false`, running `fh --verbose -- <command>` will enable verbose telemetry for that run.

---

## 5. Practical Configuration Examples

### 5.1 Minimal Setup (Local Network / Direct Connection)
Ideal for a secondary desktop or home lab build server.

```yaml
# .farhand.yaml
host: 192.168.1.50:9876
token: ${FARHAND_TOKEN}
```
**Usage:**
```bash
export FARHAND_TOKEN="super-secret-build-token"
fh -- cargo build
```

---

### 5.2 Rust Project with Compiled Binary Retrieval
Builds release binaries remotely and downloads the generated executable into `./bin`.

```yaml
# .farhand.yaml
host: build-box.lan:9876
token: ${FARHAND_TOKEN}
name: backend-service

# Retrieve only the final release binary instead of entire target/
outputs:
  - target/release/backend-service

outDir: ./bin
verbose: true
```
**Usage:**
```bash
fh -- cargo build --release
# Resulting binary extracted locally to: ./bin/target/release/backend-service
```

---

### 5.3 Web Frontend (React / Vite / Next.js)
Offloads heavy webpack/vite/tsc compilation and pulls the distribution bundle.

```yaml
# .farhand.yaml
host: 10.0.0.15:9876
token: ${FARHAND_TOKEN}
name: web-dashboard

outputs:
  - dist/
  - .next/standalone/
  - build/

outDir: ./dist-local
```
**Usage:**
```bash
fh -- npm run build
```

---

### 5.4 Python / Machine Learning with GPU Agent Pinning
Pins jobs to agents with the `gpu` tag and downloads training weights and logs.

```yaml
# .farhand.yaml
host: cluster.corp.internal:9876
token: ${CORP_AGENT_TOKEN}
name: vision-model-training

# Pin to an agent host configured with GPU capabilities
agentTag: gpu

outputs:
  - models/*.pt
  - logs/metrics.json

outDir: ./experiment-results
verbose: true
```
**Usage:**
```bash
fh -- python train.py --epochs 50
```

---

### 5.5 Secure SSH Port-Forwarding Tunnel Setup
When connecting over the public internet through an SSH bastion.

```yaml
# .farhand.yaml
# Points to local forwarded port established via ssh -L 9876:127.0.0.1:9876 user@remote-host
host: 127.0.0.1:9876
token: ${FARHAND_SSH_TUNNEL_TOKEN}
name: secure-app
verbose: false
```
**Usage:**
```bash
# 1. Establish SSH tunnel in background:
ssh -N -L 9876:127.0.0.1:9876 user@remote-build-machine &

# 2. Run farhand normally over the tunnel:
fh -- make test
```

---

### 5.6 Cloudflare Tunnel / Tailscale Private Mesh
When using private DNS resolution across Tailscale, WireGuard, or Cloudflare Zero Trust.

```yaml
# .farhand.yaml
host: agent.farhand.internal:9876
token: ${FARHAND_TOKEN}
name: payments-api
outputs:
  - target/debug/deps/*.so
  - coverage/
```

---

## 6. Verification & Troubleshooting

### Check Resolved Configuration
Run `fh` with `--verbose` to inspect the exact resolved host, project name, and upload statistics:

```bash
fh --verbose -- echo "test"
```

Sample output:
```text
=== Farhand Remote Runner ===
Connecting to agent at: 192.168.1.50:9876
Project: backend-service (/Users/dev/backend-service)
Remote command: ["echo", "test"]
Scanned 42 local files in 4.12ms
[Delta Sync] Remote workspace is completely up to date. 0 files to transfer!
test
=== Farhand Execution Summary ===
[Project]       backend-service
[Agent]         192.168.1.50:9876
---------------------------------
[Scan]          42 files in 4.12ms
[Delta Sync]    0 files (0 bytes) in 25µs
[Remote Build]  Completed in 15.3ms
[Artifacts]     0 bytes in 0ns
---------------------------------
```

### Common Errors

1. **`Error: agent host address is required` (Exit Code 125)**
   - Cause: Neither `--host`, `FARHAND_HOST`, nor `host` in `.farhand.yaml` was found.
   - Fix: Add `host: <host:port>` to `.farhand.yaml` or pass `--host <host:port>`.

2. **`Error: shared auth token is required` (Exit Code 125)**
   - Cause: Agent authentication token was missing.
   - Fix: Export `FARHAND_TOKEN` in your shell, set `token: ${FARHAND_TOKEN}` in `.farhand.yaml`, or pass `--token <token>`.

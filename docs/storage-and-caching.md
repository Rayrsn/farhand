# Storage Optimization & Caching Guide

Farhand is built specifically to allow multiple developers across local networks and remote connections to build simultaneously on the same repositories without exhausting disk space on shared agent machines (like Apple Silicon Mac Minis).

This guide details the **storage lifecycle**, **APFS Copy-on-Write cloning**, and **automated garbage collection** systems that make high-density multi-branch building possible.

---

## 1. The Multi-Developer Storage Problem

In standard CI or naive remote runners, each developer branch creates a completely isolated workspace directory. In large TypeScript, Rust, or Python codebases, this quickly balloons out of control:
- `node_modules/`: 1 GB – 3 GB per branch
- Rust `target/`: 5 GB – 15 GB per branch
- Python `.venv/`: 500 MB – 2 GB per branch

With 5 developers working across 3 branches each, a shared 512 GB SSD can run out of space within days.

Farhand solves this through **Four Pillars of Storage Management**:
1. **APFS Copy-on-Write (CoW) Branch Forking** (0-byte duplicate storage)
2. **Automated Two-Tier LRU Garbage Collection** (Quota & TTL enforcement)
3. **Shared Host-Level Toolchain Caches** (`sccache`, global package caches)
4. **Client-Driven Remote Cleanup** (`fh clean`)

---

## 2. APFS Copy-on-Write (CoW) Workspace Forking

Farhand leverages Apple's native APFS filesystem (`clonefile(2)`) on macOS and `reflink` on Linux (Btrfs, XFS) to provide instantaneous branch workspace creation without duplicating data.

### How Seed Workspaces Work
1. When a build is run on the primary branch (`main` or `master`), Farhand resolves the workspace directory as:
   ```
   /var/farhand/workspaces/my-repo
   ```
2. When a developer switches to a new feature branch (e.g. `feat-auth`), the client transparently scopes the project as:
   ```
   /var/farhand/workspaces/my-repo__feat-auth
   ```
3. Upon receiving the handshake, `fhd` checks if `my-repo__feat-auth` exists. If it does not:
   - It searches for the canonical seed workspace (`my-repo`, `my-repo__main`, or `my-repo__master`).
   - It clones the entire seed workspace using `libc::clonefile()` in **< 80 milliseconds**.
   - The clone uses **0 additional disk blocks**: all file data blocks are shared at the filesystem level.
   - All installed dependencies (`node_modules/`, `target/`) are instantly available to the branch.
4. When `fh` performs its delta sync, it only uploads the few source files that changed on the feature branch. Modified files take advantage of Copy-on-Write: only the modified disk blocks consume new storage.

---

## 3. Automated Two-Tier LRU Garbage Collection

`fhd` includes a built-in, asynchronous garbage collection engine that continuously monitors disk usage and workspace inactivity.

### GC Configuration Flags on `fhd`
```bash
fhd \
  --max-disk-gb 100 \
  --workspace-ttl-days 7 \
  --gc-interval-secs 3600
```

| Flag | Default | Description |
| :--- | :--- | :--- |
| `--max-disk-gb` | None (disabled) | Maximum total disk space allocated for all workspaces combined. |
| `--workspace-ttl-days` | None (disabled) | Maximum days of inactivity before a non-canonical branch workspace is evicted. |
| `--gc-interval-secs` | `3600` (1 hour) | Frequency of the background garbage collection check. |

### Tracking Inactivity
Every time a client connects and executes a command, `fhd` touches a marker file:
```
/var/farhand/workspaces/<project>/.farhand-last-used
```
This timestamp allows `fhd` to accurately rank workspaces by Least Recently Used (LRU) order.

### The Two-Tier Eviction Algorithm

When the background collector wakes up or total workspace storage exceeds `--max-disk-gb`:

```
+-------------------------------------------------------------+
|               1. Scan all project workspaces                |
|           Calculate total bytes & rank by last-used         |
+-------------------------------------------------------------+
                              |
                              v
+-------------------------------------------------------------+
|             Check Inactivity TTL Eviction                   |
|  Delete non-canonical branch workspaces older than N days   |
+-------------------------------------------------------------+
                              |
                              v
+-------------------------------------------------------------+
|             Still exceeding --max-disk-gb quota?            |
+-------------------------------------------------------------+
               /                               \
             YES                                NO
             /                                   \
            v                                     v
+-----------------------------+              [ Finished ]
| Tier 1: Soft Cache Pruning  |
| Prune volatile caches:      |
| - target/debug/incremental  |
| - node_modules/.cache       |
| - .gradle/caches            |
| (Keeps dependencies intact) |
+-----------------------------+
            |
            v
+-------------------------------------------------------------+
|             Still exceeding --max-disk-gb quota?            |
+-------------------------------------------------------------+
               /                               \
             YES                                NO
             /                                   \
            v                                     v
+-----------------------------+              [ Finished ]
| Tier 2: Hard Branch Purging |
| Evict oldest branch         |
| workspaces (LRU order)      |
| * Canonical seed workspaces |
|   (main, master) are NEVER  |
|   evicted!                  |
+-----------------------------+
```

---

## 4. Shared Host-Level Toolchain Caches

In addition to per-workspace dependency persistence, daemon configurations should expose host-level shared caches so compilers can reuse compilation units across different workspaces:

### 1. `sccache` (Rust / C / C++)
`sccache` shares compilation artifacts across every workspace on the machine via a shared cache directory:
```bash
export RUSTC_WRAPPER="/usr/local/bin/sccache"
export SCCACHE_DIR="/var/farhand/cache/sccache"
export SCCACHE_CACHE_SIZE="30G"
```

### 2. Global Node & Package Manager Caches
```bash
export NPM_CONFIG_CACHE="/var/farhand/cache/npm"
```

### 3. Go Build Cache
```bash
export GOCACHE="/var/farhand/cache/go-build"
export GOPATH="/var/farhand/cache/go"
```

These environment variables are pre-configured in Farhand's official `launchd` plist and `systemd` service units.

---

## 5. Client-Driven Remote Cleanup (`fh clean`)

Developers can proactively reclaim storage on the agent using the `fh clean` subcommand:

### 1. Clean Current Branch Workspace
Removes the remote workspace for the current branch when you finish a feature or merge a pull request:
```bash
fh clean
```

### 2. Soft Prune Intermediate Caches Only
Frees gigabytes of volatile compiler intermediate caches (`incremental/`, `.cache/`) on the agent without deleting installed dependencies:
```bash
fh clean --caches-only
```

### 3. Clean All Branch Workspaces
Deletes all ephemeral feature branch workspaces associated with the project while leaving the canonical seed workspace (`main`) intact:
```bash
fh clean --all-branches
```

### 4. Clean a Specific Project by Name
```bash
fh clean --name old-experiment-project
```

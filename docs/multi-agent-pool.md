# Multi-Agent Pool & Dynamic Load Balancing

Farhand supports pooling multiple remote agent machines into an elastic build cluster. The client automatically probes candidate agents, routes jobs to the least busy node matching required hardware tags, and seamlessly fails over if an agent goes offline.

---

## 1. Defining an Agent Pool in `.farhand.yaml`

Instead of specifying a single `host`, declare an `agents` list with capability tags:

```yaml
# .farhand.yaml
agents:
  - host: "mac-mini.local:9876"
    token: "${MAC_TOKEN}"
    tags: ["apple-silicon", "lan", "macos"]

  - host: "linux-ci-box.internal:9876"
    token: "${LINUX_TOKEN}"
    tags: ["linux", "x86_64", "gpu"]

  - host: "cloud-runner.example.com:9876"
    token: "${CLOUD_TOKEN}"
    tags: ["backup", "cloud"]

outputs:
  - "dist"
```

---

## 2. Dynamic Dispatch Algorithm

When executing a command (e.g. `fh cargo build`):

1. **Tag Filtering**:
   If an `--agent-tag` flag (or `agentTag` config) is specified (e.g. `--agent-tag apple-silicon`), candidate agents not matching the tag are filtered out.
2. **Concurrent Health Probe**:
   The client opens short-lived TCP connections in parallel to all candidate agents and exchanges a `STATUS_REQ` / `STATUS_RESP` handshake.
   - Measures network round-trip latency.
   - Retrieves agent CPU core count, active running jobs, and current system load.
3. **Least Busy Selection**:
   The client selects the healthiest agent according to:
   $$\text{Score} = \text{Active Runs} \times 1000 + \text{Latency (ms)}$$
   The agent with the fewest active jobs is prioritized. Latency breaks ties.
4. **Offline Failover**:
   If the preferred agent fails to respond within the timeout, `fh` transparently falls back to the next best available node in the pool.

---

## 3. CLI Tag Pinning

Developers can route specific jobs to dedicated hardware nodes at runtime:

```bash
# Force execution on Apple Silicon Mac Mini
fh --agent-tag apple-silicon -- cargo build --release

# Force execution on GPU-enabled Linux box
fh --agent-tag gpu -- python train.py

# Pin to local LAN agents only
fh --agent-tag lan -- npm test
```

---

## 4. Agent-Side Tag Configuration

Agents advertise their capabilities when launching `fhd` via the repeatable `--tag` flag:

```bash
# On your Apple Silicon Mac Mini:
fhd --listen 0.0.0.0:9876 --tag apple-silicon --tag macos --tag lan

# On a Linux GPU build box:
fhd --listen 0.0.0.0:9876 --tag linux --tag x86_64 --tag gpu
```

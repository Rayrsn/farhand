# Observability

Farhand exposes two ways to see what an agent is doing: `fh top` (a live
terminal dashboard) and an opt-in Prometheus endpoint for anything that wants
to be scraped.

## Live terminal view

```bash
fh top            # live TUI: CPU, RAM, disk, active builds
fh top --once     # a single snapshot, for scripts
fh agent info     # formatted host specs
fh agent info --json
```

## Prometheus endpoint

Disabled by default. Turn it on with a port:

```bash
fhd --listen 0.0.0.0:9876 --token "$FARHAND_TOKEN" --metrics-port 9100
```

The endpoint serves `GET /metrics` (Prometheus text format) and
`GET /healthz` (plain `ok`, suitable for a container liveness probe). It binds
separately from the agent port, so it can be firewalled or kept on a private
interface independently.

> The metrics port is unauthenticated, like Prometheus itself. Bind it to an
> interface your network considers private, or leave it off. If the port
> cannot be bound, the agent logs an error and keeps running — metrics are
> never worth taking the agent down for.

Scrape config:

```yaml
scrape_configs:
  - job_name: farhand
    scrape_interval: 15s
    static_configs:
      - targets: ["agent-host:9100"]
```

### Exposed metrics

| Metric | Type | Labels | Meaning |
| :--- | :--- | :--- | :--- |
| `farhand_up` | gauge | | `1` while the agent is serving metrics |
| `farhand_uptime_seconds` | gauge | | Seconds since the agent started |
| `farhand_active_runs` | gauge | | Runs executing right now |
| `farhand_max_runs` | gauge | | Configured concurrency limit (`--max-concurrent-runs`) |
| `farhand_queue_depth` | gauge | | Runs waiting on a project lock or a run slot |
| `farhand_max_queued_runs` | gauge | | Configured queue limit (`--max-queued-runs`) |
| `farhand_active_builds` | gauge | `project` | Runs in flight, per project |
| `farhand_workspaces` | gauge | | Persistent workspaces on the agent |
| `farhand_cpu_count` | gauge | | Usable CPUs on the host |
| `farhand_disk_free_bytes` | gauge | | Free space on the workspace volume |
| `farhand_disk_total_bytes` | gauge | | Total space on the workspace volume |
| `farhand_load_average` | gauge | `interval` (`1`, `5`, `15`) | Host load average |
| `farhand_memory_used_bytes` | gauge | | Physical memory in use |
| `farhand_memory_total_bytes` | gauge | | Physical memory installed |

Run ids are deliberately **not** labels. A run id changes on every build, so
labelling by it would leave a stale series behind in Prometheus forever.
`farhand_active_builds` is labelled by `project` instead, which is bounded by
the number of projects on the host.

Disk and memory gauges are omitted rather than reported as zero when the
platform probe fails, so a scrape never looks like "the disk is full".

### Alerts worth having

```promql
# The agent is refusing runs because it is saturated.
farhand_active_runs / farhand_max_runs > 0.9

# Builds are piling up behind a lock.
farhand_queue_depth > 0 for: 10m

# The workspace volume is close to the emergency-GC threshold.
farhand_disk_free_bytes < 5e9
```

## Grafana dashboard

`docs/grafana-farhand-dashboard.json` is an importable dashboard with panels
for the gauges above. In Grafana: **Dashboards → New → Import → Upload JSON
file**. It expects a datasource named `Prometheus`; change the
`datasource.uid` in the JSON if yours differs.

The dashboard is provided as JSON rather than a screenshot so it stays
readable, diffable, and importable — and so nobody has to trust a picture of
numbers that may not match the current metric set.

## Run history

For per-run history (exit codes, bytes synced, duration) without a metrics
backend, the agent records it and the client reads it back:

```bash
fh history              # recent runs for this project
fh history --limit 20
```

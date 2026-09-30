# Run Farhand from GitHub Actions

The interesting use of Farhand in CI is **not** replacing the runner — hosted
runners are disposable and have no persistent state, which is exactly the
property Farhand relies on. The interesting use is *remote* execution, so
CI spends a cheap runner's orchestration budget while the actual compilation
happens on a machine you control with warm caches.

## Prerequisites on the agent

The agent must be reachable from the runner. Either:

- a self-hosted runner on the same network as the agent, or
- a public agent behind TLS, or a tunnel — see
  [the Cloudflare guide](../docs/cloudflared-tunnel.md)

Store the token as a repository secret. It is the only thing guarding remote
command execution, so treat it as you would a deploy key.

## Warm the workspace, then build

```yaml
name: build
on: [push, pull_request]

jobs:
  build:
    # A hosted runner is fine: it only orchestrates. The compile happens on the
    # agent, which keeps its dependency cache and build tree between runs.
    runs-on: ubuntu-latest
    env:
      FARHAND_HOST: ${{ vars.FARHAND_HOST }}
      FARHAND_TOKEN: ${{ secrets.FARHAND_TOKEN }}

    steps:
      - uses: actions/checkout@v4

      - name: Install Farhand
        run: cargo install farhand-cli farhand-agent --locked

      # A warm-up build pays the cold-sync and dependency-install cost once, so
      # the timed run below measures the steady state rather than first-run
      # overhead. A cold workspace also legitimately re-hashes everything on
      # the second run — see the note in BENCHMARKS.md — so skipping this
      # would make your timings look worse than the tool is.
      - name: Warm the agent workspace
        run: fh -- true

      - name: Test
        run: fh cargo test --release

      - name: Build
        run: fh cargo build --release

      - name: Fetch the binary
        uses: actions/upload-artifact@v4
        with:
          name: binary-${{ github.sha }}
          path: target/release/<your-binary>
```

`outputs` in `.farhand.yaml` decides what comes back, so point it at what CI
actually needs:

```yaml
# .farhand.yaml
outputs:
  - target/release/my-app
```

## What to expect

- **The first run of a commit is cold**, and the second re-hashes; measure
  from the third.
- **CI runners are ephemeral**, so a hosted runner cannot *be* the agent. The
  agent must be somewhere persistent — a spare machine, a Mac Mini, a NAS.
- **Exit codes are meaningful.** `125` means Farhand could not reach or talk to
  the agent; `2` means the invocation was wrong; anything else is your build's
  own code. See [the CLI reference](../docs/cli.md#exit-codes).

## Pinning toolchains

If the agent's toolchain drifts from the project's, builds fail in ways that
look like your code. Pin it:

```yaml
- name: Build with a pinned toolchain
  run: fh -T rust=1.88.0 -T node=22 -- cargo build --release
```

Changing a pinned toolchain re-runs the dependency-install hook, so a
`node_modules` built under a different version is not left in place.

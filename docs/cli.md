# CLI Reference

Complete reference for `fh`. Most days you need only a handful of these — see
the [README quickstart](../README.md#quickstart) for the common path.

```bash
fh [OPTIONS] [COMMAND]... [COMMAND]
```

Anything not taken as a subcommand is the command to run remotely:

```bash
fh npm run build            # runs `npm run build` on the agent
fh cargo build --release
fh -L 3000:3000 -- npm run dev
```

## Global options

### Connection

| Flag | Config key | Meaning |
| :--- | :--- | :--- |
| `--host <HOST>` | `host` | Agent address as `host:port`. Also `FARHAND_HOST`. |
| `--token <TOKEN>` | `token` | Shared secret. Also `FARHAND_TOKEN`. |
| `--dir <DIR>` | — | Local directory to sync. Default `.`. |
| `--name <NAME>` | `name` | Project / workspace key. Default: directory basename. |
| `--config <PATH>` | — | Config file path. Default `./.farhand.yaml`. |
| `--insecure-skip-token` | `insecureSkipToken` | Connect to an agent with no token. Only for a local dev agent. |
| `--agent-tag <TAG>` | `agentTag` | Pin to a tagged agent in a multi-agent pool. |

**Precedence:** command-line flag → environment variable → `.farhand.yaml` →
default. Note that `FARHAND_HOST` and `FARHAND_TOKEN` outrank the config file,
so an exported variable silently wins over a committed config. Unset them when
testing a different agent:

```bash
env -u FARHAND_HOST -u FARHAND_TOKEN fh doctor
```

### Project selection and workspaces

| Flag | Meaning |
| :--- | :--- |
| `--branch <BRANCH>` | Explicit branch name for workspace isolation. Default: the current git branch. |
| `--no-branch-scope` | Disable branch scoping and use one workspace per project. |
| `--template <NAME>` | Force a specific template instead of auto-detecting. |
| `--no-cache` | Skip the dependency-install hook for this run. |

Without `--no-branch-scope`, each branch gets its own workspace, so switching
branches does not reuse a stale build tree.

### Artifacts

| Flag | Config key | Meaning |
| :--- | :--- | :--- |
| `-o, --output <PATH>` | `outputs` | Explicit path to fetch back. Repeatable. |
| `--no-output` | — | Skip artifact retrieval entirely. |
| `--out-dir <DIR>` | `outDir` | Local directory to extract into. Default `./farhand-out`. |

Artifacts are only retrieved when the remote command exits `0`.

### Execution

| Flag | Meaning |
| :--- | :--- |
| `-t, --tty` | Allocate a PTY. Use for anything that needs a terminal. |
| `-L, --forward <LOCAL:REMOTE>` | Reverse port forward. Repeatable. See [the guide](port-forwarding.md). |
| `-e, --env <KEY=VAL>` | Set an environment variable remotely. Repeatable. |
| `--no-env` | Do not forward ambient local environment variables. |
| `--print-env` | List the variable *names* that would be forwarded, then exit. Values are never printed. |
| `-T, --toolchain <NAME=VERSION>` | Pin a language toolchain. Repeatable. |
| `--compression <ALGO>` | `zstd` (default), `gzip`, or `none`. |
| `--watch-debounce <MS>` | Coalesce file events in watch mode. Default `150`. |

`--print-env` is worth knowing about: environment forwarding is on by default,
so if a build behaves differently between your machine and the agent, this
tells you exactly what is crossing the wire without revealing any secret.

### Transport security

| Flag | Meaning |
| :--- | :--- |
| `--tls` | Enable TLS. |
| `--tls-ca <PATH>` | Verify the agent against a custom CA (PEM). |
| `--tls-fingerprint <SHA256>` | Pin the agent certificate by fingerprint. |
| `--tls-cert <PATH>` / `--tls-key <PATH>` | Client certificate and key for mTLS. |
| `--tls-insecure` | Accept any certificate. **Insecure.** |

Prefer `--tls-ca` or `--tls-fingerprint` over `--tls-insecure`, which provides
encryption but no authentication.

### Diagnostics

| Flag | Meaning |
| :--- | :--- |
| `-v, --verbose` | Per-run sync, transfer, and timing detail. |
| `--log-level <LEVEL>` | `trace`, `debug`, `info`, `warn`, `error`. |
| `--log-format <FORMAT>` | `text` or `json`. |
| `-h, --help` / `-V, --version` | — |

`--log-format json` makes the client's own log stream machine-readable, which
is useful when wrapping `fh` in CI.

## Subcommands

### `fh init`

Create `.farhand.yaml`, and optionally a project template.

| Flag | Meaning |
| :--- | :--- |
| `--host`, `--token` | Values to write into the generated config. |
| `--token-env` | Write `token: "${FARHAND_TOKEN}"` instead of a plaintext token. **Use this for anything you commit.** |
| `-n, --name` | Project name. Default: directory name. |
| `-t, --template` | Preset to detect from. |
| `--with-template` | Also write `.farhand/templates/<name>.yaml`. |
| `-f, --force` | Overwrite existing files. |

### `fh watch [COMMAND]...`

Sync and rebuild on every local change, debounced. Honours each matched
template's `ignoreExtra`. See [port forwarding](port-forwarding.md#interaction-with-watch-mode)
for using `-L` here.

### `fh templates <list|show|init|push>`

Inspect and manage the template system. See [Templates](templates.md).

### `fh sync`

Bring the agent's workspace up to date without running a build.

| Flag | Meaning |
| :--- | :--- |
| `--dry-run` | Report what would be transferred; send no file data. |
| `--list` | Name every path that would be transferred. |

### `fh why <PATH>`

Explain what happens to one path: uploaded, already on the agent (and whether
it was a content-addressed hit), or ignored — including which rule excluded it.

### `fh doctor`

One read-only pass over everything that commonly breaks: where it is pointed,
whether a token is present and stored safely, whether the transport is
encrypted, declared toolchains, and the agent's connectivity, disk, queue, and
load. It distinguishes "cannot reach the agent" from "reached it and it
rejected your token". Exits `125` when something is actually wrong.

### `fh history`

Recent runs from the agent: exit code, duration, bytes synced.

| Flag | Meaning |
| :--- | :--- |
| `--name <NAME>` | Project to query. |
| `--limit <N>` | Maximum rows. Default `10`. |

### `fh clean`

Free remote workspace space.

| Flag | Meaning |
| :--- | :--- |
| `--name <NAME>` | Project/branch to clean. |
| `--all-branches` | Clean every non-canonical branch workspace. |
| `--caches-only` | Only drop intermediate build caches; keep the workspace. |

Workspaces with an active run are always skipped.

### `fh top`

Live dashboard of remote activity, CPU, memory, disk, and active builds.

| Flag | Meaning |
| :--- | :--- |
| `--agent <HOST>` | Agent to monitor. |
| `--once` | Print a snapshot and exit. |
| `-i, --interval <SECS>` | Refresh interval. Default `1`. |


### `fh agent info`

Agent specifications, resource usage, and active jobs. `--json` for
machine-readable output.

### `fh shell`

Interactive shell inside the remote workspace (allocated PTY).

| Flag | Meaning |
| :--- | :--- |
| `--shell <SHELL>` | Shell to launch. Default `$SHELL` or `/bin/sh`. |
| `--no-sync` | Do not sync local changes first. |

### `fh exec`

Run an ad-hoc command remotely without artifact sync or dependency hooks.

| Flag | Meaning |
| :--- | :--- |
| `-t, --tty` | Allocate a PTY. |

### `fh lsp -- <SERVER>`

Offload a language server into the remote workspace. See
[LSP integration](lsp-integration.md).

### `fh completions <SHELL>`

Print a completion script to stdout.

```bash
fh completions bash > /etc/bash_completion.d/fh
```

### `fh man [--dir <DIR>]`

Write man pages derived from the binary's own CLI definition, so they cannot
drift from the flags. Default output directory `man`.

## Exit codes

| Code | Meaning |
| :--- | :--- |
| `0` | Success. Artifacts, if any, were retrieved. |
| `1`–`124` | The remote command's own exit code. |
| `125` | Farhand could not do its job — wrong host, bad token, TLS failure, or a usage error. Nothing ran. |
| `127` | The agent could not execute the command: not found, or not executable. |
>
> The distinction that matters is **whose** failure it is. `1`–`124` means your
> build ran and failed, and the number is its own. `125` means Farhand itself
> could not run anything, so the build never started.
>
> Two things that will otherwise surprise you:
>
> - **A build killed by a signal reports `1`**, not `128 + N`. The agent reports
>   a process that died without an exit status as a plain failure, so an
>   OOM-killed build and one that legitimately returns `1` look identical from
>   the exit code alone. Correlate with `fh history` or the agent log.
> - **`126` is not used.** A command that cannot be executed reports `127`.
>
> `fh` never returns `128 + N`, and the `126–127` range described in the
> project's `AGENTS.md` reserves a `126` that the code does not produce.
>
> Verified against a live agent: `exit 3` → `3`, a nonexistent command → `127`,
> a non-executable file → `127`, `kill -TERM $$` → `1`, `kill -KILL $$` → `1`.

## `fhd` options

The agent side, for completeness. The common ones:

| Flag | Meaning |
| :--- | :--- |
| `--listen <ADDR>` | Bind address. Non-loopback binds are permitted but warn. |
| `--token <TOKEN>` | Shared secret. Required unless `--allow-unauthenticated`. |
| `--workdir <DIR>` | Workspace root. |
| `--max-connections <N>` | Concurrent connection cap. Default `32`. |
| `--min-disk-gb <N>` | Run emergency GC below this much free space. |
| `--workspace-ttl-days <N>` | Expire idle branch workspaces after this long. |
| `--metrics-port <PORT>` | Expose Prometheus metrics. |

`fhd` refuses to start on a documentation placeholder token, and refuses to
start without a token at all unless `--allow-unauthenticated` is passed.

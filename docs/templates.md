# Templates

A template is a small YAML file that tells Farhand how to build one kind of
project: which files mean "this is a Rust project", what to fetch back when the
build succeeds, what to ignore, and which command installs dependencies.

Farhand ships six built-in templates. You almost never need to write one — you
only override a built-in when a project diverges from the default.

## Built-in templates

| Name | Detected by | Dependencies via | Fetches back |
| :--- | :--- | :--- | :--- |
| `rust` | `Cargo.toml` | `cargo fetch` | `target/release`, `target/debug` |
| `npm` | `package.json` | npm / yarn / pnpm | `dist`, `build` |
| `python` | `pyproject.toml`, `setup.py`, … | pip / poetry | `dist`, `build` |
| `go` | `go.mod` | `go mod download` | the built binary |
| `maven` | `pom.xml` | Maven | `target/*.jar` |
| `gradle` | `build.gradle`, `build.gradle.kts` | Gradle | `build/libs` |

See the exact contents of any of them:

```bash
fh templates show rust
```

## How a template is chosen

Every template declares `match` conditions. All matching templates are applied,
not just the first — so a monorepo containing both a `rust` and an `npm`
subproject gets both sets of outputs and ignore rules.

```yaml
match:
  anyFile:      # at least one of these must exist
    - Cargo.toml
  allFiles:     # every one of these must exist
    - Cargo.toml
    - rust-toolchain.toml
```

A template with neither `anyFile` nor `allFiles` never auto-matches. That is
deliberate — it forces you to select it explicitly with `--template` rather
than silently applying rules to every project.

Patterns support `*`, `?`, and `[...]`; a pattern with no wildcard is a plain
existence check.

## Resolution order

Later sources win when names collide:

```
project  .farhand/templates/<name>.yaml     ← highest priority
user     ~/.farhand/templates/<name>.yaml
builtin  compiled into the binary            ← lowest priority
```

So you can shadow the built-in `rust` template for one project by dropping a
`rust.yaml` into that project's `.farhand/templates/`, without affecting anyone
else or touching `~/.farhand`. Check what you are actually getting:

```console
$ fh templates list
NAME         SOURCE     DESCRIPTION
------------ ---------- ----------------------------------------
go           builtin    Go projects using Go modules
npm          builtin    Node.js and JavaScript/TypeScript projects
python       builtin    Python projects using pip, poetry, or setup.py
rust         project    Rust projects built with Cargo      ← shadowed
```

## `fh templates` commands

### `list` — what is available

```bash
fh templates list
```

Prints every visible template with the source it resolved from and its
description. This is the first thing to run when a build behaves unexpectedly
— it answers "which rules are actually in effect?".

### `show` — the raw YAML

```bash
fh templates show rust
```

Prints the resolved definition as YAML, which is the best starting point for
writing your own.

### `init` — scaffold a template

```bash
fh templates init monorepo
```

Writes `.farhand/templates/monorepo.yaml` in the current project with a
minimal skeleton. Edit it to taste; the name inside the file is what the rest
of the system uses, so keep it consistent with the filename.

### `push` — share it with the agent

```bash
fh templates push monorepo              # project scope (default)
fh templates push monorepo --scope user # applies to every project
```

Uploads a template to the agent host, where it is written into the workspace's
`.farhand/templates/`. Use `--scope user` to make it available across all
projects on that agent without repeating it per repository. Requires a
reachable agent — it is the one `templates` subcommand that talks to the
network.

## Writing a template

A complete example:

```yaml
name: monorepo
description: Custom monorepo build toolchain

match:
  anyFile:
    - monorepo.json

# Fetched back after a successful (exit 0) run.
outputs:
  - dist

# Globs matched against each resolved output, to keep huge subtrees
# (dependency directories, build intermediates) from being copied down.
outputsIgnore:
  - "*/deps"
  - "*/deps/*"
  - "*/.cache"
  - "*.map"

# Never sync these paths from the client in the first place.
ignoreExtra:
  - .cache
  - tmp

# Dependency installation, run on the agent before your command.
hints:
  installCommand: pnpm install --frozen-lockfile
  lockfiles:
    - pnpm-lock.yaml
```

### Fields

| Field | Meaning |
| :--- | :--- |
| `name` | **Required.** Identifier; also the filename stem. |
| `description` | Shown by `fh templates list`. |
| `match.anyFile` | At least one pattern must exist for this template to apply. |
| `match.allFiles` | Every pattern must exist. |
| `outputs` | Paths or globs to retrieve after exit code `0`. |
| `outputsIgnore` | Globs excluded from each output. Checked against the full path *and* the bare filename. |
| `ignoreExtra` | Additional ignore rules layered onto `.gitignore` and the built-in defaults, applied during the delta sync. |
| `hints.installCommand` | Run on the agent before your command, when the lockfile hash changes. |
| `hints.lockfiles` | Files whose combined SHA-256 decides whether to re-run `installCommand`. |

`outputs` and `hints` are optional; a template that only sets `match` and
`ignoreExtra` is perfectly valid.

### `outputs` vs `outputsIgnore` vs `ignoreExtra`

These are three different stages and mixing them up is the usual cause of "why
didn't my artifact come back?":

- **`ignoreExtra`** runs on the way *in*. Matching paths are never uploaded, so
  they never occupy the workspace.
- **`outputs`** selects what comes back on exit code `0`.
- **`outputsIgnore`** trims what comes back — use it to avoid dragging
  `target/debug/deps` or sourcemaps to your laptop.

A file that is `ignoreExtra`d is not on the agent at all, so naming it in
`outputs` will find nothing. It is ignored going in, not coming back.

## Dependency installation

`hints.installCommand` runs on the agent before your command, and only when the
hash of the files listed in `hints.lockfiles` has changed. That is what keeps
`npm install` or `cargo fetch` from re-running on every single build — a clean
second run should be a no-op.

```yaml
hints:
  installCommand: cargo fetch
  lockfiles:
    - Cargo.lock
```

`fh` never invokes this locally; it is agent-side, so the toolchain that
installs dependencies is the one on the build box.

Override per run with `--no-cache` to skip it entirely.

## See also

- [Configuration Guide](configuration.md) — the `.farhand.yaml` reference,
  including `template:` to force a specific template
- [Storage & Caching](storage-and-caching.md) — why dependencies survive
  between runs
- [Port Forwarding](port-forwarding.md) — reaching a dev server the agent ran

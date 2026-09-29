# Ignore Rules

What gets synced to the build box is decided by a small, predictable set of
rules. Nothing is hidden, and you can always ask.

## Order of application

A path is synced unless something below excludes it. Rules accumulate from
four sources:

```
1. built-in defaults        always applied, cannot be turned off
2. .gitignore                your normal ignore rules, honoured
3. .farhand-ignore           Farhand-specific additions
4. template ignoreExtra      per-language extras, from the matched template
```

Later sources are added to the same rule set rather than replacing it, so a
`.farhand-ignore` entry narrows what `.gitignore` allows. Within one file the
**last matching rule wins**, exactly as in `gitignore` — which is what makes
negation work.

## Built-in defaults

These are always ignored, whatever your files say:

| Path | Why |
| :--- | :--- |
| `.git` | Version control metadata |
| `node_modules` | Dependencies — the agent runs its own install hook |
| `target` | Rust build output |
| `dist`, `build`, `out`, `.next` | Build output |
| `__pycache__`, `.venv`, `venv` | Python build output and virtualenvs |
| `vendor` | Vendored dependencies |
| `.DS_Store` | macOS cruft |
| `.farhand-state.json`, `.farhand-runs`, `.farhand-last-used` | Agent bookkeeping |

You cannot un-ignore these. The dependency directories in particular are
excluded on purpose: the agent keeps its own `node_modules` and `target`
between builds, and syncing yours would overwrite them and destroy the cache
that makes a warm build fast.

If your project genuinely needs one of these transferred, the supported route
is a template that lists it under `outputs` — artifact retrieval is a separate
stage (see
[Templates](templates.md#outputs-vs-outputsignore-vs-ignoreextra)).

## `.gitignore` is honoured

You do not have to maintain a second ignore file for things you already
exclude from git. Farhand reads the project's root `.gitignore` directly.

```gitignore
# .gitignore
*.log
!important.log
```

## `.farhand-ignore`

For rules that are about the *build box* rather than about version control —
paths that should never leave your machine, or that are large and regenerable
but not git-ignored:

```
# .farhand-ignore
.env.local
secrets/
fixtures/large-fixtures/
```

Use it when a path must not be transferred but has no business in
`.gitignore`, or when you want the rule to be obviously local.

## Negation

`!` re-includes a path, same as in `gitignore`. Because the last matching rule
wins, put the negation *after* the rule it overrides:

```gitignore
*.log
!important.log
```

## Templates add per-language rules

A matched template contributes an `ignoreExtra` list — the built-in `rust`
template adds `target`, for example. These stack with your files rather than
replacing them.

Because template rules are layered last, a template's `ignoreExtra` cannot be
undone by a negation in your own ignore files. In watch mode this is
load-bearing: a negated `target` in a template would make Farhand rebuild on
its own compiler output forever, so the invariant is pinned by a test.

## Asking why

Never guess. `fh why` answers for a single path:

```console
$ fh why src/main.rs
src/main.rs
  will be uploaded — 1.2 KiB is not on the agent

$ fh why build.log
build.log
  excluded — ignore rule: *.log

$ fh why node_modules/react/index.js
node_modules/react/index.js
  excluded — built-in ignore: node_modules/
```

`fh why` consults the same matcher the sync itself uses, so its answer is the
real one rather than a re-derivation that can drift from it.

For the whole transfer, without sending anything:

```bash
fh sync --dry-run   # how many files and bytes
fh sync --list      # and every path by name
```

## Common situations

**A build output keeps getting transferred.**
Add it to `.farhand-ignore` if a built-in default does not already cover it, or
check whether a template's `outputs` is pulling it back rather than ignoring
it. `fh why <path>` will name the rule that decided.

**A file I want is excluded.**
Check ordering first: a negation placed *before* the rule it overrides does
nothing, because the last match wins. If the path is a built-in default
(`node_modules`, `target`, …) it cannot be un-ignored at all.

**My `.gitignore` is not being applied.**
Only the project-root `.gitignore` is read — nested `.gitignore` files are not
walked. Move the rule to the root, or list it in `.farhand-ignore`.

**I want to see the effective rule set.**
`fh templates show <name>` prints the template including its `ignoreExtra`, and
`fh sync --list` shows what the combined result actually does.

## See also

- [Templates](templates.md) — `ignoreExtra`, `outputs`, and `outputsIgnore`
- [Configuration Guide](configuration.md) — the rest of the project config
- [Architecture](architecture.md#3-section-51-deletion-safety-rule) — what
  happens to files on the agent that are no longer sent

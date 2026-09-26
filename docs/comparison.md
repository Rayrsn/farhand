# How farhand compares

Most comparisons in this space are written against a strawman — "ssh + rsync
scripts" — and quietly avoid the tools people actually use. This one does the
opposite: it names the real alternatives, and it is explicit about where they
are the better choice.

The short version: **these tools solve different problems.** Mutagen keeps two
directories in sync. Remote-SSH moves your whole IDE to the server.
`cargo-remote` offloads one command. Farhand offloads *execution* while you
keep working locally.

## The real alternatives

| | **farhand** | **mutagen** | **Remote-SSH** (VS Code, Zed, etc.) | **cargo-remote** | **ssh + rsync** |
| :--- | :--- | :--- | :--- | :--- | :--- |
| **What it is** | remote build/test runner | bidirectional realtime file sync | remote development environment | remote `cargo build` | DIY scripts |
| **Direction** | one-way, local → remote | **two-way, realtime** | two-way (edits happen remote) | artifacts back, source up | whatever you script |
| **Edit location** | local | either side | **remote** | local | local |
| **Languages** | any (templated commands) | any file | any (whatever the server has) | **Rust only** | any |
| **Delta transfer** | SHA-256 manifests | content-hash sync | VS Code's own | full source, then `target/` back | rsync's delta |
| **Wire compression** | zstd/gzip/none, negotiated | yes | yes | rsync-ish | gzip if you script it |
| **Deduplication** | **global CAS** | per-file dedupe | no | no | no |
| **Remote dependency cache** | preserved (never wiped) | n/a | preserved | preserved | depends on your script |
| **Runs the build** | **yes** | no | yes (it is a remote machine) | yes | yes, if you script it |
| **Install on the server** | `fhd` daemon | `mutagen` | VS Code Server | `cargo-remote` agent | `sshd` (+ `rsync` locally) |
| **Artifacts back** | `--out-dir` | n/a | n/a | `target/` only | whatever you script |

## Where the other tools are genuinely better

Stating this plainly is the point of the document.

**Mutagen is a better sync engine.** It is bidirectional and realtime, with
mature conflict handling, filesystem watching on both ends, and years of
production use. If your workflow is "edit on the laptop, edit on the desktop,
and never think about it again", mutagen is the right tool and farhand is not
a substitute. Farhand is deliberately one-way: it moves local source *to* the
agent and never writes back into your working tree, which is a safety property
rather than a limitation — but it also means farhand cannot do what mutagen
does.

**Remote-SSH gives you a full remote development environment.** Extensions,
debugger, terminal, and file explorer all run on the server. If your builds are
slow because the *edit–compile–debug* loop needs to be on the machine with the
code, moving the IDE is the more complete answer. Farhand keeps your editor,
your extensions, and your local filesystem exactly where they are and offloads
only the build — which is the better trade when you want a local editing
experience and a beefy remote compiler, and the worse one when you want the
whole environment remote.

**`cargo-remote` is smaller and does one thing.** For a Rust-only team that
just wants `cargo build` to run elsewhere, it is a much smaller concept: one
binary, one flag, no daemon to supervise. Farhand's daemon, protocol, workspace
layer, and templates are real machinery that you have to understand. If all you
need is one command offloaded, that machinery is overhead.

**`ssh` + `rsync` needs nothing installed on the server** and you already know
how it works. Farhand's advantage over a good rsync invocation is deduplication,
compression negotiation, and a persistent workspace — real, but only worth the
machinery if you sync often enough to feel them.

## Where farhand is better

**Content-addressed deduplication across projects.** A file already on the
agent, in any workspace, for any project, is never re-sent. A per-workspace
copy — what rsync, mutagen, and cargo-remote all give you — re-transfers it
per project, and after a branch switch.

**Copy-on-write branch workspaces.** Creating a workspace for a new branch
clones rather than copies: on APFS and on reflink filesystems (Btrfs, XFS, ZFS)
this is sub-100ms and costs no additional blocks, where every other tool
duplicates the tree.

**A dependency cache that survives.** Remote `node_modules/`, `target/`, and
`.venv/` are never wiped by a sync, and deletion safety is enforced by tests —
the workspace only ever removes files the manifest says are gone, and never
touches ignored directories.

**Zero external binaries.** `fh` and `fhd` are self-contained static builds:
no `ssh`, `rsync`, `tar`, or `gzip` on the server, and no version skew between
a local tool and a remote one.

**It is not tied to one language or one IDE.** Templated commands, per-project
configuration, and a language-agnostic wire format mean the same setup works
for a Go service and a TypeScript monorepo, without a plugin per language.

## Choosing

- You edit in one place and want the *build* elsewhere → **farhand**
- You edit in two places and want the *files* to follow you → **mutagen**
- You want your editor, extensions, and debugger on the server → **Remote-SSH**
- You only need `cargo build` offloaded, Rust only, minimal machinery →
  **cargo-remote**
- You sync a handful of times a day and want zero moving parts → **`rsync`**

They compose, too: mutagen for two-way file sync, farhand for offloading the
build. Nothing about farhand requires you to give up another tool.

## A note on maturity

Farhand is a young project and it shows. Two honest examples from this
repository's own history:

- Windows support is recent. The client had a stack overflow that made it
  unusable there; it is fixed and now gated in CI by a job that runs the
  binaries under a 1 MiB stack, but the ecosystem around it is thin.
- Adoption is small, so the bug reports are few — which cuts both ways: less
  battle-testing, and a shorter list of people who have already hit what you
  are about to hit.

If that matters for your use, pick the boring option. If it does not, farhand
does things the alternatives do not.

# Migrate from `rsync` + `ssh`

The usual reason people reach for `rsync --exclude` plus `ssh` is to avoid
re-uploading a project for every remote build. Farhand does the same job
without the shell script, and remembers what the agent already has.

## The script this replaces

A very common pattern, in one form or another:

```bash
#!/bin/sh
# build-remote.sh
set -e
HOST=builder@mac-mini.local
DIR=~/src/my-app

rsync -az --delete \
  --exclude '.git' --exclude 'node_modules' --exclude 'target' \
  ./ "$HOST:$DIR/"

ssh "$HOST" "cd $DIR && $@"
```

Two problems with it, both of which Farhand removes:

- **`--delete` fights the remote cache.** The agent's `node_modules` and
  `target` are exactly what you excluded, so they survive — but anything
  else on the remote is destroyed on every run, and a build that fails
  halfway leaves the remote tree in a state you have to reason about.
- **There is no record of what the remote has.** `--checksum` would be
  correct but slow; without it `rsync` falls back to size and mtime, which
  is the same approximation, implemented in shell you now maintain.

## The replacement

```bash
fh cargo build --release
```

That is the whole migration for the common case. The differences worth
knowing:

| With rsync | With Farhand |
| :--- | :--- |
| `rsync -az --delete --exclude ...` | `fh` (nothing to maintain) |
| `ssh host "cd dir && cmd"` | the command is the argument |
| ignore rules duplicated between the two | one rule set: `.gitignore`, `.farhand-ignore`, and the [template](../docs/ignores.md) |
| second run re-reads everything | second run re-hashes once, then serves digests from an index |
| artifacts come back with `scp` | `outputs` in `.farhand.yaml` |
| a dev server needs a second `ssh -L` | `fh -L 3000:3000 -- npm run dev` |

## Step by step

**1. Install the agent on the box you were sshing into.**

```bash
fhd --listen 0.0.0.0:9876 --token "$(openssl rand -hex 32)" \
    --workdir /var/farhand/workspaces
```

Use a [systemd or launchd unit](../dist/services/) if it is to run unattended;
both take the token from an environment file so it is not in the unit.

**2. Point the project at it.** Drop a `.farhand.yaml` in the repo:

```yaml
host: "mac-mini.local:9876"
token: "${FARHAND_TOKEN}"
outputs:
  - target/release/my-app
```

**3. Keep your existing ignore file.** `.gitignore` is honoured as-is. If
you had exclusions only in the `rsync` command line, move them to
`.farhand-ignore`:

```
# .farhand-ignore
.env.local
coverage/
```

**4. Delete the script.** Replace `sh build-remote.sh cargo build` with
`fh cargo build`.

**5. When you used `scp` for artifacts,** list them under `outputs` instead.
`--out-dir` controls where they land; the default is `./farhand-out`.

## Keeping the `ssh -L` you already have

If you run a dev server remotely today, you have a background `ssh -L` you
have to remember. The same mapping, no second process:

```bash
fh -L 3000:3000 -- npm run dev
```

The agent only relays to ports you allow it to — it refuses by default, so
opt in on the agent side:

```bash
fhd --forward-allow 3000 ...
```

## Things that will feel different

- **Branches get separate workspaces.** That is usually what you want, and it
  is why a dependency install is not repeated for every branch switch. Use
  `fh clean` to reclaim space, and `fh clean --all-branches` when you have
  been experimenting.
- **Artifacts arrive after a successful build only** — exit code `0`. That
  matches what your script did implicitly, but it is now explicit.
- **There is a token to manage.** `rsync` relied on your SSH keys. The token
  is the same class of secret and wants the same care.

## If something is not being transferred

Ask, rather than guess:

```bash
fh why path/to/file     # uploaded, already present, or ignored — and by what
fh sync --dry-run       # what a transfer would move, sending nothing
fh sync --list          # every path by name
```

`fh why` names the rule that decided, which is usually faster than bisecting
an `rsync --exclude`.

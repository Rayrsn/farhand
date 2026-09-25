---
name: Bug report
about: Report something that doesn't work as expected
title: ''
labels: bug
assignees: ''
---

**Description**

A clear and concise description of the bug.

**To Reproduce**

Steps to reproduce the behavior:

```bash
# commands you ran, e.g.
fhd --listen 0.0.0.0:9876 --token "$FARHAND_TOKEN" --workdir ~/farhand
fh cargo build --release
```

**Expected behavior**

What you expected to happen.

**Actual behavior**

What actually happened (exit code, truncated logs, missing artifacts…).

**Environment**

- OS / version:
- Architecture:
- `fh --version`:
- `fhd --version`:
- Transport: raw TCP / `--tls` / `ssh -L` / `cloudflared`:
- Project type (npm/rust/go/python/…) and `template` used:

**Logs**

Attach output from `fh --verbose` (redact tokens and secrets first).
For daemon-side issues, `fhd` logs / `--log-format json` output help a lot.

**Additional context**

Anything else — workspace layout, `.farhand.yaml` (redacted), network setup.
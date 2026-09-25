# Security Policy

## Supported Versions

Farhand is a fast-moving project. Only the latest tagged release receives
security updates.

| Version | Supported |
| :--- | :--- |
| 1.7.x | ✅ |
| < 1.7 | ❌ (upgrade) |

## Reporting a Vulnerability

**Please do not open a public GitHub issue for security reports.**

Use GitHub's [private vulnerability reporting](https://github.com/Rayrsn/farhand/security/advisories/new)
(Repo → Security → Report a vulnerability). Reports are triaged within 72 hours.

When reporting, include:

1. `fh` / `fhd` version (`fh --version`)
2. OS, architecture, and whether TLS (`--tls`) was in use
3. A minimal reproduction (commands, config, network topology)
4. Impact assessment from your perspective

## Threat Model & Deployment Guidance

`fhd` is a daemon that **executes arbitrary commands** sent by an authenticated
client. Treat it like you would treat `sshd` with password auth:

- **Always set a token.** `fhd` refuses nothing by default if `--token` is
  omitted — an unauthenticated listener is remote code execution by design.
- **Transport**: use `--tls` (with fingerprint pinning or mTLS), or front the
  port with a tunnel (`ssh -L`, `cloudflared access tcp`, Tailscale/WireGuard).
  Without TLS, tokens and all traffic are transmitted in cleartext.
- **Network**: bind `fhd` to a loopback/private interface and reach it via
  tunneling where possible. Do not expose port 9876 to the public internet.
- **Shared hosts**: any client holding the token can access *every* project
  workspace on the agent. Do not share one daemon across mutually untrusting
  users.
- **Environment**: `fh` forwards your local environment by default (with a
  denylist of common secret variables). Prefer `--no-env` plus explicit
  `-e VAR=...` when working with sensitive ambient credentials.

## Disclosure Policy

- We will acknowledge, triage, and publish fixes with coordinated release notes.
- Security fixes are released as patch versions with a `security:` changelog entry.
- Credit is given to reporters by default unless anonymity is requested.
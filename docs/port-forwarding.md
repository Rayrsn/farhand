# Reverse Port Forwarding

Run a service **on the build box** and reach it from your laptop as if it were
local. No `ssh -L`, no tunnel to remember, nothing left running on your
machine when you close the laptop.

```bash
# `npm run dev` runs on the build box. Open http://localhost:3000 here.
fh -L 3000:3000 -- npm run dev
```

That is the whole feature. `LOCAL:REMOTE` opens a listener on your loopback
interface and splices it into the same multiplexed connection that is already
carrying your build's logs and file deltas — so it is one TCP connection, one
auth handshake, and zero extra daemons on either side.

## Why it exists

Compiling on a remote machine means the dev server runs there too. Without
this you have three bad options: keep an `ssh -L` in a second terminal, deploy
the build somewhere, or stop using the remote box for development. All three
cost you the thing you wanted — editing locally while the heavy lifting
happens elsewhere.

```bash
# Today
ssh -N -L 3000:localhost:3000 builder@mac-mini.local &   # in another terminal
fh npm run dev
```

```bash
# With Farhand
fh -L 3000:3000 -- npm run dev
```

## What it is not

Being precise about the boundaries saves a lot of confusion:

- **The listener is loopback-only.** It binds `127.0.0.1`, so it is reachable
  from your machine and nowhere else. This does not publish your build box to
  the network.
- **It lives as long as the command.** The forward exists only while the remote
  command runs. Stop the build, and the listener closes with it.
- **It is not a public tunnel.** Farhand makes no inbound connection, opens no
  port on the agent's firewall, and needs no inbound connectivity to the build
  box. The agent already had to be reachable for the build; this rides along.
- **`REMOTE` is a port on the agent's loopback.** The agent connects to
  `127.0.0.1:REMOTE`, so the service must be listening there. A dev server
  bound to a specific interface rather than loopback will not be reached.

## Multiple ports

`-L` repeats. Each one is an independent listener with its own mapping, so you
can bring up a whole stack in a single command:

```bash
fh -L 3000:3000 -L 8080:8080 -L 5432:5432 -- npm run dev
```

```
[Port Forward] Listening on 127.0.0.1:3000 -> remote:3000
[Port Forward] Listening on 127.0.0.1:8080 -> remote:8080
[Port Forward] Listening on 127.0.0.1:5432 -> remote:5432
```

The two ports need not match. `LOCAL` is the port you open locally, `REMOTE` is
the port the service uses on the agent. This is how you dodge a port you
already have bound:

```bash
# Something is already on 3000 locally — take 4000 instead.
fh -L 4000:3000 -- npm run dev
# then browse http://localhost:4000
```

## Putting it in `.farhand.yaml`

If you forward the same ports for every run, declare them once. The `forward`
key takes the same `LOCAL:REMOTE` strings:

```yaml
# .farhand.yaml
host: "mac-mini.local:9876"
token: "${FARHAND_TOKEN}"

forward:
  - "3000:3000"   # Vite / Next dev server
  - "5432:5432"   # a database on the build box
```

Then a plain `fh npm run dev` brings the tunnels up with no extra flags.

`-L` on the command line **replaces** the configured list rather than adding to
it, so a one-off override does not silently stack on top of your defaults:

```bash
# Uses only 4000:3000, ignoring the configured 3000:3000 and 5432:5432.
fh -L 4000:3000 -- npm run dev
```

Malformed entries are rejected before anything connects, and the error names
the source so you know which one to fix:

```
Error: invalid forward '3000' from .farhand.yaml: expected LOCAL:REMOTE (e.g. 3000:3000)
Error: invalid forward '70000:3000' from --forward: invalid local port '70000'
```

## What travels over it

An ordinary bidirectional TCP stream: request bodies, response bodies,
WebSocket frames, `Authorization` headers. It is multiplexed over the build
connection as length-prefixed frames, so there is no HTTP-aware rewriting and
nothing to configure per protocol.

```
laptop                                   build box
  │                                         │
  │  open 127.0.0.1:3000                    │
  ├──────── PortOpen ───────────────────────►│
  │                                         ├─ connect 127.0.0.1:3000
  │◄─────── PortData ◄───────────────────────┤
  │◄─────── PortData ◄───────────────────────┤   your dev server's response
  │──────── PortData ───────────────────────►│   your browser's request body
  │──────── PortClose ──────────────────────►│
```

## Interaction with watch mode

`fh watch` accepts `-L` the same way, and re-establishes the forward on every
rebuild. You keep one address for a whole editing session instead of
re-running an `ssh -L` per build:

```bash
fh -L 3000:3000 watch -- npm run dev
```

The listener is rebound at the start of each build rather than held open
continuously, so there is a brief moment between rebuilds where the local port
is closed. In practice this is invisible, but a client that holds a connection
across a rebuild may need to reconnect.

## Troubleshooting

**The listener prints but nothing connects.** Check the service is actually
listening on the agent's *loopback*. `fhd` connects to `127.0.0.1:REMOTE`, so
a dev server bound only to a LAN interface is not reachable this way.

**`Address already in use`.** Something local holds `LOCAL`. Either stop it or
pick another port — which is what the two-sided mapping is for.

**`Connection refused` on a mapped port.** Usually the remote service is not up
yet. If it is a dev server, give it a moment after the build finishes; the
forward is established whether or not the service is listening.

**It works locally but not remotely.** Compare with an `ssh -L` on the same
mapping. If that fails too, the problem is the service binding loopback only,
not Farhand.

## See also

- [Configuration Guide](configuration.md) — the full `.farhand.yaml` reference
- [Cloudflare Tunnel Guide](cloudflared-tunnel.md) — for exposing a build box
  to the internet, which is a different problem from reaching a dev server on it
- [Apple Silicon Build Server Setup](mac-build-server-setup.md) — running the
  agent as a long-lived service

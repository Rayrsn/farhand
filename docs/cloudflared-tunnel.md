# Remote Access via Cloudflare Tunnel (`cloudflared`)

This guide explains how to connect your local `fh` client to a remote build server (`fhd`) across the internet using **Cloudflare Tunnel (`cloudflared`)**.

Cloudflare Tunnel provides:
- **No Open Inbound Ports / Port Forwarding**: The Mac Mini creates an outbound tunnel to Cloudflare Edge.
- **End-to-End Encryption**: All traffic is encrypted over TLS.
- **Zero Trust Security**: Access can optionally be restricted to your Cloudflare Access account or service tokens.
- **Static Domain Access**: Connect via `build.yourdomain.com` without needing a static public IP.

```
┌─────────────────┐       TCP stream       ┌──────────────────┐
│  Laptop (fh)    │ ─────────────────────> │ cloudflared      │ (127.0.0.1:9876)
└─────────────────┘                        └─────────┬────────┘
                                                     │ Outbound TLS
                                                     ▼
                                           ┌──────────────────┐
                                           │ Cloudflare Edge  │
                                           └─────────┬────────┘
                                                     │ Outbound Tunnel
                                                     ▼
┌─────────────────┐       Raw TCP          ┌──────────────────┐
│  Mac Mini (fhd) │ <───────────────────── │ cloudflared      │
└─────────────────┘       port 9876        └──────────────────┘
```

---

## Architecture Overview

Farhand uses raw framed TCP over port `9876`. Cloudflare Tunnel supports arbitrary TCP traffic routing through its **TCP Ingress** and client-side **`cloudflared access tcp`** proxy:

1. The **server** (`fhd` host) runs `cloudflared tunnel run` exposing `tcp://localhost:9876`.
2. The **client** (your laptop) runs `cloudflared access tcp` binding a local port (e.g. `127.0.0.1:9876`) to your Cloudflare hostname.
3. `fh` connects to `127.0.0.1:9876`.

---

## 1. Server-Side Setup (Mac Mini Build Agent)

### Step 1.1: Install `cloudflared` on the Mac Mini
```bash
# Can be run directly on the Mac Mini or remotely via fh:
brew install cloudflared
```

### Step 1.2: Authenticate Cloudflare
```bash
cloudflared tunnel login
```
This opens a browser window to select your Cloudflare domain. It downloads a certificate to `~/.cloudflared/cert.pem`.

### Step 1.3: Create a Tunnel
```bash
cloudflared tunnel create farhand-build
```
This outputs a Tunnel ID (e.g., `a1b2c3d4-e5f6-7890-abcd-ef1234567890`).

### Step 1.4: Create the Tunnel Configuration File
Create `~/.cloudflared/config.yml`:

```yaml
tunnel: a1b2c3d4-e5f6-7890-abcd-ef1234567890
credentials-file: /Users/builder/.cloudflared/a1b2c3d4-e5f6-7890-abcd-ef1234567890.json

ingress:
  # Route Farhand TCP traffic
  - hostname: build.yourdomain.com
    service: tcp://localhost:9876

  # Default catch-all rule (required by Cloudflare)
  - service: http_status:404
```

### Step 1.5: Route DNS Traffic to the Tunnel
```bash
cloudflared tunnel route dns farhand-build build.yourdomain.com
```

### Step 1.6: Run the Tunnel as a Background Service
To make the tunnel start automatically upon boot:
```bash
sudo cloudflared service install
```
Or start it manually in the background:
```bash
cloudflared tunnel run farhand-build &
```

---

## 2. Client-Side Setup (Your Local Machine / Laptop)

On your remote machine (where you run `fh`):

### Step 2.1: Install `cloudflared` Locally
- **macOS**: `brew install cloudflared`
- **Ubuntu / Debian**: `sudo apt install cloudflared`
- **Arch Linux**: `sudo pacman -S cloudflared`
- **Windows**: `winget install --id Cloudflare.cloudflared`

### Step 2.2: Start the TCP Access Proxy
Run this command in the background or in a terminal tab:
```bash
cloudflared access tcp --hostname build.yourdomain.com --url 127.0.0.1:9876
```
> [!TIP]
> You can run this automatically at login or wrap it in a systemd user unit / launch agent so `127.0.0.1:9876` is always ready when you work remotely.

---

## 3. Configure Farhand to Use the Tunnel

In your project's `.farhand.yaml`:

```yaml
# Point to the local cloudflared access proxy
host: "127.0.0.1:9876"

# Shared auth token
token: "${FARHAND_TOKEN}"

# Build artifact outputs
outputs:
  - target/release/fh
  - target/release/fhd

outDir: "."
```

Now run your remote commands as usual:
```bash
export FARHAND_TOKEN="your-secret-token"

# Run tests or builds remotely over Cloudflare Tunnel
fh -- cargo check
fh -- cargo build --release
```

---

## 4. Setting Up SSH Over Cloudflare Tunnel (The Easiest Workflow)

By routing SSH through Cloudflare Tunnel, you get:
1. **Remote Terminal Access**: `ssh mac-mini` works from anywhere in the world without exposing port 22 to the public internet.
2. **Automatic Farhand Forwarding**: Your SSH config can automatically forward port `9876` (`LocalForward 9876 localhost:9876`) whenever you connect, so `fh` works out of the box with zero extra daemons.

### Step 4.1: Ensure SSH is Enabled on the Mac Mini
SSH (Remote Login) is already enabled and listening on port 22. Your local key has also been added to `/Users/builder/.ssh/authorized_keys`.

### Step 4.2: Add SSH Ingress to `~/.cloudflared/config.yml` on the Mac Mini
In your Cloudflare Tunnel config on the Mac Mini, add `service: ssh://localhost:22`:

```yaml
tunnel: <TUNNEL_ID>
credentials-file: /Users/builder/.cloudflared/<TUNNEL_ID>.json

ingress:
  # SSH access over Cloudflare
  - hostname: mac.yourdomain.com
    service: ssh://localhost:22

  # Direct Farhand TCP daemon access
  - hostname: build.yourdomain.com
    service: tcp://localhost:9876

  - service: http_status:404
```

Route the hostname in DNS:
```bash
cloudflared tunnel route dns farhand-build mac.yourdomain.com
```

### Step 4.3: Configure `~/.ssh/config` on Your Local Machine

> [!TIP]
> **Automated One-Step Setup Script**:
> You can install all prerequisites (`cloudflared`, OpenSSH, `fh`) and configure your `~/.ssh/config` plus a convenient `fh-tunnel` background helper script automatically by running:
> - **Linux & macOS**:
>   ```bash
>   ./scripts/setup_remote_ssh.sh
>   # Or non-interactively:
>   ./scripts/setup_remote_ssh.sh --hostname mac.yourdomain.com --alias mac-mini --user builder --yes
>   ```
> - **Windows (PowerShell)**:
>   ```powershell
>   .\scripts\setup_remote_ssh.ps1
>   # Or non-interactively:
>   .\scripts\setup_remote_ssh.ps1 -Hostname mac.yourdomain.com -HostAlias mac-mini -RemoteUser builder -NonInteractive
>   ```

Alternatively, you can manually add the following block to your local `~/.ssh/config`:

```ssh-config
Host mac-mini
    HostName mac.yourdomain.com
    User builder
    IdentityFile ~/.ssh/main
    ProxyCommand cloudflared access ssh --hostname %h
    # Automatically forward Farhand daemon port 9876
    LocalForward 9876 localhost:9876
    ServerAliveInterval 60
    ServerAliveCountMax 3
```

### Step 4.4: Connect & Offload Builds

Now simply connect with:
```bash
ssh mac-mini
```
Or start a silent background forwarder when you start working:
```bash
ssh -N mac-mini &
```

While connected, your local `127.0.0.1:9876` tunnels securely through Cloudflare to the Mac Mini! In your `.farhand.yaml`:
```yaml
host: "127.0.0.1:9876"
token: "${FARHAND_TOKEN}"
```
Run builds as usual:
```bash
fh -- cargo build --release
```

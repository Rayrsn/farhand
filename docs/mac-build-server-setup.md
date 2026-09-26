# Apple Silicon Mac Build Server Setup Guide

This guide walks through configuring an **Apple Silicon Mac (M1/M2/M3/M4)** as a remote build server using Farhand.

---

## 1. Prerequisites

Log into your Mac as an administrative user.

### Step 1.1: Install Command Line Tools & Homebrew
```bash
# Install Apple Developer Command Line Tools
xcode-select --install

# Install Homebrew
/bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"

# Ensure Homebrew is active in your environment
echo 'eval "$(/opt/homebrew/bin/brew shellenv)"' >> ~/.zprofile
eval "$(/opt/homebrew/bin/brew shellenv)"
```

### Step 1.2: Install Compilers & Toolchains
Install whatever toolchains your team's repositories build with:
```bash
# Global compiler cache & git
brew install sccache git

# Language toolchains (customize for your tech stack)
brew install node pnpm go
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source "$HOME/.cargo/env"
```

---

## 2. Directory Structure & Permissions

Create dedicated root directories for persistent workspaces, shared caches, and daemon logs:

```bash
sudo mkdir -p /var/farhand/workspaces
sudo mkdir -p /var/farhand/cache/{sccache,npm,go-build,go}
sudo mkdir -p /var/log/farhand

# Set ownership to your user account
sudo chown -R $(whoami):staff /var/farhand /var/log/farhand
sudo chmod -R 775 /var/farhand
```

---

## 3. Install the Farhand Binaries

Install `fhd` (daemon) and `fh` (client):

```bash
# Compile and install via Cargo
# `cargo install` takes crate names, not binary names: the binaries are `fh`
# and `fhd`, but the published crates are `farhand-cli` and `farhand-agent`.
cargo install --git https://github.com/Rayrsn/farhand.git farhand-cli farhand-agent

# Ensure /usr/local/bin exists and copy binaries for global system access
sudo mkdir -p /usr/local/bin
sudo cp "$HOME/.cargo/bin/fhd" /usr/local/bin/
sudo cp "$HOME/.cargo/bin/fh" /usr/local/bin/

# Verify
fhd --help
fh --help
```

---

## 4. Configure the `launchd` Service Daemon

`launchd` runs `fhd` in the background, starts it automatically when macOS boots, and restarts it if it exits.

### Step 4.1: Generate a Secret Token
```bash
openssl rand -hex 24
# Save the printed token (e.g., 8c1596d9e5e7001e9ba74abe87bd7c9eec59dbbaf52c13b3)
```

### Step 4.2: Create `/Library/LaunchDaemons/com.farhand.fhd.plist`
```bash
sudo nano /Library/LaunchDaemons/com.farhand.fhd.plist
```

Paste the following configuration (replace `REPLACE_WITH_YOUR_TOKEN` with your generated token):

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.farhand.fhd</string>
    <key>ProgramArguments</key>
    <array>
        <string>/usr/local/bin/fhd</string>
        <string>--listen</string>
        <string>0.0.0.0:9876</string>
        <string>--workdir</string>
        <string>/var/farhand/workspaces</string>
        <string>--max-disk-gb</string>
        <string>100</string>
        <string>--workspace-ttl-days</string>
        <string>7</string>
        <string>--gc-interval-secs</string>
        <string>3600</string>
    </array>
    <key>EnvironmentVariables</key>
    <dict>
        <key>FARHAND_TOKEN</key>
        <string>REPLACE_WITH_YOUR_TOKEN</string>

        <!-- Command search paths for compilers and package managers -->
        <key>PATH</key>
        <string>/opt/homebrew/bin:/opt/homebrew/sbin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin</string>

        <!-- Global Cross-Workspace Shared Toolchain Caches -->
        <key>RUSTC_WRAPPER</key>
        <string>/opt/homebrew/bin/sccache</string>
        <key>SCCACHE_DIR</key>
        <string>/var/farhand/cache/sccache</string>
        <key>SCCACHE_CACHE_SIZE</key>
        <string>30G</string>

        <key>NPM_CONFIG_CACHE</key>
        <string>/var/farhand/cache/npm</string>

        <key>GOCACHE</key>
        <string>/var/farhand/cache/go-build</string>
        <key>GOPATH</key>
        <string>/var/farhand/cache/go</string>
    </dict>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>/var/log/farhand/fhd.log</string>
    <key>StandardErrorPath</key>
    <string>/var/log/farhand/fhd.err</string>
</dict>
</plist>
```

### Step 4.3: Load and Start the Daemon
```bash
sudo chown root:wheel /Library/LaunchDaemons/com.farhand.fhd.plist
sudo chmod 644 /Library/LaunchDaemons/com.farhand.fhd.plist

sudo launchctl load -w /Library/LaunchDaemons/com.farhand.fhd.plist
```

Verify that `fhd` is active and listening:
```bash
sudo lsof -i :9876
tail -n 20 /var/log/farhand/fhd.log
```

---

## 5. Network Access Setup

### Local LAN (In-Office / Same Wi-Fi)
1. Find your Mac's active IP address:
   ```bash
   ipconfig getifaddr $(route get default 2>/dev/null | awk '/interface:/{print $2}')
   # e.g., 192.168.254.68
   ```
   Or use the macOS Bonjour hostname:
   ```bash
   scutil --get LocalHostName
   # e.g., "Mac" -> connects via Mac.local:9876
   ```
2. Check macOS Firewall:
   Navigate to **System Settings > Network > Firewall** and ensure incoming connections on port `9876` are permitted.

### Remote Access (Work from Home)

#### Option 1: Tailscale (Recommended)
1. Install [Tailscale](https://tailscale.com) on the Mac and log in.
2. Install Tailscale on developer laptops.
3. Developers can connect directly to the Mac's Tailscale IP or MagicDNS hostname (e.g. `mac:9876`).

#### Option 2: SSH Port Forwarding
If developers have SSH access to the Mac:
```bash
# Add to ~/.ssh/config on developer machines:
Host build
    HostName your-public-ip-or-domain.com
    User mac-user
    LocalForward 9876 127.0.0.1:9876

# Establish tunnel in background:
ssh -N -f build
```
Developers can now target `127.0.0.1:9876`.

#### Option 3: Cloudflare Tunnel & Automated Setup (Zero Inbound Ports)
If the Mac is behind a home/office router without a public IP or port forwarding, route through **Cloudflare Tunnel**:
- Developers on **Linux & macOS**:
  ```bash
  curl -fsSL https://raw.githubusercontent.com/Rayrsn/farhand/main/scripts/setup_remote_ssh.sh | bash
  ```
- Developers on **Windows** (PowerShell):
  ```powershell
  irm https://raw.githubusercontent.com/Rayrsn/farhand/main/scripts/setup_remote_ssh.ps1 | iex
  ```
This automatically installs `cloudflared`, configures `~/.ssh/config`, and sets up the background `fh-tunnel` manager. See the [Remote Access via Cloudflare Tunnel Guide](cloudflared-tunnel.md) for full server-side instructions.

---

## 6. Seed Workspace Initialization

To give your team instant builds with 0-byte duplicate storage:
1. Run the initial build on `main` once:
   ```bash
   cd ~/projects/my-repo
   git checkout main
   fh --host 192.168.254.68:9876 --token YOUR_TOKEN "npm install && npm run build"
   ```
2. All dependencies are now seeded in `/var/farhand/workspaces/my-repo`.
3. Any developer branching off `main` will instantly fork the workspace via APFS `clonefile` in **< 100ms with 0-byte duplicate disk space**!

---

## 7. Server Monitoring & Maintenance

```bash
# Follow real-time daemon logs
tail -f /var/log/farhand/fhd.log

# Inspect disk space allocated across workspaces
du -sh /var/farhand/workspaces/*

# Restart the service after changing configuration
sudo launchctl unload -w /Library/LaunchDaemons/com.farhand.fhd.plist
sudo launchctl load -w /Library/LaunchDaemons/com.farhand.fhd.plist

# Check sccache shared compiler cache statistics
sccache --show-stats
```

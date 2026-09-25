#!/usr/bin/env bash
set -euo pipefail

# ==============================================================================
# Farhand (fh) Remote SSH & Cloudflare Tunnel Prerequisites Setup Script
#
# This script sets up all client-side prerequisites to offload builds to a
# remote Farhand daemon (fhd) over SSH routed through Cloudflare Tunnel.
# ==============================================================================

# Formatting and color helpers
BOLD="$(printf '\033[1m')"
GREEN="$(printf '\033[0;32m')"
BLUE="$(printf '\033[0;34m')"
YELLOW="$(printf '\033[0;33m')"
RED="$(printf '\033[0;31m')"
NC="$(printf '\033[0m')" # No Color

info() {
    printf "${BLUE}${BOLD}[INFO]${NC} %s\n" "$1"
}

success() {
    printf "${GREEN}${BOLD}[OK]${NC} %s\n" "$1"
}

warn() {
    printf "${YELLOW}${BOLD}[WARN]${NC} %s\n" "$1"
}

error() {
    printf "${RED}${BOLD}[ERROR]${NC} %s\n" "$1" >&2
}

# Default configuration values
HOST_ALIAS="farhand-remote"
REMOTE_HOSTNAME=""
REMOTE_USER="${USER:-builder}"
IDENTITY_FILE=""
LOCAL_PORT="9876"
REMOTE_PORT="9876"
NON_INTERACTIVE=false
INSTALL_FH=false

usage() {
    cat <<EOF
${BOLD}Farhand Remote SSH & Cloudflare Tunnel Setup${NC}

Installs and configures all prerequisites on your client machine to connect
to a remote Farhand daemon ('fhd') over SSH using Cloudflare Tunnel.

${BOLD}Usage:${NC}
  $(basename "$0") [options]

${BOLD}Options:${NC}
  -H, --hostname <domain>    Cloudflare hostname for remote host (e.g. mac.example.com)
  -a, --alias <name>         SSH Host alias in ~/.ssh/config (default: farhand-remote)
  -u, --user <username>      Remote SSH username (default: current user: ${REMOTE_USER})
  -k, --key <path>           Path to SSH private key (default: auto-detect ~/.ssh/id_*)
  -p, --port <port>          Local port to forward Farhand to (default: 9876)
  -r, --remote-port <port>   Remote Farhand daemon port (default: 9876)
  --install-fh               Install/update 'fh' client binary if not in PATH
  -y, --yes, --non-interactive
                             Run non-interactively without interactive prompts
  -h, --help                 Show this help message and exit

${BOLD}Examples:${NC}
  # Interactive setup:
  ./scripts/setup_remote_ssh.sh

  # Automated non-interactive setup:
  ./scripts/setup_remote_ssh.sh \\
      --hostname mac.mydomain.com \\
      --user builder \\
      --alias mac-mini \\
      --key ~/.ssh/id_ed25519 \\
      --non-interactive
EOF
    exit 0
}

# Parse command line flags
while [[ $# -gt 0 ]]; do
    case "$1" in
        -H|--hostname)
            REMOTE_HOSTNAME="$2"
            shift 2
            ;;
        -a|--alias)
            HOST_ALIAS="$2"
            shift 2
            ;;
        -u|--user)
            REMOTE_USER="$2"
            shift 2
            ;;
        -k|--key)
            IDENTITY_FILE="$2"
            shift 2
            ;;
        -p|--port)
            LOCAL_PORT="$2"
            shift 2
            ;;
        -r|--remote-port)
            REMOTE_PORT="$2"
            shift 2
            ;;
        --install-fh)
            INSTALL_FH=true
            shift
            ;;
        -y|--yes|--non-interactive)
            NON_INTERACTIVE=true
            shift
            ;;
        -h|--help)
            usage
            ;;
        *)
            error "Unknown option: $1"
            echo "Run '$(basename "$0") --help' for usage."
            exit 1
            ;;
    esac
done

printf "\n${BOLD}======================================================${NC}\n"
printf "${BOLD}   Farhand Remote SSH & Cloudflare Tunnel Setup       ${NC}\n"
printf "${BOLD}======================================================${NC}\n\n"

# ------------------------------------------------------------------------------
# 1. Detect Environment & Privileges
# ------------------------------------------------------------------------------
OS="$(uname -s | tr '[:upper:]' '[:lower:]')"
ARCH="$(uname -m)"

case "$ARCH" in
    x86_64|amd64) ARCH="amd64" ;;
    aarch64|arm64) ARCH="arm64" ;;
    *)
        error "Unsupported architecture: $ARCH"
        exit 1
        ;;
esac

SUDO=""
if [ "$(id -u)" -ne 0 ]; then
    if command -v sudo >/dev/null 2>&1; then
        SUDO="sudo"
    fi
fi

# Determine a user binary path (~/.local/bin or /usr/local/bin)
BIN_DIR="${HOME}/.local/bin"
if [ -w "/usr/local/bin" ]; then
    BIN_DIR="/usr/local/bin"
fi
mkdir -p "$BIN_DIR"

# ------------------------------------------------------------------------------
# 2. Check & Install cloudflared
# ------------------------------------------------------------------------------
install_cloudflared() {
    info "Installing 'cloudflared'..."
    if [ "$OS" = "darwin" ]; then
        if command -v brew >/dev/null 2>&1; then
            brew install cloudflared
        else
            info "Homebrew not found; downloading official macOS cloudflared binary..."
            TMP_CF="$(mktemp -d)"
            curl -fsSL "https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-darwin-${ARCH}.tgz" -o "${TMP_CF}/cloudflared.tgz"
            tar -xzf "${TMP_CF}/cloudflared.tgz" -C "${BIN_DIR}"
            rm -rf "${TMP_CF}"
            chmod +x "${BIN_DIR}/cloudflared"
        fi
    elif [ "$OS" = "linux" ]; then
        if command -v pacman >/dev/null 2>&1; then
            $SUDO pacman -Sy --noconfirm cloudflared
        elif command -v apt-get >/dev/null 2>&1; then
            info "Attempting to install cloudflared via apt..."
            if ! $SUDO apt-get update && $SUDO apt-get install -y cloudflared 2>/dev/null; then
                info "Package not in standard apt; downloading official Debian package..."
                DEB_TMP="$(mktemp -d)"
                curl -fsSL "https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-linux-${ARCH}.deb" -o "${DEB_TMP}/cloudflared.deb"
                $SUDO dpkg -i "${DEB_TMP}/cloudflared.deb" || $SUDO apt-get install -f -y
                rm -rf "${DEB_TMP}"
            fi
        elif command -v dnf >/dev/null 2>&1; then
            info "Installing cloudflared via dnf..."
            if ! $SUDO dnf install -y cloudflared 2>/dev/null; then
                $SUDO dnf install -y "https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-linux-${ARCH}.rpm"
            fi
        else
            info "Downloading standalone cloudflared binary..."
            curl -fsSL "https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-linux-${ARCH}" -o "${BIN_DIR}/cloudflared"
            chmod +x "${BIN_DIR}/cloudflared"
        fi
    else
        error "Unsupported operating system: $OS"
        exit 1
    fi
}

if command -v cloudflared >/dev/null 2>&1; then
    CF_VER="$(cloudflared --version 2>&1 | head -n 1)"
    success "'cloudflared' is already installed (${CF_VER})"
else
    install_cloudflared
    if command -v cloudflared >/dev/null 2>&1 || [ -x "${BIN_DIR}/cloudflared" ]; then
        success "'cloudflared' installed successfully."
    else
        error "Failed to verify 'cloudflared' installation."
        exit 1
    fi
fi

# ------------------------------------------------------------------------------
# 3. Check & Install OpenSSH Client
# ------------------------------------------------------------------------------
if command -v ssh >/dev/null 2>&1; then
    SSH_VER="$(ssh -V 2>&1 | head -n 1)"
    success "OpenSSH client is available (${SSH_VER})"
else
    info "Installing OpenSSH client..."
    if [ "$OS" = "darwin" ]; then
        error "OpenSSH client not found. Please install Xcode Command Line Tools: xcode-select --install"
        exit 1
    elif command -v pacman >/dev/null 2>&1; then
        $SUDO pacman -Sy --noconfirm openssh
    elif command -v apt-get >/dev/null 2>&1; then
        $SUDO apt-get update && $SUDO apt-get install -y openssh-client
    elif command -v dnf >/dev/null 2>&1; then
        $SUDO dnf install -y openssh-clients
    else
        error "Please install OpenSSH client using your system package manager."
        exit 1
    fi
fi

# ------------------------------------------------------------------------------
# 4. Check & Install 'fh' Client Binary
# ------------------------------------------------------------------------------
if command -v fh >/dev/null 2>&1; then
    FH_VER="$(fh --version 2>&1 | head -n 1 || true)"
    success "Farhand client 'fh' is installed (${FH_VER})"
else
    if [ "$INSTALL_FH" = "true" ] || [ "$NON_INTERACTIVE" = "false" ]; then
        INSTALL_FH_CONFIRMED=false
        if [ "$INSTALL_FH" = "true" ]; then
            INSTALL_FH_CONFIRMED=true
        else
            read -r -p "Farhand client 'fh' was not found in PATH. Install it now? [Y/n] " response || true
            response="${response:-y}"
            if [[ "$response" =~ ^[Yy]$ ]]; then
                INSTALL_FH_CONFIRMED=true
            fi
        fi

        if [ "$INSTALL_FH_CONFIRMED" = "true" ]; then
            SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
            if [ -f "${SCRIPT_DIR}/install.sh" ]; then
                info "Running local installer: ${SCRIPT_DIR}/install.sh"
                bash "${SCRIPT_DIR}/install.sh" --prefix "$BIN_DIR"
            else
                info "Downloading and running Farhand official installer..."
                curl -fsSL https://raw.githubusercontent.com/Rayrsn/farhand/main/scripts/install.sh | bash -s -- --prefix "$BIN_DIR"
            fi
        else
            warn "Skipping 'fh' installation. You will need 'fh' in your PATH to submit jobs."
        fi
    fi
fi

# ------------------------------------------------------------------------------
# 5. Interactive Configuration (if not non-interactive)
# ------------------------------------------------------------------------------
if [ "$NON_INTERACTIVE" = "false" ]; then
    echo ""
    info "Configuring SSH connection settings for Cloudflare Tunnel..."
    echo ""

    if [ -z "$REMOTE_HOSTNAME" ]; then
        while [ -z "$REMOTE_HOSTNAME" ]; do
            read -r -p "Enter Cloudflare hostname of remote server (e.g., mac.example.com): " REMOTE_HOSTNAME
            REMOTE_HOSTNAME="$(echo "$REMOTE_HOSTNAME" | xargs)"
        done
    fi

    read -r -p "Remote SSH username [${REMOTE_USER}]: " INPUT_USER || true
    REMOTE_USER="${INPUT_USER:-$REMOTE_USER}"

    read -r -p "SSH Host alias for ~/.ssh/config [${HOST_ALIAS}]: " INPUT_ALIAS || true
    HOST_ALIAS="${INPUT_ALIAS:-$HOST_ALIAS}"

    read -r -p "Local port to forward Farhand daemon to [${LOCAL_PORT}]: " INPUT_LPORT || true
    LOCAL_PORT="${INPUT_LPORT:-$LOCAL_PORT}"

    read -r -p "Remote Farhand daemon port [${REMOTE_PORT}]: " INPUT_RPORT || true
    REMOTE_PORT="${INPUT_RPORT:-$REMOTE_PORT}"
fi

if [ -z "$REMOTE_HOSTNAME" ]; then
    error "Remote Cloudflare hostname is required (specify via --hostname <domain>)."
    exit 1
fi

# ------------------------------------------------------------------------------
# 6. SSH Identity Key Resolution & Generation
# ------------------------------------------------------------------------------
SSH_DIR="${HOME}/.ssh"
mkdir -p "$SSH_DIR"
chmod 700 "$SSH_DIR"

if [ -z "$IDENTITY_FILE" ]; then
    # Look for standard candidate keys
    for candidate in "${SSH_DIR}/id_ed25519" "${SSH_DIR}/id_rsa" "${SSH_DIR}/id_ecdsa" "${SSH_DIR}/main"; do
        if [ -f "$candidate" ]; then
            IDENTITY_FILE="$candidate"
            break
        fi
    done

    if [ -z "$IDENTITY_FILE" ]; then
        DEFAULT_NEW_KEY="${SSH_DIR}/id_ed25519"
        if [ "$NON_INTERACTIVE" = "true" ]; then
            info "No SSH key found; generating default key at ${DEFAULT_NEW_KEY}..."
            ssh-keygen -t ed25519 -f "$DEFAULT_NEW_KEY" -N "" -C "farhand-client"
            IDENTITY_FILE="$DEFAULT_NEW_KEY"
        else
            read -r -p "No existing SSH key detected. Generate a new ed25519 key at ${DEFAULT_NEW_KEY}? [Y/n] " gen_resp || true
            gen_resp="${gen_resp:-y}"
            if [[ "$gen_resp" =~ ^[Yy]$ ]]; then
                ssh-keygen -t ed25519 -f "$DEFAULT_NEW_KEY" -N "" -C "farhand-client"
                IDENTITY_FILE="$DEFAULT_NEW_KEY"
            else
                read -r -p "Enter path to your SSH private key: " IDENTITY_FILE
                IDENTITY_FILE="$(eval echo "$IDENTITY_FILE")"
            fi
        fi
    fi
else
    # Expand tilde if present
    IDENTITY_FILE="$(eval echo "$IDENTITY_FILE")"
fi

if [ ! -f "$IDENTITY_FILE" ]; then
    warn "Specified identity file '${IDENTITY_FILE}' does not exist on disk."
fi

# Display public key so user can authorize it on the remote machine
PUB_KEY_FILE="${IDENTITY_FILE}.pub"
if [ -f "$PUB_KEY_FILE" ]; then
    echo ""
    info "Your SSH Public Key (${PUB_KEY_FILE}):"
    printf "${BOLD}%s${NC}\n" "$(cat "$PUB_KEY_FILE")"
    echo ""
    info "Make sure this public key is appended to '~/.ssh/authorized_keys' on the remote host (${REMOTE_USER}@${REMOTE_HOSTNAME})."
fi

# ------------------------------------------------------------------------------
# 7. Configure ~/.ssh/config
# ------------------------------------------------------------------------------
SSH_CONFIG="${SSH_DIR}/config"
touch "$SSH_CONFIG"
chmod 600 "$SSH_CONFIG"

# Backup ssh config
cp "$SSH_CONFIG" "${SSH_CONFIG}.bak.$(date +%Y%m%d%H%M%S)"

START_MARKER="# BEGIN FARHAND TUNNEL (${HOST_ALIAS})"
END_MARKER="# END FARHAND TUNNEL (${HOST_ALIAS})"

CONFIG_BLOCK=$(cat <<EOF
${START_MARKER}
Host ${HOST_ALIAS}
    HostName ${REMOTE_HOSTNAME}
    User ${REMOTE_USER}
    IdentityFile ${IDENTITY_FILE}
    ProxyCommand cloudflared access ssh --hostname %h
    LocalForward ${LOCAL_PORT} 127.0.0.1:${REMOTE_PORT}
    ServerAliveInterval 60
    ServerAliveCountMax 3
    ExitOnForwardFailure yes
${END_MARKER}
EOF
)

# Remove previous Farhand block for this alias if it exists
if grep -qF "${START_MARKER}" "$SSH_CONFIG"; then
    info "Updating existing SSH config entry for '${HOST_ALIAS}' in ~/.ssh/config..."
    awk -v s="$START_MARKER" -v e="$END_MARKER" '
        $0 == s { skip = 1; next }
        $0 == e { skip = 0; next }
        !skip { print }
    ' "$SSH_CONFIG" > "${SSH_CONFIG}.tmp"
    mv "${SSH_CONFIG}.tmp" "$SSH_CONFIG"
fi

# Append the new block
printf "\n%s\n" "$CONFIG_BLOCK" >> "$SSH_CONFIG"
chmod 600 "$SSH_CONFIG"
success "Configured SSH host '${HOST_ALIAS}' in ~/.ssh/config"

# ------------------------------------------------------------------------------
# 8. Create 'fh-tunnel' Management Helper Script
# ------------------------------------------------------------------------------
FH_TUNNEL_SCRIPT="${BIN_DIR}/fh-tunnel"

cat > "$FH_TUNNEL_SCRIPT" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

HOST_ALIAS="__HOST_ALIAS__"
LOCAL_PORT="__LOCAL_PORT__"

usage() {
    echo "Usage: $(basename "$0") {start|stop|restart|status|ssh|test}"
    echo ""
    echo "Commands:"
    echo "  start    Start the silent background SSH tunnel for Farhand (port ${LOCAL_PORT})"
    echo "  stop     Stop the background SSH tunnel"
    echo "  restart  Restart the background SSH tunnel"
    echo "  status   Check if the tunnel is running and listening on port ${LOCAL_PORT}"
    echo "  ssh      Open an interactive SSH session to ${HOST_ALIAS}"
    echo "  test     Test SSH connectivity through Cloudflare Tunnel"
    exit 1
}

is_port_listening() {
    if command -v nc >/dev/null 2>&1; then
        nc -z 127.0.0.1 "$LOCAL_PORT" >/dev/null 2>&1
    elif command -v ss >/dev/null 2>&1; then
        ss -ltn | grep -q ":${LOCAL_PORT} "
    elif command -v netstat >/dev/null 2>&1; then
        netstat -an | grep -E "LISTEN.*:${LOCAL_PORT}|:${LOCAL_PORT}.*LISTEN" >/dev/null 2>&1
    elif command -v lsof >/dev/null 2>&1; then
        lsof -iTCP:"$LOCAL_PORT" -sTCP:LISTEN >/dev/null 2>&1
    else
        return 1
    fi
}

get_tunnel_pids() {
    pgrep -f "ssh.*LocalForward.*${LOCAL_PORT}.*${HOST_ALIAS}|ssh.*-N.*${HOST_ALIAS}" || true
}

start_tunnel() {
    if is_port_listening; then
        echo "[INFO] Port ${LOCAL_PORT} is already open and listening."
        return 0
    fi
    echo "[INFO] Starting background SSH tunnel for '${HOST_ALIAS}' forwarding port ${LOCAL_PORT}..."
    ssh -f -N "$HOST_ALIAS"
    sleep 1
    if is_port_listening; then
        echo "[OK] Farhand tunnel is active! You can now run: fh <command>"
    else
        echo "[WARN] Tunnel command executed. Verify connection with: $(basename "$0") status"
    fi
}

stop_tunnel() {
    PIDS="$(get_tunnel_pids)"
    if [ -n "$PIDS" ]; then
        echo "[INFO] Terminating background tunnel process(es): $PIDS"
        kill $PIDS
        echo "[OK] Tunnel stopped."
    else
        echo "[INFO] No active Farhand background SSH tunnel process found."
    fi
}

status_tunnel() {
    PIDS="$(get_tunnel_pids)"
    if [ -n "$PIDS" ]; then
        echo "[OK] Background SSH tunnel is running (PID: $PIDS)."
    else
        echo "[INFO] No background SSH tunnel process detected."
    fi

    if is_port_listening; then
        echo "[OK] Port ${LOCAL_PORT} is LISTENING locally."
        echo "     Farhand is ready for offloading: export FARHAND_HOST=127.0.0.1:${LOCAL_PORT}"
    else
        echo "[WARN] Port ${LOCAL_PORT} is NOT listening."
        echo "     Start it with: $(basename "$0") start"
    fi
}

case "${1:-}" in
    start)
        start_tunnel
        ;;
    stop)
        stop_tunnel
        ;;
    restart)
        stop_tunnel
        sleep 1
        start_tunnel
        ;;
    status)
        status_tunnel
        ;;
    ssh)
        exec ssh "$HOST_ALIAS"
        ;;
    test)
        echo "[INFO] Testing SSH connection to '${HOST_ALIAS}'..."
        ssh -o BatchMode=yes -o ConnectTimeout=10 "$HOST_ALIAS" "echo '[OK] Connected successfully to remote host: \$(hostname)'"
        ;;
    *)
        usage
        ;;
esac
EOF

# Substitute alias and port into fh-tunnel
sed -i.tmp "s/__HOST_ALIAS__/${HOST_ALIAS}/g" "$FH_TUNNEL_SCRIPT"
sed -i.tmp "s/__LOCAL_PORT__/${LOCAL_PORT}/g" "$FH_TUNNEL_SCRIPT"
rm -f "${FH_TUNNEL_SCRIPT}.tmp"
chmod +x "$FH_TUNNEL_SCRIPT"
success "Installed tunnel management helper to: ${FH_TUNNEL_SCRIPT}"

# ------------------------------------------------------------------------------
# 9. Verify PATH and Environment
# ------------------------------------------------------------------------------
if ! echo ":$PATH:" | grep -q ":${BIN_DIR}:"; then
    warn "'${BIN_DIR}' is not in your current PATH."
    echo "  Add it to your shell profile (~/.bashrc or ~/.zshrc):"
    echo "    export PATH=\"${BIN_DIR}:\$PATH\""
    echo ""
fi

# ------------------------------------------------------------------------------
# 10. Summary & Next Steps
# ------------------------------------------------------------------------------
printf "\n${BOLD}======================================================${NC}\n"
printf "${GREEN}${BOLD}   Setup Completed Successfully!                     ${NC}\n"
printf "${BOLD}======================================================${NC}\n\n"

cat <<EOF
${BOLD}Configuration Summary:${NC}
  - SSH Host Alias:     ${GREEN}${HOST_ALIAS}${NC}
  - Cloudflare Host:    ${GREEN}${REMOTE_HOSTNAME}${NC}
  - Remote SSH User:    ${GREEN}${REMOTE_USER}${NC}
  - SSH Identity Key:   ${GREEN}${IDENTITY_FILE}${NC}
  - Local Port Forward: ${GREEN}127.0.0.1:${LOCAL_PORT} -> localhost:${REMOTE_PORT}${NC}
  - Tunnel Helper:      ${GREEN}${FH_TUNNEL_SCRIPT}${NC}

${BOLD}How to use your remote Farhand setup:${NC}

1. ${BOLD}Test SSH connection:${NC}
     ssh ${HOST_ALIAS}
   ${BLUE}or:${NC}
     fh-tunnel test

2. ${BOLD}Start the background build tunnel:${NC}
     fh-tunnel start
   ${BLUE}(This runs 'ssh -f -N ${HOST_ALIAS}' in the background)${NC}

3. ${BOLD}Check tunnel status:${NC}
     fh-tunnel status

4. ${BOLD}Run Farhand commands from any local project:${NC}
     export FARHAND_HOST="127.0.0.1:${LOCAL_PORT}"
     export FARHAND_TOKEN="<your-remote-token>"

     fh check
     fh -- cargo test
     fh -- npm run build

5. ${BOLD}Stop the tunnel when done:${NC}
     fh-tunnel stop

EOF

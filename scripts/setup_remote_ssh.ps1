# ==============================================================================
# Farhand (fh) Remote SSH & Cloudflare Tunnel Prerequisites Setup Script (Windows)
#
# This script sets up all client-side prerequisites on Windows to offload builds
# to a remote Farhand daemon (fhd) over SSH routed through Cloudflare Tunnel.
# ==============================================================================

[CmdletBinding()]
param(
    [Alias("H")]
    [string]$Hostname = "",

    [Alias("a")]
    [string]$HostAlias = "farhand-remote",

    [Alias("u")]
    [string]$RemoteUser = "",

    [Alias("k")]
    [string]$IdentityFile = "",

    [Alias("p")]
    [int]$LocalPort = 9876,

    [Alias("r")]
    [int]$RemotePort = 9876,

    [switch]$InstallFh = $false,

    [Alias("y")]
    [switch]$NonInteractive = $false,

    [switch]$Help = $false
)

$ErrorActionPreference = "Stop"

function Show-Usage {
    Write-Host @"
Farhand Remote SSH & Cloudflare Tunnel Setup (Windows)

Installs and configures all prerequisites on your Windows machine to connect
to a remote Farhand daemon ('fhd') over SSH using Cloudflare Tunnel.

Usage:
  .\scripts\setup_remote_ssh.ps1 [options]

Options:
  -Hostname, -H <domain>     Cloudflare hostname for remote host (e.g. mac.example.com)
  -HostAlias, -a <name>      SSH Host alias in ~/.ssh/config (default: farhand-remote)
  -RemoteUser, -u <user>     Remote SSH username (default: current Windows username)
  -IdentityFile, -k <path>   Path to SSH private key (default: auto-detect ~/.ssh/id_*)
  -LocalPort, -p <port>      Local port to forward Farhand to (default: 9876)
  -RemotePort, -r <port>     Remote Farhand daemon port (default: 9876)
  -InstallFh                 Install/update 'fh' client binary if not in PATH
  -NonInteractive, -y        Run non-interactively without interactive prompts
  -Help, -h                  Show this help message and exit

Examples:
  # Interactive setup:
  .\scripts\setup_remote_ssh.ps1

  # Automated non-interactive setup:
  .\scripts\setup_remote_ssh.ps1 `
      -Hostname mac.mydomain.com `
      -RemoteUser builder `
      -HostAlias mac-mini `
      -NonInteractive
"@
    exit 0
}

if ($Help) {
    Show-Usage
}

Write-Host ""
Write-Host "======================================================" -ForegroundColor Cyan
Write-Host "   Farhand Remote SSH & Cloudflare Tunnel Setup       " -ForegroundColor Cyan
Write-Host "======================================================" -ForegroundColor Cyan
Write-Host ""

# ------------------------------------------------------------------------------
# 1. Resolve Installation Directory
# ------------------------------------------------------------------------------
if ($env:LOCALAPPDATA) {
    $BinDir = Join-Path $env:LOCALAPPDATA "Programs\farhand\bin"
} else {
    $BinDir = Join-Path $env:USERPROFILE ".farhand\bin"
}

if (-not (Test-Path $BinDir)) {
    New-Item -ItemType Directory -Path $BinDir -Force | Out-Null
}

# Ensure current session has $BinDir in PATH
if ($env:PATH -notlike "*$BinDir*") {
    $env:PATH = "$BinDir;$env:PATH"
}

# ------------------------------------------------------------------------------
# 2. Check & Install cloudflared
# ------------------------------------------------------------------------------
function Install-Cloudflared {
    Write-Host "[INFO] Installing 'cloudflared' on Windows..." -ForegroundColor Cyan

    # 1. Try winget
    if (Get-Command winget -ErrorAction SilentlyContinue) {
        Write-Host "Attempting install via winget..." -ForegroundColor Gray
        try {
            winget install --id Cloudflare.cloudflared --accept-package-agreements --accept-source-agreements --exact
            $cf = Get-Command cloudflared -ErrorAction SilentlyContinue
            if ($cf) {
                return
            }
        } catch {
            Write-Verbose "Winget install failed: $_"
        }
    }

    # 2. Try scoop
    if (Get-Command scoop -ErrorAction SilentlyContinue) {
        Write-Host "Attempting install via scoop..." -ForegroundColor Gray
        try {
            scoop install cloudflared
            $cf = Get-Command cloudflared -ErrorAction SilentlyContinue
            if ($cf) {
                return
            }
        } catch {
            Write-Verbose "Scoop install failed: $_"
        }
    }

    # 3. Direct official download fallback
    $arch = if ([System.Environment]::Is64BitOperatingSystem) { "amd64" } else { "386" }
    if ($env:PROCESSOR_ARCHITECTURE -eq "ARM64") { $arch = "arm64" }

    $cfUrl = "https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-windows-$arch.exe"
    $cfDest = Join-Path $BinDir "cloudflared.exe"

    Write-Host "Downloading standalone cloudflared binary from: $cfUrl" -ForegroundColor Cyan
    try {
        [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
        Invoke-WebRequest -Uri $cfUrl -OutFile $cfDest -UseBasicParsing
        Write-Host "[OK] Downloaded cloudflared to: $cfDest" -ForegroundColor Green
    } catch {
        throw "Failed to download cloudflared: $_"
    }
}

$cfCmd = Get-Command cloudflared -ErrorAction SilentlyContinue
if ($cfCmd) {
    try {
        $cfVer = & cloudflared --version 2>&1 | Select-Object -First 1
        Write-Host "[OK] 'cloudflared' is already installed ($cfVer)" -ForegroundColor Green
    } catch {
        Write-Host "[OK] 'cloudflared' is installed at $($cfCmd.Source)" -ForegroundColor Green
    }
} else {
    Install-Cloudflared
    $cfCheck = Get-Command cloudflared -ErrorAction SilentlyContinue
    if (-not $cfCheck -and (Test-Path (Join-Path $BinDir "cloudflared.exe"))) {
        Write-Host "[OK] 'cloudflared' installed to $BinDir\cloudflared.exe" -ForegroundColor Green
    } elseif ($cfCheck) {
        Write-Host "[OK] 'cloudflared' installed successfully." -ForegroundColor Green
    } else {
        throw "Could not verify 'cloudflared' installation."
    }
}

# ------------------------------------------------------------------------------
# 3. Check & Install OpenSSH Client
# ------------------------------------------------------------------------------
$sshCmd = Get-Command ssh -ErrorAction SilentlyContinue
if ($sshCmd) {
    try {
        $sshVer = & ssh -V 2>&1 | Select-Object -First 1
        Write-Host "[OK] OpenSSH client is available ($sshVer)" -ForegroundColor Green
    } catch {
        Write-Host "[OK] OpenSSH client is available at $($sshCmd.Source)" -ForegroundColor Green
    }
} else {
    Write-Host "[INFO] 'ssh.exe' not found in PATH. Checking Windows OpenSSH client capability..." -ForegroundColor Yellow
    $installedViaCap = $false
    try {
        $cap = Get-WindowsCapability -Online -Name "OpenSSH.Client*" -ErrorAction SilentlyContinue
        if ($cap -and $cap.State -ne "Installed") {
            Write-Host "Installing OpenSSH.Client capability via Windows Capability..." -ForegroundColor Cyan
            Add-WindowsCapability -Online -Name $cap.Name | Out-Null
            $installedViaCap = $true
        }
    } catch {
        Write-Verbose "Could not add WindowsCapability: $_"
    }

    $sshCmdRetry = Get-Command ssh -ErrorAction SilentlyContinue
    if ($sshCmdRetry -or $installedViaCap) {
        Write-Host "[OK] OpenSSH client installed successfully." -ForegroundColor Green
    } else {
        Write-Host "[WARN] Could not automatically install OpenSSH client." -ForegroundColor Yellow
        Write-Host "Please enable OpenSSH Client in Windows Settings -> Optional Features, or run in an Admin PowerShell:" -ForegroundColor Yellow
        Write-Host "  Add-WindowsCapability -Online -Name OpenSSH.Client~~~~0.0.1.0" -ForegroundColor White
    }
}

# ------------------------------------------------------------------------------
# 4. Check & Install 'fh' Client Binary
# ------------------------------------------------------------------------------
$fhCmd = Get-Command fh -ErrorAction SilentlyContinue
if ($fhCmd) {
    try {
        $fhVer = & fh --version 2>&1 | Select-Object -First 1
        Write-Host "[OK] Farhand client 'fh' is installed ($fhVer)" -ForegroundColor Green
    } catch {
        Write-Host "[OK] Farhand client 'fh' is installed at $($fhCmd.Source)" -ForegroundColor Green
    }
} else {
    $shouldInstallFh = $false
    if ($InstallFh) {
        $shouldInstallFh = $true
    } elseif (-not $NonInteractive) {
        $resp = Read-Host "Farhand client 'fh' was not found in PATH. Install it now? [Y/n]"
        if ($resp -eq "" -or $resp -match "^[Yy]$") {
            $shouldInstallFh = $true
        }
    }

    if ($shouldInstallFh) {
        $scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
        $localInstaller = Join-Path $scriptDir "install.ps1"
        if (Test-Path $localInstaller) {
            Write-Host "Running local installer: $localInstaller" -ForegroundColor Cyan
            & $localInstaller -InstallDir $BinDir
        } else {
            Write-Host "Downloading and running Farhand official Windows installer..." -ForegroundColor Cyan
            & ([ScriptBlock]::Create((Invoke-WebRequest -Uri "https://raw.githubusercontent.com/Rayrsn/farhand/main/scripts/install.ps1" -UseBasicParsing).Content)) -InstallDir $BinDir
        }
    } else {
        Write-Host "[WARN] Skipping 'fh' installation. You will need 'fh' in PATH to submit remote jobs." -ForegroundColor Yellow
    }
}

# ------------------------------------------------------------------------------
# 5. Interactive Configuration (if not non-interactive)
# ------------------------------------------------------------------------------
if (-not $RemoteUser) {
    $RemoteUser = $env:USERNAME
}

if (-not $NonInteractive) {
    Write-Host ""
    Write-Host "[INFO] Configuring SSH connection settings for Cloudflare Tunnel..." -ForegroundColor Cyan
    Write-Host ""

    if (-not $Hostname) {
        while (-not $Hostname) {
            $Hostname = Read-Host "Enter Cloudflare hostname of remote server (e.g. mac.example.com)"
            $Hostname = $Hostname.Trim()
        }
    }

    $inputUser = Read-Host "Remote SSH username [$RemoteUser]"
    if ($inputUser) { $RemoteUser = $inputUser }

    $inputAlias = Read-Host "SSH Host alias for ~/.ssh/config [$HostAlias]"
    if ($inputAlias) { $HostAlias = $inputAlias }

    $inputLPort = Read-Host "Local port to forward Farhand daemon to [$LocalPort]"
    if ($inputLPort) { $LocalPort = [int]$inputLPort }

    $inputRPort = Read-Host "Remote Farhand daemon port [$RemotePort]"
    if ($inputRPort) { $RemotePort = [int]$inputRPort }
}

if (-not $Hostname) {
    throw "Remote Cloudflare hostname is required (specify via -Hostname <domain>)."
}

# ------------------------------------------------------------------------------
# 6. SSH Identity Key Resolution & Generation
# ------------------------------------------------------------------------------
$sshDir = Join-Path $env:USERPROFILE ".ssh"
if (-not (Test-Path $sshDir)) {
    New-Item -ItemType Directory -Path $sshDir -Force | Out-Null
}

if (-not $IdentityFile) {
    $candidates = @(
        (Join-Path $sshDir "id_ed25519"),
        (Join-Path $sshDir "id_rsa"),
        (Join-Path $sshDir "id_ecdsa"),
        (Join-Path $sshDir "main")
    )

    foreach ($c in $candidates) {
        if (Test-Path $c) {
            $IdentityFile = $c
            break
        }
    }

    if (-not $IdentityFile) {
        $defaultKey = Join-Path $sshDir "id_ed25519"
        if ($NonInteractive) {
            Write-Host "[INFO] Generating default ed25519 SSH key at $defaultKey..." -ForegroundColor Cyan
            & ssh-keygen -t ed25519 -f $defaultKey -N '""' -C "farhand-client"
            $IdentityFile = $defaultKey
        } else {
            $genResp = Read-Host "No existing SSH key detected. Generate a new ed25519 key at $defaultKey? [Y/n]"
            if ($genResp -eq "" -or $genResp -match "^[Yy]$") {
                & ssh-keygen -t ed25519 -f $defaultKey -N '""' -C "farhand-client"
                $IdentityFile = $defaultKey
            } else {
                $IdentityFile = Read-Host "Enter path to your SSH private key"
            }
        }
    }
}

$IdentityFile = [System.IO.Path]::GetFullPath($IdentityFile)
if (-not (Test-Path $IdentityFile)) {
    Write-Host "[WARN] Specified identity file '$IdentityFile' does not exist on disk." -ForegroundColor Yellow
}

$pubKeyFile = "$IdentityFile.pub"
if (Test-Path $pubKeyFile) {
    Write-Host ""
    Write-Host "[INFO] Your SSH Public Key ($pubKeyFile):" -ForegroundColor Cyan
    Write-Host (Get-Content $pubKeyFile -Raw).Trim() -ForegroundColor White
    Write-Host ""
    Write-Host "[INFO] Make sure this public key is appended to '~/.ssh/authorized_keys' on the remote host ($RemoteUser@$Hostname)." -ForegroundColor Cyan
}

# ------------------------------------------------------------------------------
# 7. Configure ~/.ssh/config
# ------------------------------------------------------------------------------
$sshConfigFile = Join-Path $sshDir "config"
if (-not (Test-Path $sshConfigFile)) {
    New-Item -ItemType File -Path $sshConfigFile -Force | Out-Null
} else {
    $bakFile = "$sshConfigFile.bak.$(Get-Date -Format 'yyyyMMddHHmmss')"
    Copy-Item $sshConfigFile -Destination $bakFile -Force
}

$startMarker = "# BEGIN FARHAND TUNNEL ($HostAlias)"
$endMarker = "# END FARHAND TUNNEL ($HostAlias)"

# OpenSSH accepts forward slashes in IdentityFile on Windows
$idFileNormalized = $IdentityFile -replace '\\', '/'

$newConfigBlock = @"
$startMarker
Host $HostAlias
    HostName $Hostname
    User $RemoteUser
    IdentityFile $idFileNormalized
    ProxyCommand cloudflared access ssh --hostname %h
    LocalForward $LocalPort 127.0.0.1:$RemotePort
    ServerAliveInterval 60
    ServerAliveCountMax 3
    ExitOnForwardFailure yes
$endMarker
"@

# Read existing file and remove old block if present
$existingLines = if (Test-Path $sshConfigFile) { Get-Content $sshConfigFile } else { @() }
$filteredLines = [System.Collections.Generic.List[string]]::new()
$insideBlock = $false

foreach ($line in $existingLines) {
    if ($line.Trim() -eq $startMarker) {
        $insideBlock = $true
        continue
    }
    if ($line.Trim() -eq $endMarker) {
        $insideBlock = $false
        continue
    }
    if (-not $insideBlock) {
        $filteredLines.Add($line)
    }
}

# Append new block
$finalContent = ($filteredLines -join "`r`n").TrimEnd()
if ($finalContent) {
    $finalContent = "$finalContent`r`n`r`n$newConfigBlock`r`n"
} else {
    $finalContent = "$newConfigBlock`r`n"
}

[System.IO.File]::WriteAllText($sshConfigFile, $finalContent, [System.Text.Encoding]::UTF8)
Write-Host "[OK] Configured SSH host '$HostAlias' in $sshConfigFile" -ForegroundColor Green

# ------------------------------------------------------------------------------
# 8. Create 'fh-tunnel.ps1' & 'fh-tunnel.cmd' Management Helper Scripts
# ------------------------------------------------------------------------------
$tunnelPs1 = Join-Path $BinDir "fh-tunnel.ps1"
$tunnelCmd = Join-Path $BinDir "fh-tunnel.cmd"

$tunnelPs1Content = @"
# Farhand Windows Tunnel Manager
[CmdletBinding()]
param(
    [Parameter(Position=0)]
    [ValidateSet("start", "stop", "restart", "status", "ssh", "test")]
    [string]`$Action = "status"
)

`$HostAlias = "$HostAlias"
`$LocalPort = $LocalPort

function Test-PortListening {
    try {
        `$client = [System.Net.Sockets.TcpClient]::new()
        `$async = `$client.BeginConnect("127.0.0.1", `$LocalPort, `$null, `$null)
        `$success = `$async.AsyncWaitHandle.WaitOne(500, `$false)
        if (`$success -and `$client.Connected) {
            `$client.EndConnect(`$async)
            `$client.Close()
            return `$true
        }
        `$client.Close()
        return `$false
    } catch {
        return `$false
    }
}

function Get-TunnelProcesses {
    Get-CimInstance Win32_Process -Filter "Name = 'ssh.exe'" -ErrorAction SilentlyContinue |
        Where-Object { `$_.CommandLine -like "*`$HostAlias*" }
}

function Start-Tunnel {
    if (Test-PortListening) {
        Write-Host "[INFO] Port `$LocalPort is already open and listening." -ForegroundColor Yellow
        return
    }
    Write-Host "[INFO] Starting background SSH tunnel for '`$HostAlias' forwarding port `$LocalPort..." -ForegroundColor Cyan
    Start-Process -FilePath "ssh.exe" -ArgumentList "-N", `$HostAlias -WindowStyle Hidden
    Start-Sleep -Seconds 1
    if (Test-PortListening) {
        Write-Host "[OK] Farhand tunnel is active! You can now run: fh <command>" -ForegroundColor Green
    } else {
        Write-Host "[WARN] Tunnel launched. Check status with: fh-tunnel status" -ForegroundColor Yellow
    }
}

function Stop-Tunnel {
    `$procs = Get-TunnelProcesses
    if (`$procs) {
        foreach (`$p in `$procs) {
            Write-Host "[INFO] Terminating background tunnel process (PID: `$(`$p.ProcessId))..." -ForegroundColor Cyan
            Stop-Process -Id `$p.ProcessId -Force
        }
        Write-Host "[OK] Tunnel stopped." -ForegroundColor Green
    } else {
        Write-Host "[INFO] No active Farhand background SSH tunnel process found." -ForegroundColor Gray
    }
}

function Show-Status {
    `$procs = Get-TunnelProcesses
    if (`$procs) {
        `$pids = (`$procs | ForEach-Object { `$_.ProcessId }) -join ", "
        Write-Host "[OK] Background SSH tunnel is running (PID: `$pids)." -ForegroundColor Green
    } else {
        Write-Host "[INFO] No background SSH tunnel process detected." -ForegroundColor Gray
    }

    if (Test-PortListening) {
        Write-Host "[OK] Port `$LocalPort is LISTENING locally." -ForegroundColor Green
        Write-Host "     Farhand is ready for offloading: `$env:FARHAND_HOST = '127.0.0.1:`$LocalPort'" -ForegroundColor Gray
    } else {
        Write-Host "[WARN] Port `$LocalPort is NOT listening." -ForegroundColor Yellow
        Write-Host "     Start it with: fh-tunnel start" -ForegroundColor Gray
    }
}

switch (`$Action) {
    "start"   { Start-Tunnel }
    "stop"    { Stop-Tunnel }
    "restart" { Stop-Tunnel; Start-Sleep -Milliseconds 500; Start-Tunnel }
    "status"  { Show-Status }
    "ssh"     { & ssh.exe `$HostAlias }
    "test"    {
        Write-Host "[INFO] Testing SSH connection to '`$HostAlias'..." -ForegroundColor Cyan
        & ssh.exe -o BatchMode=yes -o ConnectTimeout=10 `$HostAlias "echo '[OK] Connected successfully to remote host: %COMPUTERNAME%'"
    }
}
"@

[System.IO.File]::WriteAllText($tunnelPs1, $tunnelPs1Content, [System.Text.Encoding]::UTF8)

# CMD Wrapper so 'fh-tunnel' works in CMD, PowerShell, and Git Bash
$tunnelCmdContent = @"
@echo off
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%~dp0fh-tunnel.ps1" %*
"@
[System.IO.File]::WriteAllText($tunnelCmd, $tunnelCmdContent, [System.Text.Encoding]::ASCII)

Write-Host "[OK] Installed tunnel management helper to: $tunnelCmd" -ForegroundColor Green

# ------------------------------------------------------------------------------
# 9. Ensure BinDir in User PATH
# ------------------------------------------------------------------------------
$userPath = [Environment]::GetEnvironmentVariable("PATH", "User")
if ($userPath -notlike "*$BinDir*") {
    $newUserPath = "$BinDir;$userPath"
    [Environment]::SetEnvironmentVariable("PATH", $newUserPath, "User")
    Write-Host "[OK] Added '$BinDir' to User PATH." -ForegroundColor Green
}

# ------------------------------------------------------------------------------
# 10. Summary & Next Steps
# ------------------------------------------------------------------------------
Write-Host ""
Write-Host "======================================================" -ForegroundColor Green
Write-Host "   Setup Completed Successfully!                     " -ForegroundColor Green
Write-Host "======================================================" -ForegroundColor Green
Write-Host ""

Write-Host "Configuration Summary:" -ForegroundColor Cyan
Write-Host "  - SSH Host Alias:     $HostAlias" -ForegroundColor White
Write-Host "  - Cloudflare Host:    $Hostname" -ForegroundColor White
Write-Host "  - Remote SSH User:    $RemoteUser" -ForegroundColor White
Write-Host "  - SSH Identity Key:   $IdentityFile" -ForegroundColor White
Write-Host "  - Local Port Forward: 127.0.0.1:$LocalPort -> localhost:$RemotePort" -ForegroundColor White
Write-Host "  - Tunnel Helper:      $tunnelCmd" -ForegroundColor White
Write-Host ""

Write-Host "How to use your remote Farhand setup:" -ForegroundColor Cyan
Write-Host ""
Write-Host "1. Test SSH connection:" -ForegroundColor White
Write-Host "     ssh $HostAlias" -ForegroundColor Gray
Write-Host "   or:" -ForegroundColor White
Write-Host "     fh-tunnel test" -ForegroundColor Gray
Write-Host ""
Write-Host "2. Start the background build tunnel:" -ForegroundColor White
Write-Host "     fh-tunnel start" -ForegroundColor Gray
Write-Host "   (Launches hidden background SSH tunnel forwarding port $LocalPort)" -ForegroundColor DarkGray
Write-Host ""
Write-Host "3. Check tunnel status:" -ForegroundColor White
Write-Host "     fh-tunnel status" -ForegroundColor Gray
Write-Host ""
Write-Host "4. Run Farhand commands from any local project:" -ForegroundColor White
Write-Host "     `$env:FARHAND_HOST = `"127.0.0.1:$LocalPort`"" -ForegroundColor Gray
Write-Host "     `$env:FARHAND_TOKEN = `"<your-remote-token>`"" -ForegroundColor Gray
Write-Host ""
Write-Host "     fh check" -ForegroundColor Gray
Write-Host "     fh -- cargo test" -ForegroundColor Gray
Write-Host "     fh -- npm run build" -ForegroundColor Gray
Write-Host ""
Write-Host "5. Stop the tunnel when done:" -ForegroundColor White
Write-Host "     fh-tunnel stop" -ForegroundColor Gray
Write-Host ""

# Farhand Installer for Windows
# Usage in PowerShell:
#   irm https://raw.githubusercontent.com/Rayrsn/farhand/main/scripts/install.ps1 | iex
#
# Options:
#   & { irm https://raw.githubusercontent.com/Rayrsn/farhand/main/scripts/install.ps1 } -Version "v1.0.0"
#   & { irm https://raw.githubusercontent.com/Rayrsn/farhand/main/scripts/install.ps1 } -InstallDir "C:\Tools\farhand"

[CmdletBinding()]
param(
    [string]$Version = "latest",
    [string]$InstallDir = "",
    [switch]$InstallVCRedist = $false
)

$ErrorActionPreference = "Stop"

$Repo = "Rayrsn/farhand"
$Target = "x86_64-pc-windows-msvc"

Write-Host "=== Farhand Windows Installer ===" -ForegroundColor Cyan

# 1. Resolve installation directory
if (-not $InstallDir) {
    if ($env:LOCALAPPDATA) {
        $InstallDir = Join-Path $env:LOCALAPPDATA "Programs\farhand\bin"
    } else {
        $InstallDir = Join-Path $env:USERPROFILE ".farhand\bin"
    }
}

if (-not (Test-Path $InstallDir)) {
    New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
}

# 2. Temporary download folder
$TempDir = Join-Path ([System.IO.Path]::GetTempPath()) ([System.Guid]::NewGuid().ToString())
New-Item -ItemType Directory -Path $TempDir -Force | Out-Null

try {
    # 3. Determine download URLs
    $CandidateUrls = @()
    if ($Version -eq "latest") {
        $CandidateUrls += "https://github.com/$Repo/releases/latest/download/farhand-$Target.zip"
        $CandidateUrls += "https://github.com/$Repo/releases/latest/download/farhand-v1.0.0-$Target.zip"
    } else {
        $Tag = if ($Version.StartsWith("v")) { $Version } else { "v$Version" }
        $CandidateUrls += "https://github.com/$Repo/releases/download/$Tag/farhand-$Tag-$Target.zip"
        $CandidateUrls += "https://github.com/$Repo/releases/download/$Tag/farhand-$Target.zip"
    }

    $ZipPath = Join-Path $TempDir "farhand.zip"
    $Downloaded = $false

    foreach ($Url in $CandidateUrls) {
        Write-Host "Downloading Farhand package from: $Url" -ForegroundColor Gray
        try {
            [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
            Invoke-WebRequest -Uri $Url -OutFile $ZipPath -UseBasicParsing
            $Downloaded = $true
            break
        } catch {
            Write-Verbose "Could not download from $Url : $_"
        }
    }

    if (-not $Downloaded) {
        # Fallback: check if local source repository exists with prebuilt binaries
        if (Test-Path "target\release\fh.exe") {
            Write-Host "Online release zip not found. Using local release build..." -ForegroundColor Yellow
            Copy-Item "target\release\fh.exe" -Destination (Join-Path $InstallDir "fh.exe") -Force
            Copy-Item "target\release\fhd.exe" -Destination (Join-Path $InstallDir "fhd.exe") -Force
        } else {
            throw "Failed to download Farhand prebuilt release binary. Please verify network access or specify -Version."
        }
    } else {
        Write-Host "Extracting binaries..." -ForegroundColor Gray
        $ExtractPath = Join-Path $TempDir "extracted"
        Expand-Archive -Path $ZipPath -DestinationPath $ExtractPath -Force

        $fhExe = Get-ChildItem -Path $ExtractPath -Recurse -Filter "fh.exe" | Select-Object -First 1
        $fhdExe = Get-ChildItem -Path $ExtractPath -Recurse -Filter "fhd.exe" | Select-Object -First 1

        if (-not $fhExe -or -not $fhdExe) {
            throw "Release archive did not contain fh.exe and fhd.exe"
        }

        Copy-Item $fhExe.FullName -Destination (Join-Path $InstallDir "fh.exe") -Force
        Copy-Item $fhdExe.FullName -Destination (Join-Path $InstallDir "fhd.exe") -Force
    }

    # 4. Self-contained MSVC validation & automatic runtime resolution
    # Farhand Windows binaries are statically compiled (+crt-static) with embedded C-runtime,
    # so they run with ZERO external runtime dependencies whether MSVC / Visual Studio is installed or not.
    $System32 = [System.Environment]::GetFolderPath("System")
    $VcRuntimePath = Join-Path $System32 "vcruntime140.dll"
    if (-not (Test-Path $VcRuntimePath)) {
        Write-Host "Notice: Microsoft Visual C++ runtime not found in System32." -ForegroundColor Gray
        Write-Host "Farhand is compiled with static CRT (+crt-static) and runs completely standalone." -ForegroundColor Green
        
        if ($InstallVCRedist) {
            Write-Host "Downloading and installing official Microsoft VC++ Redistributable as requested..." -ForegroundColor Cyan
            $VcRedistUrl = "https://aka.ms/vs/17/release/vc_redist.x64.exe"
            $VcRedistPath = Join-Path $TempDir "vc_redist.x64.exe"
            Invoke-WebRequest -Uri $VcRedistUrl -OutFile $VcRedistPath -UseBasicParsing
            Start-Process -FilePath $VcRedistPath -ArgumentList "/install", "/quiet", "/norestart" -Wait
            Write-Host "Visual C++ Redistributable installed successfully." -ForegroundColor Green
        }
    }

    # 5. Add to User PATH if not present
    $UserPath = [Environment]::GetEnvironmentVariable("PATH", "User")
    if ($UserPath -notlike "*$InstallDir*") {
        $NewUserPath = "$InstallDir;$UserPath"
        [Environment]::SetEnvironmentVariable("PATH", $NewUserPath, "User")
        Write-Host "Added $InstallDir to User PATH." -ForegroundColor Green
    }

    # Also update current session PATH
    if ($env:PATH -notlike "*$InstallDir*") {
        $env:PATH = "$InstallDir;$env:PATH"
    }

    # 6. Verify executable
    $InstalledFh = Join-Path $InstallDir "fh.exe"
    Write-Host ""
    Write-Host "=== Installation Successful! ===" -ForegroundColor Green
    Write-Host "Farhand binaries installed to:"
    Write-Host "  Client: $InstallDir\fh.exe" -ForegroundColor Gray
    Write-Host "  Daemon: $InstallDir\fhd.exe" -ForegroundColor Gray
    Write-Host ""

    & $InstalledFh --version
    Write-Host ""
    Write-Host "Quickstart:" -ForegroundColor Cyan
    Write-Host "  fh --help" -ForegroundColor White
    Write-Host "  fh -- cargo build --release" -ForegroundColor White
    Write-Host ""

} finally {
    if (Test-Path $TempDir) {
        Remove-Item -Path $TempDir -Recurse -Force -ErrorAction SilentlyContinue
    }
}

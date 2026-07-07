# Marqi installer for Windows.
#
# Downloads the latest release, verifies its sha256 checksum, installs to
# %LOCALAPPDATA%\Programs\marqi, and adds that directory to your user PATH:
#
#   irm https://raw.githubusercontent.com/i-naji/marqi/main/install.ps1 | iex
#
# Environment overrides:
#   $env:MARQI_VERSION       install a specific tag, e.g. v0.1.0 (default: latest)
#   $env:MARQI_INSTALL_DIR   install directory
#   $env:MARQI_NO_MODIFY_PATH  set to skip the PATH update
#   $env:MARQI_ALLOW_UNVERIFIED = "1"  install when the checksum is unavailable

$ErrorActionPreference = "Stop"

$Repo = "i-naji/marqi"
$Target = "x86_64-pc-windows-msvc"

# Older Windows PowerShell defaults to TLS 1.0, which GitHub rejects.
[Net.ServicePointManager]::SecurityProtocol = `
    [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

switch ($env:PROCESSOR_ARCHITECTURE) {
    "AMD64" { }
    "ARM64" { Write-Host "note: no native ARM64 build; installing x64 (runs under emulation)" }
    default { throw "unsupported architecture '$env:PROCESSOR_ARCHITECTURE' — see https://github.com/$Repo/releases" }
}

$Version = $env:MARQI_VERSION
if (-not $Version) {
    $Version = (Invoke-RestMethod "https://api.github.com/repos/$Repo/releases/latest").tag_name
    if (-not $Version) { throw "could not determine the latest release" }
}

$InstallDir = $env:MARQI_INSTALL_DIR
if (-not $InstallDir) { $InstallDir = Join-Path $env:LOCALAPPDATA "Programs\marqi" }
$null = New-Item -ItemType Directory -Force -Path $InstallDir

$Archive = "marqi-$Version-$Target.zip"
$Url = "https://github.com/$Repo/releases/download/$Version/$Archive"
$Tmp = Join-Path ([System.IO.Path]::GetTempPath()) "marqi-install-$PID"
$null = New-Item -ItemType Directory -Force -Path $Tmp

try {
    Write-Host "Downloading marqi $Version ($Target)..."
    $ZipPath = Join-Path $Tmp $Archive
    Invoke-WebRequest -Uri $Url -OutFile $ZipPath -UseBasicParsing

    # Verify the published sha256 checksum.
    $Expected = $null
    try {
        $Expected = ((Invoke-RestMethod "$Url.sha256") -split '\s+')[0].ToLower()
    } catch {
        if ($env:MARQI_ALLOW_UNVERIFIED -eq "1") {
            Write-Host "warning: checksum file unavailable; installing without verification"
        } else {
            throw "checksum file unavailable (or set MARQI_ALLOW_UNVERIFIED=1)"
        }
    }
    if ($Expected) {
        $Actual = (Get-FileHash $ZipPath -Algorithm SHA256).Hash.ToLower()
        if ($Actual -ne $Expected) { throw "checksum verification failed" }
    }

    Expand-Archive -Path $ZipPath -DestinationPath $Tmp -Force
    $Exe = Join-Path $Tmp "marqi.exe"
    if (-not (Test-Path $Exe)) { throw "archive did not contain marqi.exe" }
    Copy-Item $Exe (Join-Path $InstallDir "marqi.exe") -Force
} finally {
    Remove-Item -Recurse -Force $Tmp -ErrorAction SilentlyContinue
}

# Add the install directory to the user PATH (and this session) if missing.
if (-not $env:MARQI_NO_MODIFY_PATH) {
    $Normalized = $InstallDir.TrimEnd('\')
    $UserPath = [Environment]::GetEnvironmentVariable("Path", "User")
    if (-not $UserPath) { $UserPath = "" }
    $OnPath = ($UserPath -split ';' | ForEach-Object { $_.TrimEnd('\') }) -contains $Normalized
    if (-not $OnPath) {
        $NewPath = if ($UserPath) { "$UserPath;$InstallDir" } else { $InstallDir }
        [Environment]::SetEnvironmentVariable("Path", $NewPath, "User")
        Write-Host "Added $InstallDir to your user PATH."
    }
    $SessionOnPath = ($env:Path -split ';' | ForEach-Object { $_.TrimEnd('\') }) -contains $Normalized
    if (-not $SessionOnPath) { $env:Path = "$env:Path;$InstallDir" }
}

$Installed = & (Join-Path $InstallDir "marqi.exe") -V
Write-Host "Installed $Installed to $InstallDir"
Write-Host "Open a new terminal if 'marqi' is not picked up in existing ones."

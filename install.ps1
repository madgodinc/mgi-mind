# MGI-Mind installer for Windows.
#
# Usage (PowerShell):
#   irm https://raw.githubusercontent.com/madgodinc/mgi-mind/main/install.ps1 | iex
#
# Environment / params:
#   -InstallDir   target directory (default: $env:LOCALAPPDATA\Programs\mgimind)
#   -Tag          release tag (default: latest)
#   -SkipDoctor   skip downloading Qdrant/ONNX/models at the end

[CmdletBinding()]
param(
    [string]$InstallDir = (Join-Path $env:LOCALAPPDATA 'Programs\mgimind'),
    [string]$Tag        = 'latest',
    [switch]$SkipDoctor
)

$ErrorActionPreference = 'Stop'

$Repo    = 'madgodinc/mgi-mind'
$BinName = 'mgimind.exe'

function Die($msg) { Write-Error $msg; exit 1 }

# --- detect arch -------------------------------------------------------------

$arch = $env:PROCESSOR_ARCHITECTURE
switch ($arch) {
    'AMD64' { $target = 'x86_64-pc-windows-msvc' }
    default { Die "unsupported Windows arch: $arch (only x86_64 is published; build from source)" }
}

Write-Host "Detected: Windows / $arch -> $target"

# --- pick release URL --------------------------------------------------------

$asset = "mgimind-$target.zip"
if ($Tag -eq 'latest') {
    $url = "https://github.com/$Repo/releases/latest/download/$asset"
} else {
    $url = "https://github.com/$Repo/releases/download/$Tag/$asset"
}

# --- download + extract ------------------------------------------------------

New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null

$tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("mgimind-install-" + [System.IO.Path]::GetRandomFileName())
New-Item -ItemType Directory -Force -Path $tmp | Out-Null

try {
    $zip = Join-Path $tmp $asset
    Write-Host "Downloading $url"
    try {
        Invoke-WebRequest -Uri $url -OutFile $zip -UseBasicParsing
    } catch {
        Die "download failed (release for $target may not exist yet; check https://github.com/$Repo/releases): $_"
    }

    # Fetch and verify the SHA-256 checksum published alongside the asset.
    # Fail closed: if the .sha256 file is missing OR the hash mismatches, we
    # do not install. Pipe-to-shell installs are the canonical place to insist on this.
    Write-Host "Verifying SHA-256"
    $shaFile = Join-Path $tmp "$asset.sha256"
    try {
        Invoke-WebRequest -Uri "$url.sha256" -OutFile $shaFile -UseBasicParsing
    } catch {
        Die "checksum file missing at $url.sha256 — refusing to install unverified binary: $_"
    }
    $expectedHex = ((Get-Content -Raw $shaFile) -split '\s+')[0].ToLower()
    if ([string]::IsNullOrWhiteSpace($expectedHex)) {
        Die "checksum file at $url.sha256 is empty or malformed"
    }
    $actualHex = (Get-FileHash -Algorithm SHA256 -Path $zip).Hash.ToLower()
    if ($actualHex -ne $expectedHex) {
        Die "SHA-256 mismatch — refusing to install (expected $expectedHex, got $actualHex)"
    }
    Write-Host "Checksum OK ($expectedHex)"

    Write-Host "Extracting to $InstallDir"
    Expand-Archive -Path $zip -DestinationPath $tmp -Force
    $src = Join-Path $tmp $BinName
    if (-not (Test-Path $src)) { Die "archive did not contain '$BinName'" }
    Copy-Item -Force $src (Join-Path $InstallDir $BinName)
}
finally {
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}

$binPath = Join-Path $InstallDir $BinName
Write-Host "Installed: $binPath"

# --- ensure InstallDir is on the user PATH -----------------------------------

$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
$userPathParts = if ($userPath) { $userPath -split ';' | Where-Object { $_ } } else { @() }

$alreadyOnPath = $userPathParts | Where-Object { $_.TrimEnd('\') -ieq $InstallDir.TrimEnd('\') }
if (-not $alreadyOnPath) {
    Write-Host "Adding $InstallDir to user PATH"
    $newPath = (@($InstallDir) + $userPathParts) -join ';'
    [Environment]::SetEnvironmentVariable('Path', $newPath, 'User')
    # Effective for THIS session too, so init/doctor below resolve $binPath cleanly.
    $env:Path = "$InstallDir;$env:Path"
    Write-Host "Open a new terminal for the PATH change to apply to future shells."
}

# --- init + doctor -----------------------------------------------------------

if ($SkipDoctor) {
    Write-Host "SkipDoctor set; skipping data-dir setup. Run '$binPath doctor --fix' yourself."
} else {
    Write-Host ""
    Write-Host "Setting up data directory and downloading runtime + models (~600 MB)..."
    & $binPath init
    if ($LASTEXITCODE -ne 0) { Die "'mgimind init' failed" }
    & $binPath doctor --fix
    if ($LASTEXITCODE -ne 0) { Die "'mgimind doctor --fix' failed" }
}

# --- final message -----------------------------------------------------------

@"

Done. To wire mgi-mind into Claude Code, run:

    claude mcp add mgimind -- "$binPath" mcp

(Other MCP clients: point them at '"$binPath" mcp' over stdio.)

See AI_INSTRUCTIONS.md in the repo for the assistant-side protocol.
"@ | Write-Host

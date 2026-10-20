<#
.SYNOPSIS
  codegraph installer for Windows (Windows PowerShell 5.1 or PowerShell 7+).

.DESCRIPTION
    irm https://raw.githubusercontent.com/crowbett/codegraph/main/packaging/install.ps1 | iex

  or, with parameters:

    & ([scriptblock]::Create((irm .../install.ps1))) -Version 0.2.0

  Downloads codegraph-<version>-windows-amd64.zip from GitHub releases, verifies it
  against the release's SHA256SUMS and installs codegraph.exe into
  %LOCALAPPDATA%\codegraph\bin (or -Dir / $env:CODEGRAPH_INSTALL_DIR). It never edits
  PATH or your profile; it prints the command to run if the directory is not on PATH.

  Optional token: set $env:GH_TOKEN (or $env:GITHUB_TOKEN) to fetch through the
  GitHub API (higher rate limits). A fine-grained token with only "Contents: read" is enough.
  With a token the script downloads assets through the GitHub API. The token only
  travels in HTTPS request headers (never on a command line), and both Windows
  PowerShell and PowerShell 7 drop the Authorization header when GitHub redirects
  the download to its storage host.

  Environment: CODEGRAPH_VERSION, CODEGRAPH_INSTALL_DIR, GH_TOKEN / GITHUB_TOKEN,
  CODEGRAPH_REPO (owner/name, default crowbett/codegraph).

.PARAMETER Version
  Release to install (X.Y.Z, leading v allowed). Default: latest.

.PARAMETER Dir
  Install directory. Default: %LOCALAPPDATA%\codegraph\bin.
#>
[CmdletBinding()]
param(
    [string]$Version = $env:CODEGRAPH_VERSION,
    [string]$Dir = $env:CODEGRAPH_INSTALL_DIR
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
# Windows PowerShell 5.1 defaults to TLS 1.0/1.1 on older .NET configs.
[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

function Write-Step([string]$msg) { Write-Host "codegraph-install: $msg" }

$repo = if ($env:CODEGRAPH_REPO) { $env:CODEGRAPH_REPO } else { 'crowbett/codegraph' }
$token = if ($env:GH_TOKEN) { $env:GH_TOKEN } elseif ($env:GITHUB_TOKEN) { $env:GITHUB_TOKEN } else { '' }
if (-not $Dir) { $Dir = Join-Path $env:LOCALAPPDATA 'codegraph\bin' }
if ($Version) { $Version = $Version.TrimStart('v') }

$arch = $env:PROCESSOR_ARCHITEW6432
if (-not $arch) { $arch = $env:PROCESSOR_ARCHITECTURE }
if ($arch -ne 'AMD64' -and $arch -ne 'ARM64') {
    throw "unsupported architecture $arch (codegraph ships windows-amd64 only)"
}
if ($arch -eq 'ARM64') {
    Write-Step 'ARM64 Windows: installing the amd64 build (runs under emulation)'
}

$api = "https://api.github.com/repos/$repo"
$headers = @{ 'User-Agent' = 'codegraph-install' }
if ($token) { $headers['Authorization'] = "Bearer $token" }

$release = $null
if ($token) {
    $relUrl = if ($Version) { "$api/releases/tags/v$Version" } else { "$api/releases/latest" }
    $h = $headers.Clone(); $h['Accept'] = 'application/vnd.github+json'
    try {
        $release = Invoke-RestMethod -Uri $relUrl -Headers $h -UseBasicParsing
    } catch {
        throw "cannot read release metadata from $relUrl (token lacks access, or no such release): $_"
    }
    if (-not $Version) { $Version = $release.tag_name.TrimStart('v') }
} elseif (-not $Version) {
    try {
        $release = Invoke-RestMethod -Uri "$api/releases/latest" -Headers $headers -UseBasicParsing
        $Version = $release.tag_name.TrimStart('v')
    } catch {
        throw "cannot find the latest release of $repo (pass -Version): $_"
    }
}

$asset = "codegraph-$Version-windows-amd64.zip"
Write-Step "installing codegraph $Version into $Dir"

$tmp = Join-Path ([IO.Path]::GetTempPath()) ("codegraph-" + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $tmp | Out-Null

function Get-Asset([string]$name, [string]$out) {
    if ($token) {
        $a = $release.assets | Where-Object { $_.name -eq $name } | Select-Object -First 1
        if (-not $a) { throw "release v$Version has no asset $name" }
        $h = $headers.Clone(); $h['Accept'] = 'application/octet-stream'
        Invoke-WebRequest -Uri $a.url -Headers $h -OutFile $out -UseBasicParsing
    } else {
        Invoke-WebRequest -Uri "https://github.com/$repo/releases/download/v$Version/$name" -Headers $headers -OutFile $out -UseBasicParsing
    }
}

try {
    $zip = Join-Path $tmp $asset
    $sums = Join-Path $tmp 'SHA256SUMS'
    Get-Asset $asset $zip
    Get-Asset 'SHA256SUMS' $sums

    $expected = $null
    foreach ($line in Get-Content $sums) {
        $parts = $line.Trim() -split '\s+', 2
        if ($parts.Count -eq 2 -and $parts[1].TrimStart('*') -eq $asset) { $expected = $parts[0].ToLowerInvariant() }
    }
    if (-not $expected) { throw "SHA256SUMS has no entry for $asset" }
    $actual = (Get-FileHash -Algorithm SHA256 $zip).Hash.ToLowerInvariant()
    if ($actual -ne $expected) { throw "checksum mismatch for $asset (expected $expected, got $actual)" }
    Write-Step 'sha256 ok'

    $x = Join-Path $tmp 'x'
    Expand-Archive -Path $zip -DestinationPath $x -Force
    $exe = Join-Path $x 'codegraph.exe'
    if (-not (Test-Path $exe)) { throw 'archive does not contain codegraph.exe' }

    New-Item -ItemType Directory -Force -Path $Dir | Out-Null
    $dest = Join-Path $Dir 'codegraph.exe'
    # A running codegraph.exe (e.g. an MCP server) cannot be overwritten but can be renamed.
    if (Test-Path $dest) {
        $old = "$dest.old"
        Remove-Item $old -Force -ErrorAction SilentlyContinue
        Move-Item $dest $old -Force
    }
    Copy-Item $exe $dest -Force
    Remove-Item "$dest.old" -Force -ErrorAction SilentlyContinue
    Write-Step "installed $dest"
    & $dest version
} finally {
    Remove-Item $tmp -Recurse -Force -ErrorAction SilentlyContinue
}

$onPath = ($env:PATH -split ';') | Where-Object { $_.TrimEnd('\') -ieq $Dir.TrimEnd('\') }
if (-not $onPath) {
    Write-Step "$Dir is not on your PATH. To add it for your user, run:"
    Write-Host "  [Environment]::SetEnvironmentVariable('Path', [Environment]::GetEnvironmentVariable('Path', 'User') + ';$Dir', 'User')"
}

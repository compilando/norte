[CmdletBinding()]
param(
    # Defaults to this checkout, resolved below: Windows PowerShell 5.1
    # leaves $PSScriptRoot empty inside param() under `powershell -File`.
    [string]$Repository = '',
    [ValidateSet('debug', 'release')]
    [string]$Profile = 'debug',
    # Use the `ui\dist` already there (built elsewhere: the bundle is
    # platform-independent). The local VM's emulated e1000e NIC has hung
    # under `npm ci`'s download burst; a hosted runner never needs this.
    [switch]$SkipUi,
    # Stop after the sidecars: `package.ps1` has the Tauri CLI build the
    # window itself.
    [switch]$SkipGui
)

# ADR 0157 "build": norte, ntc and norte-gui from ONE build. The window
# carries the other two as Tauri sidecars (`externalBin`), and Tauri's build
# script refuses to compile it until `binaries\<bin>-<triple>.exe` exist, so
# they are built and copied first.

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$env:CARGO_INCREMENTAL = '0'
$target = 'x86_64-pc-windows-msvc'
if (-not $Repository) { $Repository = (Resolve-Path (Join-Path $PSScriptRoot '..\..\..')).Path }

Push-Location $Repository
try {
    $flags = @('--locked', '--target', $target)
    if ($Profile -eq 'release') { $flags += '--release' }
    $out = "target\$target\$Profile"

    Write-Host "== build norte, ntc ($Profile)"
    cargo build @flags -p norte-cli -p norte-tui
    if ($LASTEXITCODE -ne 0) { throw 'cargo build failed: norte-cli, norte-tui' }

    $sidecars = 'crates\norte-gui-tauri\binaries'
    New-Item -ItemType Directory -Force $sidecars | Out-Null
    foreach ($bin in @('norte', 'ntc')) {
        Copy-Item -Force "$out\$bin.exe" "$sidecars\$bin-$target.exe"
    }

    if ($SkipGui) { return }

    if ($SkipUi) {
        if (-not (Test-Path 'crates\norte-gui-tauri\ui\dist\index.html')) {
            throw '-SkipUi needs an existing crates\norte-gui-tauri\ui\dist'
        }
        Write-Host '== webview bundle: using the existing ui\dist'
    } else {
        Write-Host '== build the webview bundle'
        Push-Location 'crates\norte-gui-tauri\ui'
        try {
            npm ci --no-audit --no-fund
            if ($LASTEXITCODE -ne 0) { throw 'npm ci failed' }
            npm run build
            if ($LASTEXITCODE -ne 0) { throw 'UI build failed' }
        } finally { Pop-Location }
    }

    Write-Host "== build norte-gui ($Profile)"
    cargo build @flags -p norte-gui-tauri --bin norte-gui
    if ($LASTEXITCODE -ne 0) { throw 'cargo build failed: norte-gui' }

    foreach ($bin in @('norte', 'ntc', 'norte-gui')) {
        $item = Get-Item "$out\$bin.exe"
        Write-Host ('built {0} ({1:N1} MB)' -f $item.FullName, ($item.Length / 1MB))
    }
} finally {
    Pop-Location
}

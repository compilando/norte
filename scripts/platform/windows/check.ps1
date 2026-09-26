[CmdletBinding()]
param(
    [string]$Repository = (Resolve-Path (Join-Path $PSScriptRoot '..\..\..')).Path,
    [switch]$WithUi
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$env:CARGO_INCREMENTAL = '0'
$target = 'x86_64-pc-windows-msvc'

Push-Location $Repository
try {
    rustup target add $target
    if ($LASTEXITCODE -ne 0) { throw 'could not install the Windows Rust target' }

    foreach ($package in @('norte-client', 'norte-core', 'norte-cli', 'norte-tui')) {
        Write-Host "== check $package ($target)"
        cargo check --locked --target $target -p $package
        if ($LASTEXITCODE -ne 0) { throw "cargo check failed: $package" }
    }

    if ($WithUi) {
        Push-Location 'crates\norte-gui-tauri\ui'
        try {
            npm ci --no-audit --no-fund
            if ($LASTEXITCODE -ne 0) { throw 'npm ci failed' }
            npm run build
            if ($LASTEXITCODE -ne 0) { throw 'UI build failed' }
        } finally { Pop-Location }
        cargo check --locked --target $target -p norte-gui-tauri
        if ($LASTEXITCODE -ne 0) { throw 'cargo check failed: norte-gui-tauri' }
    }
} finally {
    Pop-Location
}

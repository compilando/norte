[CmdletBinding()]
param(
    # Defaults to this checkout, resolved below (see build.ps1).
    [string]$Repository = '',
    [switch]$WithUi
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$env:CARGO_INCREMENTAL = '0'
$target = 'x86_64-pc-windows-msvc'
if (-not $Repository) { $Repository = (Resolve-Path (Join-Path $PSScriptRoot '..\..\..')).Path }

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
        # The window cannot even be checked without its sidecars, which
        # means building norte and ntc: that is the build step.
        & (Join-Path $PSScriptRoot 'build.ps1') -Repository $Repository
    }
} finally {
    Pop-Location
}

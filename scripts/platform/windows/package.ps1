[CmdletBinding()]
param(
    # Defaults to this checkout, resolved below (see build.ps1).
    [string]$Repository = '',
    [string]$OutDir = '',
    # Reuse an existing `ui\dist` (see build.ps1).
    [switch]$SkipUi
)

# ADR 0157 "package": from ONE release build, the NSIS installer and a
# portable ZIP, both carrying norte-gui, norte and ntc, plus SHA256SUMS.
# The installer is Tauri's own NSIS bundle: it places the sidecars next to
# the window, which is where the window looks for its daemon.

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$env:CARGO_INCREMENTAL = '0'
$target = 'x86_64-pc-windows-msvc'
if (-not $Repository) { $Repository = (Resolve-Path (Join-Path $PSScriptRoot '..\..\..')).Path }

Push-Location $Repository
try {
    if (-not $OutDir) { $OutDir = Join-Path $Repository 'target\dist-windows' }
    $version = ((cargo pkgid -p norte-gui-tauri) -split '[#@]')[-1]
    Write-Host "== package norte $version ($target)"

    # Release sidecars, plus the webview bundle unless reused.
    & (Join-Path $PSScriptRoot 'build.ps1') -Repository $Repository -Profile release -SkipGui
    if (-not $SkipUi) {
        Push-Location 'crates\norte-gui-tauri\ui'
        try {
            npm ci --no-audit --no-fund
            if ($LASTEXITCODE -ne 0) { throw 'npm ci failed' }
            npm run build
            if ($LASTEXITCODE -ne 0) { throw 'UI build failed' }
        } finally { Pop-Location }
    }

    # The Tauri CLI runs from the crate: tauri.conf.json lives there.
    Push-Location 'crates\norte-gui-tauri'
    try {
        & '.\ui\node_modules\.bin\tauri.cmd' build --bundles nsis
        if ($LASTEXITCODE -ne 0) { throw 'tauri build failed' }
    } finally { Pop-Location }

    $release = 'target\release'
    New-Item -ItemType Directory -Force $OutDir | Out-Null
    Get-ChildItem $OutDir | Remove-Item -Recurse -Force

    # By name, not "the first one": `bundle\nsis` keeps every earlier
    # version's installer, and v0.3.0-alpha.5 first shipped alpha.4's.
    $installer = Get-Item "$release\bundle\nsis\norte_${version}_x64-setup.exe" -ErrorAction SilentlyContinue
    if (-not $installer) { throw "no NSIS installer for $version was produced" }
    Copy-Item $installer.FullName $OutDir

    # The portable ZIP takes the SAME binaries the installer carries.
    $stage = Join-Path $OutDir "norte-$version-$target"
    New-Item -ItemType Directory -Force $stage | Out-Null
    foreach ($bin in @('norte-gui', 'norte', 'ntc')) {
        # A clean Windows has no vcruntime140.dll: such a binary dies on
        # start with 0xC0000135 (see .cargo/config.toml). The import table
        # names it in ASCII, so a byte search is enough.
        $bytes = [System.IO.File]::ReadAllBytes((Resolve-Path "$release\$bin.exe"))
        $text = [System.Text.Encoding]::ASCII.GetString($bytes)
        if ($text.IndexOf('vcruntime140.dll', [StringComparison]::OrdinalIgnoreCase) -ge 0) {
            throw "$bin.exe imports vcruntime140.dll: it would not start on a clean Windows"
        }
        Copy-Item -Force "$release\$bin.exe" $stage
    }
    $zip = "$stage.zip"
    Compress-Archive -Path "$stage\*" -DestinationPath $zip -Force
    Remove-Item -Recurse -Force $stage

    Push-Location $OutDir
    try {
        # `-Path *`: in Windows PowerShell 5 `-Exclude` filters nothing
        # without a wildcard path, and the pipeline came out empty.
        Get-ChildItem -Path * -File -Exclude SHA256SUMS | ForEach-Object {
            '{0}  {1}' -f (Get-FileHash $_.Name -Algorithm SHA256).Hash.ToLower(), $_.Name
        } | Set-Content -Encoding ascii SHA256SUMS
        Get-ChildItem -File | ForEach-Object {
            Write-Host ('packaged {0} ({1:N1} MB)' -f $_.Name, ($_.Length / 1MB))
        }
    } finally { Pop-Location }
} finally {
    Pop-Location
}

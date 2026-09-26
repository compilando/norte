[CmdletBinding()]
param(
    [switch]$Install,
    [switch]$RemoteOnly
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = [Security.Principal.WindowsPrincipal]::new($identity)
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    $forwarded = @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', "`"$PSCommandPath`"")
    if ($Install) { $forwarded += '-Install' }
    if ($RemoteOnly) { $forwarded += '-RemoteOnly' }
    Start-Process powershell.exe -Verb RunAs -ArgumentList $forwarded
    return
}

function Has-Command([string]$Name) {
    return [bool](Get-Command $Name -ErrorAction SilentlyContinue)
}

function Install-WingetPackage([string]$Id, [string[]]$Extra = @()) {
    $arguments = @('install', '--id', $Id, '--exact', '--silent', '--accept-package-agreements', '--accept-source-agreements') + $Extra
    & winget @arguments
    if ($LASTEXITCODE -ne 0) { throw "winget failed for $Id ($LASTEXITCODE)" }
}

function Enable-OpenSshServer {
    if (-not (Get-Service sshd -ErrorAction SilentlyContinue)) {
        if (-not (Has-Command 'winget')) { throw 'winget is required to install OpenSSH Server' }
        Install-WingetPackage 'Microsoft.OpenSSH.Preview' @('--version', '10.0.0.0')
    }
    if (-not (Get-Service sshd -ErrorAction SilentlyContinue)) { throw 'OpenSSH installed without an sshd service' }
    Start-Service sshd
    Set-Service -Name sshd -StartupType Automatic
    if (-not (Get-NetFirewallRule -Name 'OpenSSH-Server-In-TCP' -ErrorAction SilentlyContinue)) {
        New-NetFirewallRule -Name 'OpenSSH-Server-In-TCP' -DisplayName 'OpenSSH Server (sshd)' `
            -Enabled True -Direction Inbound -Protocol TCP -Action Allow -LocalPort 22 | Out-Null
    }
    Write-Host 'OpenSSH Server: ready'
}

Enable-OpenSshServer
if ($RemoteOnly) { return }

if ($Install) {
    if (-not (Has-Command 'winget')) { throw 'winget is required for unattended provisioning' }
    Install-WingetPackage 'Git.Git'
    Install-WingetPackage 'OpenJS.NodeJS.22'
    Install-WingetPackage 'Rustlang.Rustup'
    Install-WingetPackage 'Microsoft.EdgeWebView2Runtime'
    Install-WingetPackage 'Microsoft.VisualStudio.2022.BuildTools' @(
        '--override', '--wait --quiet --norestart --nocache --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended'
    )
}

$env:Path = [Environment]::GetEnvironmentVariable('Path', 'Machine') + ';' + `
    [Environment]::GetEnvironmentVariable('Path', 'User')
New-ItemProperty -Path 'HKLM:\SYSTEM\CurrentControlSet\Control\FileSystem' `
    -Name LongPathsEnabled -Value 1 -PropertyType DWord -Force | Out-Null
if (Has-Command 'git') { git config --global core.longpaths true }

$missing = @()
foreach ($command in @('git', 'node', 'npm', 'rustup', 'cargo')) {
    if (-not (Has-Command $command)) { $missing += $command }
}
if ($missing.Count -gt 0) {
    throw "missing tools after bootstrap: $($missing -join ', ')"
}

$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
if (-not (Test-Path $vswhere)) { throw 'Visual Studio Build Tools were not found' }
$vcTools = & $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
if (-not $vcTools) { throw 'the Visual C++ x64 build tools are not installed' }
if (-not (node --version).StartsWith('v22.')) { throw 'Node 22 is required' }

Write-Host "git:    $(git --version)"
Write-Host "node:   $(node --version)"
Write-Host "npm:    $(npm --version)"
Write-Host "rustup: $(rustup --version)"
Write-Host 'bootstrap: ready; clone Norte to C:\src\norte'

[CmdletBinding()]
param(
    # A published release: its installer, ZIP and SHA256SUMS-windows are
    # downloaded from GitHub.
    [string]$Tag = '',
    # Or a local `target\dist-windows` copied onto the machine.
    [string]$From = ''
)

# ADR 0157's smoke, for a CLEAN Windows: no Visual Studio, no Rust, no
# redistributables — what a user has. The build VM cannot answer this: it
# has every runtime a binary might forget to carry (alpha.5 and alpha.6
# needed vcruntime140.dll, and only a clean machine showed it).

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$failed = @()
function Check([string]$what, [bool]$ok, [string]$detail = '') {
    Write-Host ('{0,-4} {1} {2}' -f $(if ($ok) { 'ok' } else { 'FAIL' }), $what, $detail)
    if (-not $ok) { $script:failed += $what }
}

$work = Join-Path $env:TEMP 'norte-smoke'
Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
New-Item -ItemType Directory $work | Out-Null
if ($Tag) {
    $ver = $Tag.TrimStart('v')
    $base = "https://github.com/compilando/norte/releases/download/$Tag"
    $ProgressPreference = 'SilentlyContinue'
    foreach ($f in "norte_${ver}_x64-setup.exe", "norte-$ver-x86_64-pc-windows-msvc.zip", 'SHA256SUMS-windows') {
        Invoke-WebRequest "$base/$f" -OutFile (Join-Path $work $f) -UseBasicParsing
    }
    $sums = 'SHA256SUMS-windows'
} elseif ($From) {
    Copy-Item "$From\*" $work
    $sums = 'SHA256SUMS'
} else { throw 'pass -Tag or -From' }

Write-Host "== environment (should be clean)"
Write-Host "vcruntime140 in System32: $(Test-Path C:\Windows\System32\vcruntime140.dll)"
Write-Host "cargo on PATH: $([bool](Get-Command cargo -ErrorAction SilentlyContinue))"

Write-Host "== checksums"
foreach ($line in Get-Content (Join-Path $work $sums)) {
    $sum, $name = $line -split '\s+', 2
    $name = $name.TrimStart('*')
    Check "sha256 $name" ((Get-FileHash (Join-Path $work $name) -Algorithm SHA256).Hash.ToLower() -eq $sum)
}

function Versions([string]$dir, [string]$label) {
    foreach ($b in 'ntc', 'norte', 'norte-gui') {
        $o = & "$dir\$b.exe" --version 2>&1
        Check "$label $b --version" ($LASTEXITCODE -eq 0) "($LASTEXITCODE) $o"
    }
}

Write-Host "== portable ZIP"
$zip = Get-ChildItem $work -Filter *.zip | Select-Object -First 1
Expand-Archive $zip.FullName (Join-Path $work 'zip')
Versions (Get-ChildItem (Join-Path $work 'zip') -Recurse -Filter ntc.exe | Select-Object -First 1).DirectoryName 'zip'

Write-Host "== installer"
$setup = Get-ChildItem $work -Filter *-setup.exe | Select-Object -First 1
$p = Start-Process $setup.FullName -ArgumentList '/S' -Wait -PassThru
Check 'setup /S' ($p.ExitCode -eq 0) "($($p.ExitCode))"
$installed = Join-Path $env:LOCALAPPDATA 'norte'
Versions $installed 'installed'

Write-Host "== the installed daemon, over its pipe"
$dir = Join-Path $work 'listed'
New-Item -ItemType Directory $dir | Out-Null
Set-Content (Join-Path $dir 'hello.txt') 'hi'
$o = & "$installed\norte.exe" --daemon ls $dir 2>&1
Check 'norte --daemon ls' ($LASTEXITCODE -eq 0 -and "$o" -match 'hello\.txt') "$o"

Write-Host "== the window starts and stays up"
$gui = Start-Process "$installed\norte-gui.exe" -PassThru
Start-Sleep -Seconds 8
Check 'norte-gui alive after 8 s' (-not $gui.HasExited)
if (-not $gui.HasExited) { Stop-Process $gui.Id -Force }
# The window started its own daemon; stopping it prints to stderr, which
# Windows PowerShell 5.1 turns into a terminating error under 'Stop'.
$ErrorActionPreference = 'Continue'
& "$installed\norte.exe" daemon stop 2>&1 | Out-Null

if ($failed) { throw "smoke FAILED: $($failed -join ', ')" }
Write-Host 'smoke: all green'

<#
    Build, install and (optionally) register Codenotch for Windows.

    The development build lives in target\release, which is a bad place to point an
    autostart entry at: a `cargo clean`, or a rebuild while the app is running, leaves
    the entry aiming at nothing. So the release binary is copied to
    %LOCALAPPDATA%\Programs\Codenotch, and the Run value is written from there: running
    the installed copy with `autostart on` is what makes the entry point at itself.

    Usage:
      .\install.ps1                 build, install, start with Windows, launch
      .\install.ps1 -NoAutostart    build, install, launch (no Run entry)
      .\install.ps1 -Uninstall      remove the Run entry and the installed copy
#>
param(
    [switch]$NoAutostart,
    [switch]$Uninstall
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $MyInvocation.MyCommand.Path
$dest = Join-Path $env:LOCALAPPDATA 'Programs\Codenotch'
$target = Join-Path $dest 'codenotch.exe'
$name = 'Codenotch'
$runKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'

if ($Uninstall) {
    if (Get-Process codenotch -ErrorAction SilentlyContinue) {
        Get-Process codenotch | Stop-Process -Force
        Start-Sleep -Seconds 2
    }
    Remove-ItemProperty -Path $runKey -Name $name -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $dest -Recurse -Force -ErrorAction SilentlyContinue
    Write-Host "removed: $dest and the Run entry"
    exit 0
}

Push-Location $root
try {
    Write-Host 'building (release)...'
    # cargo reports progress on stderr, and PowerShell 5.1 turns a native command's stderr into a
    # NativeCommandError, which ErrorActionPreference='Stop' would turn into a failed build. The
    # exit code is what actually says whether it worked.
    $prev = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    cargo build --release
    $code = $LASTEXITCODE
    $ErrorActionPreference = $prev
    if ($code -ne 0) { throw "cargo build failed with $code" }
} finally {
    Pop-Location
}

# The running copy holds a lock on the file being replaced
if (Get-Process codenotch -ErrorAction SilentlyContinue) {
    Write-Host 'stopping the running instance...'
    Get-Process codenotch | Stop-Process -Force
    Start-Sleep -Seconds 2
}

New-Item -ItemType Directory -Force -Path $dest | Out-Null
Copy-Item (Join-Path $root 'target\release\codenotch.exe') $target -Force
Write-Host "installed: $target"

if (-not $NoAutostart) {
    & $target autostart on | Out-Host
    $entry = (Get-ItemProperty -Path $runKey -Name $name -ErrorAction SilentlyContinue).$name
    Write-Host "start with Windows: $entry"
}

Start-Process -FilePath $target
Write-Host 'started'

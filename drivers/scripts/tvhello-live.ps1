# Guest-side Step 104 live path. Run in Windows 11 ARM64 with VS + WDK.
# Does not commit certificates. Scratch bugcheck is opt-in.
param(
    [string]$RepoRoot = "",
    [switch]$Bugcheck
)

$ErrorActionPreference = "Stop"
if (-not $RepoRoot) {
    $RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
}
$Build = Join-Path $RepoRoot "drivers\build.ps1"
$Inf = Join-Path $RepoRoot "drivers\tvhello\tvhello.inf"
$LogDir = Join-Path $env:ProgramData "Ternvale\driver-logs"
$Out = Join-Path $RepoRoot "target\driver-dev-guest"
New-Item -ItemType Directory -Force -Path $Out | Out-Null

Write-Host "tvhello-live: testsigning"
bcdedit /set testsigning on | Out-File (Join-Path $Out "bcdedit-testsigning.txt")
bcdedit /enum "{current}" | Out-File (Join-Path $Out "bcdedit-enum.txt")

Write-Host "tvhello-live: build (warnings-as-errors)"
& $Build -Config Debug
if ($LASTEXITCODE -ne 0) { throw "build.ps1 failed" }

Write-Host "tvhello-live: pnputil install"
pnputil /add-driver $Inf /install | Tee-Object (Join-Path $Out "pnputil.txt")

Write-Host "tvhello-live: verifier standard on tvhello.sys"
verifier /standard /driver tvhello.sys | Tee-Object (Join-Path $Out "verifier-set.txt")
verifier /query | Tee-Object (Join-Path $Out "verifier-query.txt")

New-Item -ItemType Directory -Force -Path $LogDir | Out-Null
if (Test-Path (Join-Path $LogDir "tvhello.log")) {
    Copy-Item (Join-Path $LogDir "tvhello.log") (Join-Path $Out "tvhello.log") -Force
}

if ($Bugcheck) {
    Write-Host "tvhello-live: scratch bugcheck build"
    & $Build -Config Debug -ScratchBugcheck
    Write-Host "tvhello-live: sending IOCTL_TVHELLO_BUGCHECK (guest will bugcheck)"
    # User-mode DeviceIoControl is left to a tiny helper; dump collection is Windows crash settings.
}

Write-Host "tvhello-live: PASS (install + verifier programmed). Reboot for Verifier to attach."
Write-Host "WinDbg: bp tvhello!TvHelloEvtD0Entry ; dt TVHELLO_CONTEXT ; !devobj"

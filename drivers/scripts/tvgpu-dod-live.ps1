# Step 105 guest live path: KMDOD install, Device Manager, Code Analysis / SDV,
# Driver Verifier, disable/enable. 1-hour stress is TERNVALE_TVGPU_STRESS_SECONDS
# (default 3600). Sleep/resume is opt-in (TERNVALE_TVGPU_SLEEP=1).
param(
    [string]$RepoRoot = "",
    [int]$StressSeconds = -1
)

$ErrorActionPreference = "Stop"
if (-not $RepoRoot) {
    $RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
}
if ($StressSeconds -lt 0) {
    if ($env:TERNVALE_TVGPU_STRESS_SECONDS) {
        $StressSeconds = [int]$env:TERNVALE_TVGPU_STRESS_SECONDS
    } else {
        $StressSeconds = 3600
    }
}

$Build = Join-Path $RepoRoot "drivers\build.ps1"
$Proj = Join-Path $RepoRoot "drivers\tvgpu-dod\tvgpu-dod.vcxproj"
$Inf = Join-Path $RepoRoot "drivers\tvgpu-dod\tvgpu-dod.inf"
$Out = Join-Path $RepoRoot "target\tvgpu-dod-guest"
New-Item -ItemType Directory -Force -Path $Out | Out-Null

function Write-Sidecar([string]$Message) {
    $dir = Join-Path $env:ProgramData "Ternvale\driver-logs"
    New-Item -ItemType Directory -Force -Path $dir | Out-Null
    Add-Content -Path (Join-Path $dir "tvgpu-dod-live.log") -Value $Message
}

Write-Host "tvgpu-dod-live: testsigning"
bcdedit /set testsigning on | Out-File (Join-Path $Out "bcdedit-testsigning.txt")

Write-Host "tvgpu-dod-live: MSBuild + Code Analysis (warnings-as-errors)"
& $Build -Config Debug -Project $Proj
if ($LASTEXITCODE -ne 0) { throw "tvgpu-dod build/code analysis failed" }

$SdvLog = Join-Path $Out "sdv.txt"
$Sdv = Get-Command sdv.exe -ErrorAction SilentlyContinue
if ($Sdv) {
    Write-Host "tvgpu-dod-live: Static Driver Verifier"
    Push-Location (Join-Path $RepoRoot "drivers\tvgpu-dod")
    try {
        & $Sdv.Source /check /de tvgpu-dod 2>&1 | Tee-Object $SdvLog
        if ($LASTEXITCODE -ne 0) { throw "SDV failed; see $SdvLog" }
    } finally {
        Pop-Location
    }
} else {
    "SDV skipped: sdv.exe not on PATH (install WDK Static Driver Verifier)" |
        Tee-Object $SdvLog
    Write-Host "tvgpu-dod-live: SKIP SDV (no sdv.exe)"
}

Write-Host "tvgpu-dod-live: pnputil install"
pnputil /add-driver $Inf /install | Tee-Object (Join-Path $Out "pnputil.txt")

Write-Host "tvgpu-dod-live: Device Manager query"
$gpu = Get-PnpDevice -PresentOnly -ErrorAction SilentlyContinue |
    Where-Object { $_.InstanceId -match 'VEN_1AF4.*DEV_1050' }
$gpu | Format-List Status, Class, FriendlyName, InstanceId, Problem |
    Out-File (Join-Path $Out "pnp-gpu.txt")
if (-not $gpu) { throw "PCI VEN_1AF4 DEV_1050 not present" }
$bad = @($gpu | Where-Object { $_.Status -ne 'OK' })
if ($bad.Count -gt 0) { throw "tvgpu-dod PnP status not OK: $($bad.Status)" }

$basic = Get-PnpDevice -Class Display -PresentOnly -ErrorAction SilentlyContinue |
    Where-Object { $_.FriendlyName -match 'Basic Display Adapter' -and $_.Status -eq 'OK' }
$basic | Format-List | Out-File (Join-Path $Out "pnp-basic.txt")
$startedGpu = @($gpu | Where-Object { $_.Status -eq 'OK' })
if ($startedGpu.Count -eq 0 -and $basic) {
    throw "desktop is on Microsoft Basic Display Adapter; tvgpu-dod did not start"
}
Write-Sidecar "device manager OK $($gpu.FriendlyName)"

Write-Host "tvgpu-dod-live: verifier /standard tvgpu-dod.sys"
verifier /standard /driver tvgpu-dod.sys | Tee-Object (Join-Path $Out "verifier-set.txt")
verifier /query | Tee-Object (Join-Path $Out "verifier-query.txt")

function Invoke-DisableEnable {
    foreach ($dev in $gpu) {
        Disable-PnpDevice -InstanceId $dev.InstanceId -Confirm:$false -ErrorAction SilentlyContinue
        Start-Sleep -Milliseconds 200
        Enable-PnpDevice -InstanceId $dev.InstanceId -Confirm:$false -ErrorAction SilentlyContinue
        Start-Sleep -Milliseconds 200
    }
}

Write-Host "tvgpu-dod-live: one disable/enable cycle"
Invoke-DisableEnable
Write-Sidecar "disable/enable cycle done"

if ($env:TERNVALE_TVGPU_SLEEP -eq "1") {
    Write-Host "tvgpu-dod-live: sleep/resume (rundll32 SetSuspendState)"
    rundll32.exe powrprof.dll,SetSuspendState 0,1,0
}

Write-Host "tvgpu-dod-live: stress ${StressSeconds}s (disable/enable)"
$end = (Get-Date).AddSeconds($StressSeconds)
$cycles = 0
while ((Get-Date) -lt $end) {
    Invoke-DisableEnable
    $cycles++
    if (($cycles % 10) -eq 0) {
        Write-Sidecar "stress cycles=$cycles"
    }
}
Write-Sidecar "stress done cycles=$cycles seconds=$StressSeconds"

$flags = verifier /query | Out-String
if ($flags -match 'Violation') {
    throw "Driver Verifier reported a violation"
}

Write-Host "tvgpu-dod-live: PASS cycles=$cycles (reboot if Verifier was just enabled)"

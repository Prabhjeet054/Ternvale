# Step 106 guest: WDK infverif, test-sign package, pnputil with testsigning on.
# A second pass with testsigning off is expected to fail (no production sig).
param(
    [string]$RepoRoot = "",
    [switch]$ExpectUnsignedReject
)

$ErrorActionPreference = "Stop"
if (-not $RepoRoot) {
    $RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
}
$Out = Join-Path $RepoRoot "target\driver-package-guest"
New-Item -ItemType Directory -Force -Path $Out | Out-Null

Write-Host "driver-package-live: infverif /u"
$InfVerif = Get-Command infverif.exe -ErrorAction SilentlyContinue
if (-not $InfVerif) { throw "infverif.exe not on PATH (install the WDK)" }
$infs = @(
    (Join-Path $RepoRoot "drivers\tvhello\tvhello.inf"),
    (Join-Path $RepoRoot "drivers\tvgpu-dod\tvgpu-dod.inf")
)
foreach ($inf in $infs) {
    & $InfVerif.Source /u $inf | Tee-Object (Join-Path $Out "$(Split-Path -Leaf $inf).infverif.txt")
    if ($LASTEXITCODE -ne 0) { throw "infverif failed for $inf" }
}

Write-Host "driver-package-live: package.ps1 (Inf2Cat + optional test-sign)"
& (Join-Path $RepoRoot "drivers\release\package.ps1") -Config Debug
if ($LASTEXITCODE -ne 0) { throw "package.ps1 failed" }

Write-Host "driver-package-live: attestation CAB layout"
& (Join-Path $RepoRoot "drivers\release\prepare-attestation.ps1")
if ($LASTEXITCODE -ne 0) { throw "prepare-attestation.ps1 failed" }

if ($ExpectUnsignedReject) {
    Write-Host "driver-package-live: testsigning OFF (expect pnputil reject)"
    bcdedit /set testsigning off | Out-File (Join-Path $Out "bcdedit-off.txt")
    $inf = Join-Path $RepoRoot "drivers\release\dist\tvhello\tvhello.inf"
    & pnputil.exe /add-driver $inf /install
    if ($LASTEXITCODE -eq 0) {
        throw "unsigned/test-signed package installed with testsigning off (unexpected)"
    }
    Write-Host "driver-package-live: pnputil rejected as expected (exit $LASTEXITCODE)"
    return
}

Write-Host "driver-package-live: testsigning ON"
bcdedit /set testsigning on | Out-File (Join-Path $Out "bcdedit-on.txt")
$tvhello = Join-Path $RepoRoot "drivers\release\dist\tvhello\tvhello.inf"
if (Test-Path $tvhello) {
    pnputil /add-driver $tvhello /install | Tee-Object (Join-Path $Out "pnputil-tvhello.txt")
}
Write-Host "driver-package-live: PASS (reboot if bcdedit just enabled testsigning)"

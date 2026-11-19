# Build, stamp, infverif, Inf2Cat, and *test-sign* Ternvale guest drivers.
# Certificates stay in the guest keychain / env. Never commit .pfx or passwords.
#
#   TERNVALE_TESTSIGN_PFX      path to a test certificate PFX (optional)
#   TERNVALE_TESTSIGN_PASSWORD password via env only (optional; empty PFX ok)
#   TERNVALE_TESTSIGN_SHA1     SHA1 of a cert already in the store
param(
    [ValidateSet("Debug", "Release")]
    [string]$Config = "Release",
    [string]$OutDir = ""
)

$ErrorActionPreference = "Stop"
$ReleaseRoot = Split-Path -Parent $MyInvocation.MyCommand.Path
$RepoRoot = Split-Path -Parent (Split-Path -Parent $ReleaseRoot)
if (-not $OutDir) {
    $OutDir = Join-Path $ReleaseRoot "dist"
}

New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
Write-Host "package: build drivers"
& (Join-Path $RepoRoot "drivers\build.ps1") -Config $Config
if ($LASTEXITCODE -ne 0) { throw "drivers/build.ps1 failed" }

$python = Get-Command python3 -ErrorAction SilentlyContinue
if (-not $python) { $python = Get-Command python -ErrorAction SilentlyContinue }
if (-not $python) { throw "python3 required to stamp DriverVer" }

$infs = @(
    (Join-Path $RepoRoot "drivers\tvhello\tvhello.inf"),
    (Join-Path $RepoRoot "drivers\tvgpu-dod\tvgpu-dod.inf")
)
foreach ($inf in $infs) {
    & $python.Source (Join-Path $ReleaseRoot "infverif_dch.py") $inf
    if ($LASTEXITCODE -ne 0) { throw "DCH INF check failed for $inf" }
}

$InfVerif = Get-Command infverif.exe -ErrorAction SilentlyContinue
if ($InfVerif) {
    Write-Host "package: infverif /u"
    foreach ($inf in $infs) {
        & $InfVerif.Source /u $inf
        if ($LASTEXITCODE -ne 0) { throw "infverif failed for $inf" }
    }
} else {
    Write-Host "package: SKIP WDK infverif.exe (host analog already ran)"
}

function Copy-Built([string]$Name) {
    $sys = Get-ChildItem -Path (Join-Path $RepoRoot "drivers\$Name") -Filter "$Name.sys" -Recurse -ErrorAction SilentlyContinue |
        Where-Object { $_.FullName -match "ARM64" } |
        Select-Object -First 1
    $dest = Join-Path $OutDir $Name
    New-Item -ItemType Directory -Force -Path $dest | Out-Null
    if ($sys) {
        Copy-Item -Force $sys.FullName (Join-Path $dest "$Name.sys")
        $pdb = [IO.Path]::ChangeExtension($sys.FullName, ".pdb")
        if (Test-Path $pdb) { Copy-Item -Force $pdb (Join-Path $dest "$Name.pdb") }
    } else {
        Write-Warning "no $Name.sys (WDK output missing)"
    }
    Copy-Item -Force (Join-Path $RepoRoot "drivers\$Name\$Name.inf") (Join-Path $dest "$Name.inf")
    & $python.Source (Join-Path $ReleaseRoot "stamp.py") --in-place (Join-Path $dest "$Name.inf")
}

Copy-Built "tvhello"
Copy-Built "tvgpu-dod"

$Inf2Cat = Get-Command inf2cat.exe -ErrorAction SilentlyContinue
if ($Inf2Cat) {
    Get-ChildItem $OutDir -Directory | ForEach-Object {
        Write-Host "package: Inf2Cat $($_.Name)"
        & $Inf2Cat.Source /driver:$($_.FullName) /os:10_NI_ARM64 /verbose
        if ($LASTEXITCODE -ne 0) { throw "Inf2Cat failed for $($_.Name)" }
    }
} else {
    Write-Host "package: SKIP Inf2Cat (WDK not on PATH)"
}

$SignTool = Get-Command signtool.exe -ErrorAction SilentlyContinue
$Pfx = $env:TERNVALE_TESTSIGN_PFX
$Sha = $env:TERNVALE_TESTSIGN_SHA1
if ($SignTool -and ($Pfx -or $Sha)) {
    $files = Get-ChildItem $OutDir -Include *.sys, *.cat -Recurse
    foreach ($f in $files) {
        if ($Pfx) {
            if ($env:TERNVALE_TESTSIGN_PASSWORD) {
                & $SignTool.Source sign /fd SHA256 /td SHA256 /tr http://timestamp.digicert.com /f $Pfx /p $env:TERNVALE_TESTSIGN_PASSWORD $f.FullName
            } else {
                & $SignTool.Source sign /fd SHA256 /td SHA256 /tr http://timestamp.digicert.com /f $Pfx $f.FullName
            }
        } else {
            & $SignTool.Source sign /fd SHA256 /td SHA256 /tr http://timestamp.digicert.com /sha1 $Sha $f.FullName
        }
        if ($LASTEXITCODE -ne 0) { throw "signtool failed for $($f.Name)" }
    }
    Write-Host "package: test-signed $($files.Count) files (do not commit the PFX)"
} else {
    Write-Host "package: SKIP test-sign (set TERNVALE_TESTSIGN_PFX or TERNVALE_TESTSIGN_SHA1)"
}

Write-Host "package: dist $OutDir"
Get-ChildItem $OutDir -Recurse | Select-Object FullName

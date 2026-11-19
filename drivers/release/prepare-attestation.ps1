# Prepare a Partner Center *attestation* CAB. Does not submit and does not
# store EV certificates. Manual upload: see ATTESTATION.md.
#
# Layout (Microsoft Learn: Attestation sign Windows drivers):
#   each driver folder < 40 chars, no UNC paths, same architecture (ARM64),
#   .sys + .inf + .pdb + .cat. Microsoft regenerates the catalog.
param(
    [string]$DistDir = "",
    [string]$OutCab = ""
)

$ErrorActionPreference = "Stop"
$ReleaseRoot = Split-Path -Parent $MyInvocation.MyCommand.Path
if (-not $DistDir) { $DistDir = Join-Path $ReleaseRoot "dist" }
$Stage = Join-Path $ReleaseRoot "attestation-stage"
if (-not $OutCab) { $OutCab = Join-Path $ReleaseRoot "ternvale-drivers-arm64.cab" }

if (-not (Test-Path $DistDir)) {
    throw "dist missing; run package.ps1 first: $DistDir"
}

if (Test-Path $Stage) { Remove-Item -Recurse -Force $Stage }
New-Item -ItemType Directory -Force -Path $Stage | Out-Null

Get-ChildItem $DistDir -Directory | ForEach-Object {
    if ($_.Name.Length -ge 40) { throw "folder name too long for attestation: $($_.Name)" }
    if ($_.Name -match '[^A-Za-z0-9._-]') { throw "special characters in folder $($_.Name)" }
    $dest = Join-Path $Stage $_.Name
    Copy-Item -Recurse $_.FullName $dest
}

$Ddf = Join-Path $ReleaseRoot "attestation.ddf"
@"
.OPTION EXPLICIT
.Set CabinetNameTemplate=$(Split-Path -Leaf $OutCab)
.Set DiskDirectoryTemplate=$(Split-Path -Parent $OutCab)
.Set CompressionType=MSZIP
.Set UniqueFiles=OFF
.Set Cabinet=ON
.Set DiskDirectory1=$(Split-Path -Parent $OutCab)
"@ | Set-Content -Encoding ASCII $Ddf

Get-ChildItem $Stage -Recurse -File | ForEach-Object {
    Add-Content -Encoding ASCII $Ddf ("`"{0}`"" -f $_.FullName)
}

Write-Host "prepare-attestation: makecab"
& makecab.exe /f $Ddf
if ($LASTEXITCODE -ne 0) { throw "makecab failed" }

Write-Host "CAB ready: $OutCab"
Write-Host "Next: EV-sign this CAB, then Partner Center > Submit new hardware (ATTESTATION.md)."
Write-Host "Do not check in the EV PFX or the signed CAB if it contains secrets."

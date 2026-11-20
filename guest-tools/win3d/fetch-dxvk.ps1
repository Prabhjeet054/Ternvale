# Fetch DXVK license (and optionally a release tarball) on the user's machine.
# DXVK is zlib/libpng (Philip Rebohle et al.). Not linked into Ternvale.
# Does not vendor binaries into git. User must type "yes".
param(
    [switch]$Tarball
)

$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent $MyInvocation.MyCommand.Path
$Out = Join-Path $Root "third_party"
New-Item -ItemType Directory -Force -Path $Out | Out-Null

Write-Host "DXVK (https://github.com/doitsujin/dxvk) is zlib/libpng."
Write-Host "Official binaries are Wine x86/x64, not Windows ARM64 native."
Write-Host "Ternvale will not copy DXVK into the VMM or Ternvale.app."
$Ans = Read-Host "Type yes to download the upstream LICENSE"
if ($Ans -ne "yes") {
    throw "Aborted (consent required)"
}

$LicenseUrl = "https://raw.githubusercontent.com/doitsujin/dxvk/master/LICENSE"
$LicensePath = Join-Path $Out "DXVK-LICENSE"
Invoke-WebRequest -Uri $LicenseUrl -OutFile $LicensePath -UseBasicParsing
$Text = Get-Content -Raw $LicensePath
if ($Text -notmatch "zlib" -and $Text -notmatch "AS IS") {
    throw "LICENSE did not look like zlib/libpng; inspect $LicensePath"
}
Write-Host "Wrote $LicensePath"

if ($Tarball) {
    Write-Host "ARM64 Windows cannot use the stock x64 Wine tarball."
    Write-Host "Build dxvk from source in the guest if you need native DLLs."
    throw "Refusing to fetch x64 DXVK binaries for an ARM64 guest"
}

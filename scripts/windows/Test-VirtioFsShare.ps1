# In-guest checks for Step 76 (run over SSH as Administrator).
# Expects the virtio-fs share on Z: and a host-visible share root.
$ErrorActionPreference = 'Stop'
$Drive = 'Z:'
if (-not (Test-Path -LiteralPath $Drive)) {
    throw "share drive $Drive is missing"
}
Write-Output "drive=$Drive present"

$hostHello = Join-Path $Drive 'from_host.txt'
if (-not (Test-Path -LiteralPath $hostHello)) {
    throw "host file not visible on $Drive"
}
Write-Output "host_file=$hostHello"

Set-Content -LiteralPath (Join-Path $Drive 'from_guest.txt') -Value 'guest-v1' -NoNewline
Set-Content -LiteralPath (Join-Path $Drive 'my file.txt') -Value 'spaces' -NoNewline
Set-Content -LiteralPath (Join-Path $Drive 'café 世界.txt') -Value 'unicode' -NoNewline

$con = $null
try {
    New-Item -LiteralPath (Join-Path $Drive 'CON') -ItemType File -ErrorAction Stop | Out-Null
    $con = 'created'
} catch {
    $con = "rejected:$($_.Exception.Message)"
}
Write-Output "con=$con"

$aux = $null
try {
    New-Item -LiteralPath (Join-Path $Drive 'aux.txt') -ItemType File -ErrorAction Stop | Out-Null
    $aux = 'created'
} catch {
    $aux = "rejected:$($_.Exception.Message)"
}
Write-Output "aux=$aux"

if (Test-Path -LiteralPath (Join-Path $Drive 'host_to_guest.bin')) {
    $sum = (Get-FileHash -Algorithm SHA256 -LiteralPath (Join-Path $Drive 'host_to_guest.bin')).Hash.ToLower()
    Write-Output "host_to_guest_sha256=$sum"
    Copy-Item -LiteralPath (Join-Path $Drive 'host_to_guest.bin') (Join-Path $Drive 'guest_to_host.bin')
    $sum2 = (Get-FileHash -Algorithm SHA256 -LiteralPath (Join-Path $Drive 'guest_to_host.bin')).Hash.ToLower()
    Write-Output "guest_to_host_sha256=$sum2"
}

Remove-Item -LiteralPath $hostHello -ErrorAction SilentlyContinue
Write-Output 'guest_checks_done'

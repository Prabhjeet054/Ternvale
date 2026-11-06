# Silent tools install from ternvale-tools.iso (run as Administrator).
# Installs ARM64 virtio-win INF packages via pnputil, then registers the
# Ternvale guest agent service (`ternvale-agent.exe --install`).
$ErrorActionPreference = 'Stop'
$Root = Split-Path -Parent $MyInvocation.MyCommand.Path
$Log = Join-Path $env:TEMP 'ternvale-tools-install.log'
Start-Transcript -Path $Log -Append | Out-Null
try {
    $Drivers = Join-Path $Root 'virtio-win'
    if (Test-Path -LiteralPath $Drivers) {
        Get-ChildItem -LiteralPath $Drivers -Filter '*.inf' -Recurse -ErrorAction SilentlyContinue |
            Where-Object { $_.FullName -match '\\ARM64\\' } |
            ForEach-Object {
                Write-Host "pnputil $($_.FullName)"
                & pnputil.exe /add-driver $_.FullName /install
                if ($LASTEXITCODE -ne 0) {
                    Write-Warning "pnputil failed for $($_.FullName) (exit $LASTEXITCODE)"
                }
            }
    }
    else {
        Write-Warning "no virtio-win directory on this ISO"
    }

    $Agent = Join-Path $Root 'agent\ternvale-agent.exe'
    if (-not (Test-Path -LiteralPath $Agent)) {
        throw "missing $Agent"
    }
    Write-Host "installing TernvaleGuestAgent"
    & $Agent --install
    if ($LASTEXITCODE -ne 0) {
        throw "ternvale-agent --install failed (exit $LASTEXITCODE)"
    }
    $svc = Get-Service -Name 'TernvaleGuestAgent' -ErrorAction SilentlyContinue
    if ($svc -and $svc.Status -ne 'Running') {
        Start-Service -Name 'TernvaleGuestAgent'
    }

    $stamp = Join-Path $env:ProgramData 'Ternvale'
    New-Item -ItemType Directory -Force -Path $stamp | Out-Null
    Copy-Item -Force (Join-Path $Root 'MANIFEST.json') (Join-Path $stamp 'tools-manifest.json')
    Write-Host "Ternvale tools installed. Manifest copied to $stamp\tools-manifest.json"
}
finally {
    Stop-Transcript | Out-Null
}

# Silent tools install from ternvale-tools.iso (run as Administrator).
# Installs ARM64 virtio-win INF packages via pnputil, WinFsp (unmodified MSI),
# copies virtiofs.exe, registers demand-start VirtioFsSvc, then the
# TernvaleGuestAgent service (`ternvale-agent.exe --install`).
# WinFsp / virtio-win terms: docs/LICENSES.md on the host tree.
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

    $Winfsp = Join-Path $Root 'winfsp\winfsp.msi'
    if (Test-Path -LiteralPath $Winfsp) {
        Write-Host "installing WinFsp from $Winfsp"
        & msiexec.exe /i $Winfsp /qn /norestart
        if ($LASTEXITCODE -ne 0 -and $LASTEXITCODE -ne 3010) {
            throw "WinFsp msiexec failed (exit $LASTEXITCODE)"
        }
    }
    else {
        Write-Warning "no winfsp\winfsp.msi on this ISO; virtio-fs will not mount until WinFsp is installed (docs/LICENSES.md)"
    }

    $Dest = Join-Path ${env:ProgramFiles} 'Ternvale'
    New-Item -ItemType Directory -Force -Path $Dest | Out-Null
    $Viofs = $null
    if (Test-Path -LiteralPath $Drivers) {
        $Viofs = Get-ChildItem -LiteralPath $Drivers -Filter 'virtiofs.exe' -Recurse -ErrorAction SilentlyContinue |
            Where-Object { $_.FullName -match '\\ARM64\\' } |
            Select-Object -First 1
    }
    if ($Viofs) {
        Copy-Item -Force $Viofs.FullName (Join-Path $Dest 'virtiofs.exe')
        Write-Host "copied virtiofs.exe to $Dest"
    }
    else {
        Write-Warning "virtiofs.exe not found under virtio-win (ARM64)"
    }

    $Exe = Join-Path $Dest 'virtiofs.exe'
    if (Test-Path -LiteralPath $Exe) {
        $existing = Get-Service -Name 'VirtioFsSvc' -ErrorAction SilentlyContinue
        $bin = "`"$Exe`""
        if (-not $existing) {
            Write-Host "registering VirtioFsSvc (demand start; agent supplies -m/-t at logon)"
            & sc.exe create VirtioFsSvc binPath= $bin start= demand depend= VirtioFsDrv DisplayName= "VirtIO-FS"
            if ($LASTEXITCODE -ne 0) {
                Write-Warning "sc create VirtioFsSvc failed (exit $LASTEXITCODE)"
            }
        }
        else {
            Write-Host "VirtioFsSvc already registered"
        }
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

# Step 108 guest: vkcube.exe + D3D11/DXVK. Fail closed; do not claim Metal.
$ErrorActionPreference = "Continue"
$LogDir = "C:\ProgramData\Ternvale\driver-logs"
New-Item -ItemType Directory -Force -Path $LogDir | Out-Null
$Out = Join-Path $LogDir "tvvk-live.txt"

function Note([string]$Line) {
    Add-Content -Path $Out -Value $Line
    Write-Host $Line
}

Note "tvvk-live start $(Get-Date -Format o)"

$Icd = Join-Path $env:SystemRoot "System32\tvvk.dll"
if (Test-Path $Icd) {
    Note "tvvk_dll=present"
} else {
    Note "tvvk_dll=missing (ICD not installed; 2D KMDOD fallback)"
}

$VkCube = Get-Command vkcube.exe -ErrorAction SilentlyContinue
if (-not $VkCube) {
    $Guess = @(
        "$env:VULKAN_SDK\Bin\vkcube.exe",
        "$env:ProgramFiles\VulkanSDK\*\Bin\vkcube.exe"
    )
    $VkCube = Get-Item -ErrorAction SilentlyContinue $Guess | Select-Object -First 1
}
if ($VkCube) {
    Note "vkcube_path=$($VkCube.FullName)"
    $p = Start-Process -FilePath $VkCube.FullName -PassThru -WindowStyle Minimized
    Start-Sleep -Seconds 5
    if (-not $p.HasExited) {
        Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue
        Note "vkcube=launched_not_verified_metal (WSI/Venus missing in tvvk prototype)"
    } else {
        Note "vkcube=exited_code=$($p.ExitCode) (expected FEATURE_NOT_PRESENT / no device)"
    }
} else {
    Note "vkcube=not_installed"
}

$Tri = Join-Path $PSScriptRoot "d3d11_triangle.exe"
if (Test-Path $Tri) {
    $p = Start-Process -FilePath $Tri -PassThru -WindowStyle Minimized
    Start-Sleep -Seconds 3
    if (-not $p.HasExited) {
        Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue
    }
    Note "d3d11=ran (WARP unless native DXVK ARM64 d3d11.dll is beside the exe)"
} else {
    Note "d3d11=sample_not_built"
}

$Dxvk = Get-ChildItem -ErrorAction SilentlyContinue -Path $PSScriptRoot, (Join-Path $PSScriptRoot "third_party") -Filter "d3d11.dll"
if ($Dxvk) {
    Note "dxvk=dll_present $($Dxvk[0].FullName) (ARM64 native required; Wine x64 will not load)"
} else {
    Note "dxvk=not_fetched"
}

Note "metal=not_claimed_from_guest"
Note "fallback=tvgpu-dod (display-only) when TERNVALE_GPU3D=0 or \\\\.\\TVGPU3D missing"
Note "tvvk-live done"
exit 0

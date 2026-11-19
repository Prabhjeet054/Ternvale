# Shared ARM64 KMDF build. Run inside Windows 11 ARM64 with VS + WDK.
param(
    [ValidateSet("Debug", "Release")]
    [string]$Config = "Debug",
    [string]$Project = "",
    [switch]$ScratchBugcheck
)

$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent $MyInvocation.MyCommand.Path
$Projects = @()
if ($Project) {
    $Projects = @($Project)
} else {
    $Projects = @(
        (Join-Path $Root "tvhello\tvhello.vcxproj"),
        (Join-Path $Root "tvgpu-dod\tvgpu-dod.vcxproj")
    )
}

$VsWhere = Join-Path ${env:ProgramFiles(x86)} "Microsoft Visual Studio\Installer\vswhere.exe"
if (-not (Test-Path $VsWhere)) {
    throw "vswhere.exe not found. Install Visual Studio 2022 (or later) with the ARM64 WDK."
}

$VsPath = & $VsWhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.ARM64 -property installationPath
if (-not $VsPath) {
    throw "No Visual Studio install with VC ARM64 tools. Add the ARM64 C++ workload and WDK."
}

$MsBuild = Join-Path $VsPath "MSBuild\Current\Bin\amd64\MSBuild.exe"
if (-not (Test-Path $MsBuild)) {
    $MsBuild = Join-Path $VsPath "MSBuild\Current\Bin\MSBuild.exe"
}
if (-not (Test-Path $MsBuild)) {
    throw "MSBuild.exe not found under $VsPath"
}

$ScratchProp = "false"
if ($ScratchBugcheck) {
    Write-Host "SCRATCH: TVHELLO_SCRATCH_BUGCHECK (deliberate KeBugCheckEx on IOCTL)"
    $ScratchProp = "true"
}

foreach ($Proj in $Projects) {
    Write-Host "Building $Proj ($Config|ARM64)"
    & $MsBuild $Proj /p:Configuration=$Config /p:Platform=ARM64 /p:TreatWarningAsError=true /p:TvHelloScratchBugcheck=$ScratchProp /m
    if ($LASTEXITCODE -ne 0) {
        throw "MSBuild failed with $LASTEXITCODE for $Proj"
    }
}

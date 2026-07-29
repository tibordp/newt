# Builds the Windows MSI with WiX 6. The wix dotnet tool is pinned in
# .config/dotnet-tools.json at the repo root; requires the .NET SDK.
#
#   packaging/windows/build-msi.ps1 -Exe target\release\newt.exe `
#       -AgentsDir agents -Version 0.1.0 -Arch x64
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$Exe,
    [Parameter(Mandatory)][string]$AgentsDir,
    [Parameter(Mandatory)][string]$Version,
    [Parameter(Mandatory)][ValidateSet('x64', 'arm64')][string]$Arch,
    [string]$OutDir = '.'
)
$ErrorActionPreference = 'Stop'

$src = $PSScriptRoot
$repo = (Resolve-Path (Join-Path $src '..\..')).Path
$Exe = (Resolve-Path $Exe).Path
$AgentsDir = (Resolve-Path $AgentsDir).Path
if (-not (Test-Path $OutDir)) { New-Item -ItemType Directory $OutDir | Out-Null }
$out = Join-Path (Resolve-Path $OutDir).Path "Newt_${Version}_${Arch}_en-US.msi"

Push-Location $repo
try {
    dotnet tool restore
    if ($LASTEXITCODE -ne 0) { throw 'dotnet tool restore failed' }
    # --acceptEula wix7: OSMF EULA, accepted as a fee-exempt open-source project.
    dotnet tool run wix -- --acceptEula wix7 extension add WixToolset.UI.wixext/7.0.0
    if ($LASTEXITCODE -ne 0) { throw 'wix extension add failed' }
    dotnet tool run wix -- --acceptEula wix7 build `
        -arch $Arch `
        -culture en-US `
        -ext WixToolset.UI.wixext `
        -d "Version=$Version" `
        -d "NewtExe=$Exe" `
        -d "AgentsDir=$AgentsDir" `
        -d "SrcDir=$src" `
        -o $out `
        "$src\newt.wxs" "$src\ui.wxs" "$src\installdir-dlg.wxs"
    if ($LASTEXITCODE -ne 0) { throw 'wix build failed' }
} finally {
    Pop-Location
}
Write-Output $out

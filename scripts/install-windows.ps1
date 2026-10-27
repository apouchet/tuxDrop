# Builds tuxDrop and installs it for the current user (see
# packaging\windows\install.ps1), or removes it.
#
#   powershell -ExecutionPolicy Bypass -File scripts\install-windows.ps1             build, install, start
#   powershell -ExecutionPolicy Bypass -File scripts\install-windows.ps1 -Uninstall  stop and remove

param([switch]$Uninstall)

$ErrorActionPreference = 'Stop'
$Root = Split-Path $PSScriptRoot -Parent

if ($Uninstall) {
    & (Join-Path $Root 'packaging\windows\install.ps1') -Uninstall
    exit $LASTEXITCODE
}

& (Join-Path $PSScriptRoot 'package-windows.ps1')
$stage = Get-ChildItem (Join-Path $Root 'dist') -Directory -Filter 'tuxDrop-*-windows-*' |
    Sort-Object LastWriteTime -Descending | Select-Object -First 1
& (Join-Path $stage.FullName 'install.ps1')

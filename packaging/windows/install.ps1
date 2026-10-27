# Installs tuxDrop for the current user from this folder (built by
# scripts/package-windows.ps1), or removes it with -Uninstall.
#
#   install.cmd               (or: powershell -ExecutionPolicy Bypass -File install.ps1)
#   install.cmd -Uninstall    keeps the identity, trusted devices and settings
#
# Installs in %LOCALAPPDATA%\Programs\tuxDrop, adds it to PATH (tuxdrop
# command), starts tuxdropd at login and from the Start menu, and registers the
# sparse package that puts tuxDrop in the Share sheet of Explorer. Asks once
# for administrator rights: to trust the package certificate
# (LocalMachine\TrustedPeople) and to open the firewall to the local network
# for tuxDrop's programs.

param([switch]$Uninstall)

$ErrorActionPreference = 'Stop'
$Source = $PSScriptRoot
$Target = Join-Path $env:LOCALAPPDATA 'Programs\tuxDrop'
$RunKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
$Shortcut = Join-Path ([Environment]::GetFolderPath('Programs')) 'tuxDrop.lnk'
$Programs = 'tuxdropd.exe', 'tuxdrop.exe', 'tuxdrop-share.exe'

# Runs $Script as administrator (one UAC prompt); $Script gets $Args0.
function Invoke-Elevated([string]$Script, [string[]]$Arguments) {
    $quoted = ($Arguments | ForEach-Object { "'" + ($_ -replace "'", "''") + "'" }) -join ','
    $full = "`$ErrorActionPreference = 'Stop'; `$Args0 = @($quoted)`n$Script"
    $encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($full))
    $process = Start-Process powershell.exe -Verb RunAs -Wait -PassThru -WindowStyle Hidden `
        -ArgumentList '-NoProfile', '-ExecutionPolicy', 'Bypass', '-EncodedCommand', $encoded
    if ($process.ExitCode -ne 0) { throw "the administrator step failed (exit code $($process.ExitCode))" }
}

function Stop-Daemon {
    Get-Process tuxdropd, tuxdrop-share -ErrorAction SilentlyContinue | Stop-Process -Force
    Start-Sleep -Milliseconds 500
}

function Set-UserPath([bool]$Present) {
    $path = [Environment]::GetEnvironmentVariable('Path', 'User')
    $entries = @($path -split ';' | Where-Object { $_ -and $_ -ne $Target })
    if ($Present) { $entries += $Target }
    [Environment]::SetEnvironmentVariable('Path', ($entries -join ';'), 'User')
}

if ($Uninstall) {
    Stop-Daemon
    Get-AppxPackage -Name tuxDrop | Remove-AppxPackage
    Remove-ItemProperty -Path $RunKey -Name tuxDrop -ErrorAction SilentlyContinue
    Remove-Item 'HKCU:\Software\Classes\AppUserModelId\tuxDrop.Daemon' -Recurse -ErrorAction SilentlyContinue
    Remove-Item $Shortcut -ErrorAction SilentlyContinue
    Set-UserPath $false
    Invoke-Elevated @'
Remove-NetFirewallRule -Group tuxDrop -ErrorAction SilentlyContinue
Get-ChildItem Cert:\LocalMachine\TrustedPeople | Where-Object Subject -eq 'CN=tuxDrop' | Remove-Item
'@ @()
    Remove-Item $Target -Recurse -Force -ErrorAction SilentlyContinue
    Write-Host 'tuxDrop removed. Identity, trusted devices and settings are kept in' `
        (Join-Path $env:APPDATA 'tuxdrop')
    exit 0
}

foreach ($file in $Programs + 'resources.pri', 'tuxDrop.msix', 'tuxDrop.cer') {
    if (-not (Test-Path (Join-Path $Source $file))) { throw "$file is missing from $Source" }
}

Stop-Daemon
New-Item -ItemType Directory -Force $Target | Out-Null
foreach ($file in $Programs) { Copy-Item (Join-Path $Source $file) $Target -Force }
Copy-Item (Join-Path $Source 'Assets') $Target -Recurse -Force
Copy-Item (Join-Path $Source 'resources.pri') $Target -Force

# Administrator step: trust the certificate the package is signed with, and
# let other devices of the local network reach tuxDrop (discovery and
# transfers, port 53317), whatever the network profile.
$certificate = New-Object Security.Cryptography.X509Certificates.X509Certificate2 (Join-Path $Source 'tuxDrop.cer')
$trusted = Get-ChildItem Cert:\LocalMachine\TrustedPeople | Where-Object Thumbprint -eq $certificate.Thumbprint
$rules = @(Get-NetFirewallRule -Group tuxDrop -ErrorAction SilentlyContinue)
$daemon = Join-Path $Target 'tuxdropd.exe'
$cli = Join-Path $Target 'tuxdrop.exe'
$rulesOk = $rules.Count -eq 2 -and @($rules | Get-NetFirewallApplicationFilter |
    Where-Object { $_.Program -eq $daemon -or $_.Program -eq $cli }).Count -eq 2
if (-not $trusted -or -not $rulesOk) {
    Write-Host 'Asking for administrator rights (certificate and firewall)...'
    Invoke-Elevated @'
$cer, $daemon, $cli = $Args0
Get-ChildItem Cert:\LocalMachine\TrustedPeople | Where-Object Subject -eq 'CN=tuxDrop' | Remove-Item
Import-Certificate -FilePath $cer -CertStoreLocation Cert:\LocalMachine\TrustedPeople | Out-Null
Remove-NetFirewallRule -Group tuxDrop -ErrorAction SilentlyContinue
foreach ($program in $daemon, $cli) {
    New-NetFirewallRule -DisplayName ('tuxDrop (' + [IO.Path]::GetFileName($program) + ')') -Group tuxDrop `
        -Direction Inbound -Action Allow -Program $program -Profile Any -RemoteAddress LocalSubnet | Out-Null
}
'@ @((Join-Path $Source 'tuxDrop.cer'), $daemon, $cli)
}

# The Share sheet entry.
Get-AppxPackage -Name tuxDrop | Remove-AppxPackage
Add-AppxPackage -Path (Join-Path $Source 'tuxDrop.msix') -ExternalLocation $Target

Set-ItemProperty -Path $RunKey -Name tuxDrop -Value "`"$daemon`""
$shell = New-Object -ComObject WScript.Shell
$link = $shell.CreateShortcut($Shortcut)
$link.TargetPath = $daemon
$link.WorkingDirectory = $Target
$link.Description = 'Send and receive files with nearby devices'
$link.Save()
Set-UserPath $true

Start-Process $daemon
Write-Host "tuxDrop installed in $Target and started (notification area)."
Write-Host 'Share files from Explorer: right-click, Share, tuxDrop. Open a new terminal for the tuxdrop command.'

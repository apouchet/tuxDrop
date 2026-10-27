# Builds the Windows distribution: dist\tuxDrop-<version>-windows-<arch>\ and
# its .zip, holding the programs, the signed sparse package that adds tuxDrop
# to the Share sheet (tuxDrop.msix), its certificate (tuxDrop.cer) and the
# installer (install.cmd / install.ps1).
#
#   powershell -ExecutionPolicy Bypass -File scripts\package-windows.ps1
#
# Needs Rust (rustup, MSVC toolchain) and the Windows SDK (MakeAppx, SignTool),
# installed with the Visual Studio Build Tools "Desktop development with C++".
#
# The package is signed with a self-signed certificate "CN=tuxDrop", created in
# the current user's store (Cert:\CurrentUser\My) on first use and reused
# after: keep it to sign updates that installed copies already trust.

$ErrorActionPreference = 'Stop'
$Root = Split-Path $PSScriptRoot -Parent
Set-Location $Root

$version = (Select-String -Path Cargo.toml -Pattern '^version = "(.+)"' |
    Select-Object -First 1).Matches[0].Groups[1].Value
$arch = switch ($env:PROCESSOR_ARCHITECTURE) { 'ARM64' { 'arm64' } default { 'x64' } }
$name = "tuxDrop-$version-windows-$arch"
$stage = Join-Path $Root "dist\$name"

function Find-SdkTool([string]$Tool) {
    $kits = Join-Path ${env:ProgramFiles(x86)} 'Windows Kits\10\bin'
    $found = Get-ChildItem $kits -Filter $Tool -Recurse -ErrorAction SilentlyContinue |
        Where-Object { $_.Directory.Name -eq 'x64' -and $_.Directory.Parent.Name -match '^\d+(\.\d+)+$' } |
        Sort-Object { [version]$_.Directory.Parent.Name } -Descending |
        Select-Object -First 1
    if (-not $found) { throw "$Tool not found: install the Windows SDK (Visual Studio Build Tools)" }
    $found.FullName
}

function Invoke-Checked([string]$Program) {
    & $Program @args
    if ($LASTEXITCODE -ne 0) { throw "$([IO.Path]::GetFileName($Program)) failed (exit code $LASTEXITCODE)" }
}

Invoke-Checked cargo build --release --workspace --locked

Remove-Item $stage -Recurse -Force -ErrorAction SilentlyContinue
$assets = New-Item -ItemType Directory -Force (Join-Path $stage 'Assets')
foreach ($program in 'tuxdropd.exe', 'tuxdrop.exe', 'tuxdrop-share.exe') {
    Copy-Item "target\release\$program" $stage
}
Copy-Item packaging\windows\install.ps1, packaging\windows\install.cmd $stage
$logos = @{ 'Square44x44Logo.png' = 44; 'Square150x150Logo.png' = 150; 'StoreLogo.png' = 50 }
# The Share sheet and the taskbar pick the icon of their size among these,
# drawn as is (unplated), through resources.pri; without them they show a
# blank tile.
foreach ($size in 16, 24, 32, 48, 256) {
    foreach ($form in '', '_altform-unplated', '_altform-lightunplated') {
        $logos["Square44x44Logo.targetsize-$size$form.png"] = $size
    }
}
foreach ($logo in $logos.GetEnumerator()) {
    $file = Join-Path $assets $logo.Key
    Start-Process "target\release\tuxdropd.exe" -ArgumentList '--write-icon', $logo.Value, "`"$file`"" -Wait
    if (-not (Test-Path $file)) { throw "cannot write $file" }
}

# The package itself holds the manifest and the logos only.
$layout = Join-Path $Root 'target\windows-package'
Remove-Item $layout -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $layout | Out-Null
Copy-Item $assets $layout -Recurse
(Get-Content packaging\windows\AppxManifest.xml -Raw) `
    -replace '@VERSION@', "$version.0" -replace '@ARCH@', $arch |
    Set-Content (Join-Path $layout 'AppxManifest.xml') -Encoding UTF8
$makepri = Find-SdkTool makepri.exe
$priconfig = Join-Path $Root 'target\priconfig.xml'
Invoke-Checked $makepri createconfig /cf $priconfig /dq en-US /pv 10.0.0 /o
Invoke-Checked $makepri new /pr $layout /cf $priconfig /mn (Join-Path $layout 'AppxManifest.xml') `
    /of (Join-Path $layout 'resources.pri') /o
# The package's files are looked up in its external location (the install
# folder): the index goes there too.
Copy-Item (Join-Path $layout 'resources.pri') $stage
$msix = Join-Path $stage 'tuxDrop.msix'
Invoke-Checked (Find-SdkTool makeappx.exe) pack /o /nv /d $layout /p $msix

$certificate = Get-ChildItem Cert:\CurrentUser\My |
    Where-Object { $_.Subject -eq 'CN=tuxDrop' -and $_.HasPrivateKey -and $_.NotAfter -gt (Get-Date) } |
    Sort-Object NotAfter -Descending | Select-Object -First 1
if (-not $certificate) {
    Write-Host 'Creating the signing certificate CN=tuxDrop (Cert:\CurrentUser\My)'
    $certificate = New-SelfSignedCertificate -Type Custom -Subject 'CN=tuxDrop' `
        -FriendlyName 'tuxDrop package signing' -KeyUsage DigitalSignature `
        -CertStoreLocation Cert:\CurrentUser\My -NotAfter (Get-Date).AddYears(10) `
        -TextExtension @('2.5.29.37={text}1.3.6.1.5.5.7.3.3', '2.5.29.19={text}')
}
Export-Certificate -Cert $certificate -FilePath (Join-Path $stage 'tuxDrop.cer') | Out-Null
Invoke-Checked (Find-SdkTool signtool.exe) sign /q /fd SHA256 /sha1 $certificate.Thumbprint /s My $msix

$zip = Join-Path $Root "dist\$name.zip"
Remove-Item $zip -ErrorAction SilentlyContinue
Compress-Archive -Path $stage -DestinationPath $zip
Write-Host "Built $stage and $zip"

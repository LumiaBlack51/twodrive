#requires -Version 5.1
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$Bundle,
    [string]$OutputDirectory = '',
    [string]$IdentityName = '',
    [string]$Publisher = '',
    [string]$PublisherDisplayName = '',
    [string]$Version = '',
    [string]$MakeAppx = '',
    [switch]$TestPackage
)
$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path -Parent $PSScriptRoot
$bundlePath = (Resolve-Path -LiteralPath $Bundle).Path
foreach ($name in @('twodrive_dav.exe', 'twodrive-dev.exe', 'twodrive-dav-engine.exe', 'twodrive-native-dev.exe', 'flutter_windows.dll', 'data\icudtl.dat', 'data\flutter_assets')) {
    if (-not (Test-Path -LiteralPath (Join-Path $bundlePath $name))) { throw "Incomplete Windows bundle: $name" }
}
if ($TestPackage) {
    if ($IdentityName -or $Publisher -or $PublisherDisplayName) { throw 'TestPackage cannot use a Store identity.' }
    $IdentityName = 'TwoDrive.Development.Test'
    # Microsoft's reserved unsigned-package OID allows explicit -AllowUnsigned
    # installation on Windows 11 without adding a certificate to a trust store.
    # It is exclusive to the test identity; Store packages use Partner Center.
    $Publisher = 'CN=TwoDrive Development Test, OID.2.25.311729368913984317654407730594956997722=1'
    $PublisherDisplayName = 'TwoDrive contributors (local test)'
} elseif (-not $IdentityName -or -not $Publisher -or -not $PublisherDisplayName) {
    throw 'Supply the exact IdentityName, Publisher and PublisherDisplayName from Partner Center, or use -TestPackage.'
}
if (-not $TestPackage -and ($IdentityName -ieq 'TwoDrive.Development.Test' -or
    $Publisher -match '(?i)(OID\.)?2\.25\.311729368913984317654407730594956997722\s*=')) {
    throw 'The reserved unsigned test identity/Publisher cannot be used for a Store package. Use -TestPackage or the exact Partner Center identity.'
}
if ($IdentityName -notmatch '^[A-Za-z0-9.-]{3,50}$' -or $Publisher -notmatch '^CN=') { throw 'Invalid package identity.' }
if (-not $Version) {
    $cargo = Get-Content -LiteralPath (Join-Path $projectRoot 'experiments\twodrive-dev\Cargo.toml') -Raw
    if ($cargo -notmatch '(?m)^version = "(\d+)\.(\d+)\.(\d+)"') { throw 'Cannot determine Development version.' }
    # Reserve Store major 1 for the pre-1.0 Development series. Override for later series.
    if ([int]$Matches[1] -ne 0) { throw 'Set an explicit Store Version for Development 1.0 and later.' }
    $Version = "1.$($Matches[2]).$($Matches[3]).0"
}
if ($Version -notmatch '^([1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)\.0$' -or
    @($Version.Split('.') | Where-Object { [long]$_ -gt 65535 }).Count) { throw 'Store Version must be M.m.b.0, M > 0, each field <= 65535.' }
if (-not $MakeAppx) {
    $sdk = Join-Path ${env:ProgramFiles(x86)} 'Windows Kits\10\bin'
    $candidate = Get-ChildItem -LiteralPath $sdk -Directory |
        Where-Object { $_.Name -match '^10\.0\.\d+\.0$' } |
        Sort-Object { [version]$_.Name } -Descending |
        ForEach-Object { Join-Path $_.FullName 'x64\makeappx.exe' } |
        Where-Object { Test-Path -LiteralPath $_ } | Select-Object -First 1
    if (-not $candidate) { throw 'Install the Windows 10/11 SDK with MakeAppx.' }
    $MakeAppx = $candidate
}
if (-not $OutputDirectory) { $OutputDirectory = Join-Path $projectRoot 'dist\msix' }
New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
$outputPath = (Resolve-Path -LiteralPath $OutputDirectory).Path
$suffix = if ($TestPackage) { '-test-unsigned' } else { '-store-unsigned' }
$package = Join-Path $outputPath "TwoDrive-Development-$Version-x64$suffix.msix"
if (Test-Path -LiteralPath $package) { throw "Output already exists: $package" }
$tempRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath())
$stage = Join-Path $tempRoot ('twodrive-msix-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $stage | Out-Null
try {
    Get-ChildItem -LiteralPath $bundlePath | Copy-Item -Destination $stage -Recurse -Force
    # A packaged app must not offer the portable self-copy installer.
    foreach ($name in @('install-desktop-windows.ps1', 'INSTALL.md')) {
        $portableFile = Join-Path $stage $name
        if (Test-Path -LiteralPath $portableFile) { Remove-Item -LiteralPath $portableFile -Force }
    }
    Copy-Item -LiteralPath (Join-Path $projectRoot 'LICENSE') -Destination (Join-Path $stage 'LICENSE')
    [IO.File]::WriteAllText((Join-Path $stage 'distribution-msix'), 'Updates are managed by the MSIX distribution channel.')
    $assets = Join-Path $stage 'Assets'
    New-Item -ItemType Directory -Force -Path $assets | Out-Null
    Add-Type -AssemblyName System.Drawing
    $icon = New-Object System.Drawing.Icon((Join-Path $projectRoot 'apps\dav\assets\cloud-connected.ico'), 256, 256)
    $original = $icon.ToBitmap()
    try {
        foreach ($entry in @(@('StoreLogo', 50), @('Square44x44Logo', 44), @('Square150x150Logo', 150))) {
            $bitmap = New-Object System.Drawing.Bitmap([int]$entry[1], [int]$entry[1])
            $graphics = [Drawing.Graphics]::FromImage($bitmap)
            try {
                $graphics.Clear([Drawing.Color]::Transparent)
                $graphics.InterpolationMode = [Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
                $graphics.DrawImage($original, 0, 0, [int]$entry[1], [int]$entry[1])
                $bitmap.Save((Join-Path $assets ($entry[0] + '.png')), [Drawing.Imaging.ImageFormat]::Png)
            } finally { $graphics.Dispose(); $bitmap.Dispose() }
        }
    } finally { $original.Dispose(); $icon.Dispose() }
    $manifest = Get-Content -LiteralPath (Join-Path $projectRoot 'packaging\msix\AppxManifest.xml.in') -Raw
    $manifest = $manifest.Replace('@IDENTITY@', [Security.SecurityElement]::Escape($IdentityName)).Replace('@PUBLISHER@', [Security.SecurityElement]::Escape($Publisher)).Replace('@PUBLISHER_DISPLAY@', [Security.SecurityElement]::Escape($PublisherDisplayName)).Replace('@VERSION@', $Version)
    [IO.File]::WriteAllText((Join-Path $stage 'AppxManifest.xml'), $manifest, (New-Object Text.UTF8Encoding($false)))
    & $MakeAppx pack /d $stage /p $package /o
    if ($LASTEXITCODE -ne 0) { throw "MakeAppx validation/pack failed: $LASTEXITCODE" }
    $hash = (Get-FileHash -LiteralPath $package -Algorithm SHA256).Hash.ToLowerInvariant()
    [IO.File]::WriteAllText(($package + '.sha256'), "$hash  $([IO.Path]::GetFileName($package))`n")
    Write-Host "Validated unsigned package: $package"
    Write-Host 'No certificate was trusted, no package was installed, and nothing was submitted to the Store.'
} finally {
    $resolvedStage = [IO.Path]::GetFullPath($stage)
    if (-not $resolvedStage.StartsWith($tempRoot, [StringComparison]::OrdinalIgnoreCase) -or
        [IO.Path]::GetFileName($resolvedStage) -notmatch '^twodrive-msix-[0-9a-f]{32}$') { throw 'Refusing unexpected staging cleanup path.' }
    Remove-Item -LiteralPath $resolvedStage -Recurse -Force
}

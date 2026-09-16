param(
    [ValidateSet("Both", "Full", "Lite")][string]$Edition = "Both",
    [Parameter(Mandatory=$true)][string]$OutputDirectory,
    [switch]$Offline
)
$ErrorActionPreference = "Stop"
Set-Location (Split-Path $PSScriptRoot -Parent)
if (Test-Path -LiteralPath $OutputDirectory) { throw "Output directory must be new; refusing to overwrite artifacts." }
$destination = [IO.Path]::GetFullPath($OutputDirectory)
function Check-Exit([string]$Step) { if ($LASTEXITCODE -ne 0) { throw "$Step failed ($LASTEXITCODE)" } }
cargo build -p twodrive-windows --release --locked
Check-Exit "Rust release"
cargo test -p twodrive-windows --locked
Check-Exit "Rust native tests"
$version = (& target\release\twodrive-engine.exe --version).Trim()
Check-Exit "version"
if ($Edition -ne "Lite") {
    Push-Location apps\full
    try {
        if ($Offline) { flutter pub get --offline --enforce-lockfile } else { flutter pub get --enforce-lockfile }
        Check-Exit "Flutter dependencies"
        flutter analyze
        Check-Exit "Flutter analysis"
        flutter test
        Check-Exit "Flutter state regression"
        flutter build windows --release --no-pub
        Check-Exit "Flutter release"
    } finally { Pop-Location }
    $uiVersion = (Select-String -Path apps\full\pubspec.yaml -Pattern '^version: (.+)$').Matches.Groups[1].Value.Split('+')[0]
    if ($uiVersion -ne $version) { throw "Full and engine versions differ" }
}
New-Item -ItemType Directory -Path $destination | Out-Null
$editions = if ($Edition -eq "Both") { @("Full","Lite") } else { @($Edition) }
foreach ($name in $editions) {
    $package = Join-Path $destination "TwoDrive-$version-$name-preview-win-x64"
    New-Item -ItemType Directory -Path $package | Out-Null
    Copy-Item target\release\twodrive-engine.exe,target\release\twodrive-tray.exe -Destination $package
    Copy-Item LICENSE -Destination $package
    Copy-Item doc\windows-preview.md -Destination (Join-Path $package "README.md")
    Copy-Item scripts\start-windows-preview.ps1 -Destination (Join-Path $package "Start.ps1")
    Set-Content -LiteralPath (Join-Path $package "edition.txt") -Value $name -Encoding ascii
    if ($name -eq "Full") {
        Copy-Item apps\full\build\windows\x64\runner\Release -Destination (Join-Path $package "ui") -Recurse
    } else {
        $unexpected = Get-ChildItem -LiteralPath $package -Recurse | Where-Object { $_.Name -match 'flutter|dart|\.dll$' }
        if ($unexpected) { throw "Lite unexpectedly contains a runtime/DLL" }
    }
    $files = @(Get-ChildItem -LiteralPath $package -File -Recurse | ForEach-Object {
        @{ path=[IO.Path]::GetRelativePath($package,$_.FullName); bytes=$_.Length; sha256=(Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLower() }
    })
    @{ version=$version; edition=$name; channel="preview"; architecture="x64"; signed=$false;
       native_sync_accepted=$false; ipc_version=1; files=$files } |
       ConvertTo-Json -Depth 6 | Set-Content -LiteralPath (Join-Path $package "manifest.json") -Encoding utf8
    Compress-Archive -LiteralPath $package -DestinationPath "$package.zip"
}
if ($editions.Count -eq 2) {
    $engineHashes = @(Get-ChildItem -LiteralPath $destination -Directory | ForEach-Object {
        (Get-FileHash -LiteralPath (Join-Path $_.FullName "twodrive-engine.exe")).Hash
    } | Select-Object -Unique)
    if ($engineHashes.Count -ne 1) { throw "Editions contain different engines" }
}
Get-ChildItem -LiteralPath $destination -Filter *.zip | ForEach-Object {
    "{0}  {1}" -f (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLower(), $_.Name
} | Set-Content -LiteralPath (Join-Path $destination "SHA256SUMS.txt") -Encoding ascii

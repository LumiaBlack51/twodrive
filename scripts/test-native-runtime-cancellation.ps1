[CmdletBinding()]
param([Parameter(Mandatory=$true)][string]$RootParent)
$ErrorActionPreference='Stop'
$tdParent=(Resolve-Path -LiteralPath $RootParent).ProviderPath
if ((Get-Item -LiteralPath $tdParent).Attributes -band [IO.FileAttributes]::ReparsePoint) { throw 'Ordinary explicit fixture parent required' }
$tdManifest=Join-Path $PSScriptRoot '../experiments/twodrive-dev/Cargo.toml'
$tdPrevious=$env:TWODRIVE_APPROVED_CANCELLATION_ROOT
try {
    foreach ($tdCase in @(
        'large_local_application_stops_between_receipts_and_resumes_without_duplicates',
        'large_remote_application_stops_after_durable_publications_and_resumes_same_root'
    )) {
        $tdRoot=[IO.Path]::GetFullPath((Join-Path $tdParent ('twodrive-cfapi-fixture-office-stop-'+[Guid]::NewGuid().ToString('N'))))
        if (!$tdRoot.StartsWith($tdParent.TrimEnd('\')+'\',[StringComparison]::OrdinalIgnoreCase) -or (Test-Path -LiteralPath $tdRoot)) {throw 'Fresh bounded fixture root required'}
        $env:TWODRIVE_APPROVED_CANCELLATION_ROOT=$tdRoot
        & cargo test --manifest-path $tdManifest --locked --features native-sync,native-test-hooks --lib ('native_coordinator::cancellation_tests::'+$tdCase) -- --exact --ignored --nocapture
        if ($LASTEXITCODE -ne 0) {throw ('Generated native cancellation test failed; retain fixture for diagnosis: '+$tdRoot)}
        if ((Test-Path -LiteralPath $tdRoot) -or (Test-Path -LiteralPath ([IO.Path]::ChangeExtension($tdRoot,'receipt.json')))) {throw 'Generated root/receipt was not retired by the passing test'}
    }
} finally { $env:TWODRIVE_APPROVED_CANCELLATION_ROOT=$tdPrevious }

param()
if ($PSVersionTable.PSEdition -eq 'Core') {
    & powershell.exe -NoProfile -NonInteractive -File $PSCommandPath
    if ($LASTEXITCODE -ne 0) {throw 'Windows PowerShell installer regression failed'}
    return
}
$ErrorActionPreference='Stop'
# Disposable executable/task only. No real account or native registration.
# Keep the generated evidence directory for explicit Recycle Bin cleanup.
$fixture=Join-Path $env:TEMP ('twodrive-startup-staging-'+[Guid]::NewGuid().ToString('N'))
$state=Join-Path $fixture 'state'
$inputDir=Join-Path $fixture 'input'
New-Item -ItemType Directory -Path $state,$inputDir | Out-Null
[IO.File]::WriteAllText((Join-Path $state 'native-owner.dat'),'generated-test-marker')
$source=Join-Path $inputDir 'twodrive-native-dev.exe'
$class='GeneratedRuntime'+[Guid]::NewGuid().ToString('N')
Add-Type -TypeDefinition ('public static class '+$class+' { public static int Main(string[] args) { return 0; } }') -OutputAssembly $source -OutputType ConsoleApplication
$taskName='TwoDriveDevNative-Staging-'+[Guid]::NewGuid().ToString('N')
$installer=Join-Path $PSScriptRoot 'install-native-runtime.ps1'
$sha=(Get-FileHash -LiteralPath $source -Algorithm SHA256).Hash.ToLowerInvariant()
$target=Join-Path $state ('runtime\'+$sha+'\twodrive-native-dev.exe')
$copyState=@{Reject=$true}
function Copy-Item {
    param([string]$LiteralPath,[string]$Destination)
    if ($copyState.Reject) {
        $copyState.Reject=$false
        [IO.File]::WriteAllText($Destination,'generated wrong staged bytes')
    } else { Microsoft.PowerShell.Management\Copy-Item -LiteralPath $LiteralPath -Destination $Destination }
}
try {
    $rejected=$false
    try { & $installer -NativeState $state -Binary $source -TaskName $taskName | Out-Null }
    catch { if ($_.Exception.Message -notlike '*Staged runtime hash differs*') {throw}; $rejected=$true }
    if (!$rejected -or (Test-Path -LiteralPath $target) -or (Get-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue)) {throw 'Rejected copy published a runtime/task'}
    # Simulate an invalid final file left by the earlier non-atomic installer.
    [IO.File]::WriteAllText($target,'generated invalid uncommitted version')
    & $installer -NativeState $state -Binary $source -TaskName $taskName | Out-Null
    if ($LASTEXITCODE -ne 0 -or (Get-FileHash -LiteralPath $target -Algorithm SHA256).Hash.ToLowerInvariant() -ne $sha) {throw 'Same intended version did not recover'}
    $task=Get-ScheduledTask -TaskName $taskName
    if ($task.Settings.Enabled) {throw 'Default installation enabled task'}
    if (@(Get-ChildItem -LiteralPath (Split-Path $target) -Filter 'rejected-*.exe').Count -ne 1) {throw 'Invalid candidate backup missing'}
    & $installer -NativeState $state -Binary $source -TaskName $taskName | Out-Null
    if ((Get-ScheduledTask -TaskName $taskName).Settings.Enabled) {throw 'Reinstallation enabled task'}
    [pscustomobject]@{passed=$true;fixture=$fixture;same_version_retry=$true;invalid_uncommitted_recovered=$true;default_disabled=$true}|ConvertTo-Json -Compress
} finally {
    $task=Get-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
    if ($task -and $task.Description -eq ('TwoDrive dev native runtime: '+$state)) {Unregister-ScheduledTask -TaskName $taskName -Confirm:$false}
}

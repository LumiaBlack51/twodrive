param(
    [Parameter(Mandatory=$true)][string]$NativeState,
    [Parameter(Mandatory=$true)][string]$Binary,
    [string]$TaskName='TwoDriveDevNative',
    [switch]$Enable,
    [switch]$StartNow
)
$ErrorActionPreference='Stop'
$state=(Resolve-Path -LiteralPath $NativeState).ProviderPath
$source=(Resolve-Path -LiteralPath $Binary).ProviderPath
if (!(Test-Path -LiteralPath (Join-Path $state 'native-owner.dat')) -or [IO.Path]::GetFileName($source) -ne 'twodrive-native-dev.exe') {throw 'Sealed native state and exact runtime binary required'}
if ($TaskName -notmatch '^TwoDriveDevNative(?:-[A-Za-z0-9-]+)?$') {throw 'Unexpected development task name'}
if ($StartNow -and !$Enable) {throw 'StartNow requires Enable'}
$existing=Get-ScheduledTask -TaskName $TaskName -ErrorAction SilentlyContinue
$description='TwoDrive dev native runtime: '+$state
if ($existing -and $existing.Description -ne $description) {throw 'An unrelated task owns this name'}
$runtimeDir=Join-Path $state 'runtime'
$sha=(Get-FileHash -LiteralPath $source -Algorithm SHA256).Hash.ToLowerInvariant()
$versionDir=Join-Path $runtimeDir $sha
if (!(Test-Path -LiteralPath $versionDir)) {New-Item -ItemType Directory -Path $versionDir | Out-Null}
$target=Join-Path $versionDir 'twodrive-native-dev.exe'
if (!(Test-Path -LiteralPath $target) -or (Get-FileHash -LiteralPath $target -Algorithm SHA256).Hash.ToLowerInvariant() -ne $sha) {
    $staging=Join-Path $versionDir ('candidate-'+[Guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $staging | Out-Null
    $candidate=Join-Path $staging 'twodrive-native-dev.exe'
    Copy-Item -LiteralPath $source -Destination $candidate
    if ((Get-FileHash -LiteralPath $candidate -Algorithm SHA256).Hash.ToLowerInvariant() -ne $sha) {throw 'Staged runtime hash differs; task preserved'}
    & $candidate verify --native-state $state
    if ($LASTEXITCODE -ne 0) {throw 'Staged candidate verification failed; task preserved'}
    if (Test-Path -LiteralPath $target) {
        # Retain the invalid uncommitted bytes for inspection; replace atomically.
        [IO.File]::Replace($candidate,$target,(Join-Path $versionDir ('rejected-'+[Guid]::NewGuid().ToString('N')+'.exe')))
    } else { [IO.File]::Move($candidate,$target) }
    [IO.Directory]::Delete($staging)
}
# Candidate verification precedes task changes. The previous executable remains
# at its original path, so failure cannot replace a working installation.
& $target verify --native-state $state
if ($LASTEXITCODE -ne 0) {throw 'Candidate root verification failed; previous task/binary preserved'}
function Quote-Literal([string]$value) {return "'"+$value.Replace("'","''")+"'"}
$log=Join-Path $state 'runtime.log'
$script='$ErrorActionPreference=''Stop''; & '+(Quote-Literal $target)+' run --native-state '+(Quote-Literal $state)+' *> '+(Quote-Literal $log)+'; exit $LASTEXITCODE'
$encoded=[Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($script))
$action=New-ScheduledTaskAction -Execute 'powershell.exe' -Argument ('-NoProfile -NonInteractive -WindowStyle Hidden -EncodedCommand '+$encoded)
$user=[Security.Principal.WindowsIdentity]::GetCurrent().Name
$principal=New-ScheduledTaskPrincipal -UserId $user -LogonType Interactive -RunLevel Limited
$trigger=New-ScheduledTaskTrigger -AtLogOn -User $user
$settings=New-ScheduledTaskSettingsSet -MultipleInstances IgnoreNew -ExecutionTimeLimit ([TimeSpan]::Zero) -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -RestartCount 3 -RestartInterval (New-TimeSpan -Minutes 1) -Disable
$task=New-ScheduledTask -Action $action -Trigger $trigger -Principal $principal -Settings $settings -Description $description
if ($existing) {
    Export-ScheduledTask -TaskName $TaskName | Set-Content -LiteralPath (Join-Path $state ('previous-native-startup-'+$TaskName+'.xml')) -Encoding UTF8
    Register-ScheduledTask -TaskName $TaskName -InputObject $task -Force | Out-Null
} else {Register-ScheduledTask -TaskName $TaskName -InputObject $task | Out-Null}
if ($Enable) {Enable-ScheduledTask -TaskName $TaskName | Out-Null} else {Disable-ScheduledTask -TaskName $TaskName | Out-Null}
$receipt=[pscustomobject]@{schema=1;task=$TaskName;state=$state;binary=$target;sha256=(Get-FileHash -LiteralPath $target -Algorithm SHA256).Hash;enabled=[bool]$Enable;installed_utc=[DateTime]::UtcNow.ToString('o')}
$receipt | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $state ('native-startup-receipt-'+$TaskName+'.json')) -Encoding UTF8
if ($StartNow) {Start-ScheduledTask -TaskName $TaskName}
$receipt | ConvertTo-Json -Compress

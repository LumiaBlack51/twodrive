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
if (!(Test-Path -LiteralPath $runtimeDir)) {New-Item -ItemType Directory -Path $runtimeDir | Out-Null}
$target=Join-Path $runtimeDir 'twodrive-native-dev.exe'
if (Get-CimInstance Win32_Process | Where-Object {$_.ExecutablePath -eq $target}) {throw 'Stop the current owned runtime before replacing its binary'}
if ($source -ne $target) {Copy-Item -LiteralPath $source -Destination $target}
& $target verify --native-state $state
if ($LASTEXITCODE -ne 0) {throw 'Native root verification failed; startup was not installed'}
function Quote-Literal([string]$value) {return "'"+$value.Replace("'","''")+"'"}
$log=Join-Path $state 'runtime.log'
$script='$ErrorActionPreference=''Stop''; & '+(Quote-Literal $target)+' run --native-state '+(Quote-Literal $state)+' *> '+(Quote-Literal $log)+'; exit $LASTEXITCODE'
$encoded=[Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($script))
$action=New-ScheduledTaskAction -Execute 'powershell.exe' -Argument ('-NoProfile -NonInteractive -WindowStyle Hidden -EncodedCommand '+$encoded)
$user=[Security.Principal.WindowsIdentity]::GetCurrent().Name
$principal=New-ScheduledTaskPrincipal -UserId $user -LogonType Interactive -RunLevel Limited
$trigger=New-ScheduledTaskTrigger -AtLogOn -User $user
$settings=New-ScheduledTaskSettingsSet -MultipleInstances IgnoreNew -ExecutionTimeLimit ([TimeSpan]::Zero) -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -RestartCount 3 -RestartInterval (New-TimeSpan -Minutes 1)
$task=New-ScheduledTask -Action $action -Trigger $trigger -Principal $principal -Settings $settings -Description $description
if ($existing) {
    Export-ScheduledTask -TaskName $TaskName | Set-Content -LiteralPath (Join-Path $state 'previous-native-startup.xml') -Encoding UTF8
    Set-ScheduledTask -TaskName $TaskName -InputObject $task | Out-Null
} else {Register-ScheduledTask -TaskName $TaskName -InputObject $task | Out-Null}
if ($Enable) {Enable-ScheduledTask -TaskName $TaskName | Out-Null} else {Disable-ScheduledTask -TaskName $TaskName | Out-Null}
$receipt=[pscustomobject]@{schema=1;task=$TaskName;state=$state;binary=$target;sha256=(Get-FileHash -LiteralPath $target -Algorithm SHA256).Hash;enabled=[bool]$Enable;installed_utc=[DateTime]::UtcNow.ToString('o')}
$receipt | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $state 'native-startup-receipt.json') -Encoding UTF8
if ($StartNow) {Start-ScheduledTask -TaskName $TaskName}
$receipt | ConvertTo-Json -Compress

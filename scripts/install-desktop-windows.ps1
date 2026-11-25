# Run from the extracted Windows portable directory. Autostart remains off.
[CmdletBinding()]
param([string]$Source = $PSScriptRoot)
$ErrorActionPreference = 'Stop'
$target = Join-Path $env:LOCALAPPDATA 'TwoDrive Development\runtime'
if (-not (Test-Path -LiteralPath (Join-Path $Source 'twodrive_dav.exe'))) { throw 'Extract the complete TwoDrive Development bundle first.' }
$running = Get-Process -Name twodrive_dav -ErrorAction SilentlyContinue
if ($running) { throw 'Close TwoDrive Development before updating it.' }
New-Item -ItemType Directory -Path $target -Force | Out-Null
Get-ChildItem -LiteralPath $Source | Where-Object { $_.Name -ne 'install-desktop-windows.ps1' } | Copy-Item -Destination $target -Recurse -Force
$shell = New-Object -ComObject WScript.Shell
$shortcut = $shell.CreateShortcut((Join-Path ([Environment]::GetFolderPath('Programs')) 'TwoDrive Development.lnk'))
$shortcut.TargetPath = Join-Path $target 'twodrive_dav.exe'
$shortcut.WorkingDirectory = $target
$shortcut.IconLocation = "$target\twodrive_dav.exe,0"
$shortcut.Save()
$serverShortcut = $shell.CreateShortcut((Join-Path ([Environment]::GetFolderPath('Programs')) 'TDLink Server.lnk'))
$serverShortcut.TargetPath = $shortcut.TargetPath
$serverShortcut.WorkingDirectory = $target
$serverShortcut.Arguments = '--tdlink-server'
$serverShortcut.Save()
Write-Host 'TwoDrive Development installed. Autostart was not enabled.'

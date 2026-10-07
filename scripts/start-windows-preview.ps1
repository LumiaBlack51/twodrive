param([Parameter(Mandatory=$true)][string]$StateDirectory, [switch]$Mock)
$ErrorActionPreference = "Stop"
if (![IO.Path]::IsPathFullyQualified($StateDirectory)) { throw "Use an explicit absolute disposable state directory." }
$edition = (Get-Content -LiteralPath (Join-Path $PSScriptRoot "edition.txt")).Trim()
$arguments = @("--state", ('"' + $StateDirectory + '"'))
if ($edition -eq "Full") { $arguments += "--full" }
if ($Mock) { $arguments += "--mock" }
Start-Process -FilePath (Join-Path $PSScriptRoot "twodrive-tray.exe") -ArgumentList $arguments -WindowStyle Hidden

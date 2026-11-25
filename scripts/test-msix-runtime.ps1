#requires -Version 5.1
[CmdletBinding()]
param(
    [Parameter(Mandatory=$true)][string]$Package,
    [Parameter(Mandatory=$true)][string]$Report,
    [string]$VcRuntime = ''
)
$ErrorActionPreference = 'Stop'
$packageName = 'TwoDrive.Development.Test'
$dependencyName = 'Microsoft.VCLibs.140.00.UWPDesktop'
$os = Get-CimInstance Win32_OperatingSystem
if ([int]$os.BuildNumber -lt 22000 -or $os.ProductType -ne 1) {
    throw 'Runtime installation smoke requires a Windows 11 client; Server runners only validate packaging.'
}
if (-not ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'Unsigned executable MSIX testing requires an elevated PowerShell.'
}
if (@(Get-AppxPackage -AllUsers -Name $packageName).Count) {
    throw 'A TwoDrive test package is already installed; refusing to replace it.'
}
$Package = (Resolve-Path -LiteralPath $Package).Path
$Report = [IO.Path]::GetFullPath($Report)
if (Test-Path -LiteralPath $Report) { throw 'Report must be new.' }
$work = Split-Path -Parent $Report
New-Item -ItemType Directory -Force -Path $work | Out-Null
$state = Join-Path $work ('msix-state-' + [guid]::NewGuid().ToString('N'))
Add-Type -AssemblyName System.IO.Compression.FileSystem
$zip = [IO.Compression.ZipFile]::OpenRead($Package)
try {
    $reader = [IO.StreamReader]::new($zip.GetEntry('AppxManifest.xml').Open())
    try { [xml]$manifest = $reader.ReadToEnd() } finally { $reader.Dispose() }
    if ($manifest.Package.Identity.Name -ne $packageName -or $manifest.Package.Identity.Publisher -notlike '*OID.2.25.311729368913984317654407730594956997722=1*') {
        throw 'Only the reserved unsigned TwoDrive test identity may be installed by this smoke.'
    }
} finally { $zip.Dispose() }
Add-Type -TypeDefinition @'
using System;
using System.Text;
using System.Runtime.InteropServices;
namespace TwoDriveMsixSmoke {
  [ComImport, Guid("2E941141-7F97-4756-BA1D-9DECDE894A3D"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
  interface IActivation {
    [PreserveSig] int ActivateApplication([MarshalAs(UnmanagedType.LPWStr)] string id, [MarshalAs(UnmanagedType.LPWStr)] string args, uint options, out uint pid);
    [PreserveSig] int ActivateForFile(IntPtr id, IntPtr items, IntPtr verb, out uint pid);
    [PreserveSig] int ActivateForProtocol(IntPtr id, IntPtr items, out uint pid);
  }
  [ComImport, Guid("45BA127D-10A8-46EA-8AB7-56EA9078943C")]
  class ActivationManager {}
  public static class Native {
    [DllImport("kernel32.dll", SetLastError=true)] static extern IntPtr OpenProcess(uint access, bool inherit, uint pid);
    [DllImport("kernel32.dll")] static extern bool CloseHandle(IntPtr handle);
    [DllImport("kernel32.dll", CharSet=CharSet.Unicode)] static extern int GetPackageFullName(IntPtr process, ref uint length, StringBuilder name);
    [DllImport("user32.dll")] static extern bool EnumWindows(Callback callback, IntPtr data);
    delegate bool Callback(IntPtr window, IntPtr data);
    [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr window, out uint pid);
    [DllImport("user32.dll", SetLastError=true)] static extern bool PostThreadMessage(uint thread, uint message, UIntPtr wparam, IntPtr lparam);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] static extern int GetClassName(IntPtr window, StringBuilder name, int count);
    public static uint Activate(string id, string args) {
      var manager = (IActivation)new ActivationManager();
      try { uint pid; Marshal.ThrowExceptionForHR(manager.ActivateApplication(id, args, 6, out pid)); return pid; }
      finally { Marshal.ReleaseComObject(manager); }
    }
    public static string Identity(uint pid) {
      var process = OpenProcess(0x1000, false, pid);
      if (process == IntPtr.Zero) throw new System.ComponentModel.Win32Exception();
      try {
        uint length=0; int result=GetPackageFullName(process, ref length, null);
        if (result != 122) throw new System.ComponentModel.Win32Exception(result);
        var name=new StringBuilder((int)length);
        result=GetPackageFullName(process, ref length, name);
        if (result != 0) throw new System.ComponentModel.Win32Exception(result);
        return name.ToString();
      } finally { CloseHandle(process); }
    }
    [DllImport("user32.dll", SetLastError=true)] static extern bool PostMessage(IntPtr window, uint message, UIntPtr wparam, IntPtr lparam);
    public static void Close(uint target) { CloseWindow(target, false); }
    public static void RequestClose(uint target) { CloseWindow(target, true); }
    static void CloseWindow(uint target, bool requestClose) {
      bool found=false;
      EnumWindows((window, data) => {
        uint pid; uint thread=GetWindowThreadProcessId(window, out pid);
        if (pid!=target) return true;
        var name=new StringBuilder(256);
        if (GetClassName(window, name, name.Capacity)==0) throw new System.ComponentModel.Win32Exception();
        Console.WriteLine("MSIX close candidate: class=" + name + " thread=" + thread);
        // COM/OLE helper windows may precede the Flutter runner and live on
        // other threads. Only the runner owns the GUI message loop under test.
        if (name.ToString()!="FLUTTER_RUNNER_WIN32_WINDOW") return true;
        bool sent = requestClose ? PostMessage(window, 0x10, UIntPtr.Zero, IntPtr.Zero) : PostThreadMessage(thread, 0x12, UIntPtr.Zero, IntPtr.Zero);
        if (!sent) throw new System.ComponentModel.Win32Exception();
        found=true;
        return false;
      }, IntPtr.Zero);
      if (!found) throw new InvalidOperationException("No Flutter runner window belongs to the activated package process.");
    }
  }
}
'@
$beforeDependencies = @(Get-AppxPackage -Name $dependencyName | ForEach-Object PackageFullName)
$installed = $null
$applicationPid = 0
$enginePid = 0
$serverProcess = $null
$addedDependency = $null
$result = [ordered]@{platform='Windows 11 native'; package_version=[string]$manifest.Package.Identity.Version; signed=$false; certificates_modified=$false; checks=@{}; passed=$false}
try {
    if (-not @(Get-AppxPackage -Name $dependencyName | Where-Object { $_.Architecture -eq 'X64' -and $_.Version -ge [version]'14.0.30704.0' }).Count) {
        if (-not $VcRuntime) { throw 'Supply the official Microsoft x64 Desktop VCLibs APPX dependency.' }
        $VcRuntime = (Resolve-Path -LiteralPath $VcRuntime).Path
        $signature = Get-AuthenticodeSignature -LiteralPath $VcRuntime
        if ($signature.Status -ne 'Valid' -or $signature.SignerCertificate.Subject -notlike '*O=Microsoft Corporation*') {
            throw 'The VCLibs dependency must have a valid Microsoft signature.'
        }
        Add-AppxPackage -Path $VcRuntime
        $addedDependency = Get-AppxPackage -Name $dependencyName | Where-Object { $_.PackageFullName -notin $beforeDependencies } | Select-Object -First 1
    }
    Add-AppxPackage -Path $Package -AllowUnsigned
    $installed = Get-AppxPackage -Name $packageName
    if (-not $installed -or [string]$installed.Version -ne [string]$manifest.Package.Identity.Version) { throw 'Installed package version mismatch.' }
    $result.checks.install = $true
    $appId = $installed.PackageFamilyName + '!TwoDrive'
    $applicationPid = [TwoDriveMsixSmoke.Native]::Activate($appId, ('--background --state "' + $state + '"'))
    $deadline = [DateTime]::UtcNow.AddSeconds(45)
    do {
        if (-not (Get-Process -Id $applicationPid -ErrorAction SilentlyContinue)) { throw 'Packaged GUI exited during startup.' }
        $engine = Get-CimInstance Win32_Process -Filter "ParentProcessId = $applicationPid" | Where-Object Name -eq 'twodrive-dav-engine.exe' | Select-Object -First 1
        if ($engine -and (Test-Path -LiteralPath (Join-Path $state 'run.lock'))) { break }
        if ([DateTime]::UtcNow -ge $deadline) { throw 'Packaged GUI did not start its desktop engine.' }
        Start-Sleep -Milliseconds 100
    } while ($true)
    $enginePid = $engine.ProcessId
    if ([TwoDriveMsixSmoke.Native]::Identity($applicationPid) -ne $installed.PackageFullName) { throw 'GUI does not have the installed MSIX identity.' }
    if ([TwoDriveMsixSmoke.Native]::Identity($enginePid) -ne $installed.PackageFullName) { throw 'Engine child lost the package identity.' }
    $serverAppId = $installed.PackageFamilyName + '!TDLinkServer'
    $serverPid = [TwoDriveMsixSmoke.Native]::Activate($serverAppId, ('--state "' + $state + '"'))
    if ($serverPid -eq $applicationPid -or [TwoDriveMsixSmoke.Native]::Identity($serverPid) -ne $installed.PackageFullName) {
        throw 'TDLink did not start an independent packaged process.'
    }
    $serverProcess = Get-Process -Id $serverPid
    $deadline = [DateTime]::UtcNow.AddSeconds(45)
    while (-not (Test-Path -LiteralPath (Join-Path $state 'tdlink-server/run.lock'))) {
        if (-not (Get-Process -Id $serverPid -ErrorAction SilentlyContinue) -or [DateTime]::UtcNow -ge $deadline) {
            throw 'The TDLink launcher did not reach the server role.'
        }
        Start-Sleep -Milliseconds 100
    }
    [TwoDriveMsixSmoke.Native]::RequestClose($serverPid)
    if (-not $serverProcess.WaitForExit(15000)) { throw 'TDLink did not close normally.' }
    if (-not (Get-Process -Id $applicationPid -ErrorAction SilentlyContinue) -or -not (Get-Process -Id $enginePid -ErrorAction SilentlyContinue)) {
        throw 'Closing TDLink also closed the client.'
    }
    $result.checks.separate_start_menu_entries = $true
    if (-not (Test-Path -LiteralPath (Join-Path $installed.InstallLocation 'distribution-msix'))) { throw 'MSIX update-channel marker missing.' }
    $result.checks.packaged_gui_and_engine_identity = $true
    $result.checks.managed_update_marker = $true
    # Launching helpers in WindowsApps directly from an unpackaged test shell is
    # rejected by Windows. Use the official package-context debugger for backend
    # tests, separately from the real GUI activation and identity assertions.
    [TwoDriveMsixSmoke.Native]::Close($applicationPid)
    Wait-Process -Id $applicationPid -Timeout 15
    $deadline = [DateTime]::UtcNow.AddSeconds(10)
    while ((Get-Process -Id $enginePid -ErrorAction SilentlyContinue) -and [DateTime]::UtcNow -lt $deadline) { Start-Sleep -Milliseconds 100 }
    $result.checks.gui_exit_releases_engine = -not [bool](Get-Process -Id $enginePid -ErrorAction SilentlyContinue)
    if (-not $result.checks.gui_exit_releases_engine) { throw 'Normal packaged GUI exit retained its engine.' }
    $enginePid = 0
    $sharingLog = Join-Path $work 'msix-installed-sharing.json'
    $exitFile = Join-Path $work 'msix-context-result.json'
    if (Test-Path -LiteralPath $exitFile) { Remove-Item -LiteralPath $exitFile }
    $planFile = Join-Path $work 'msix-context-plan.json'
    @{identity=$installed.PackageFullName;script=(Join-Path $PSScriptRoot 'test-desktop-sharing.py');engine=(Join-Path $installed.InstallLocation 'twodrive-dav-engine.exe');peer=(Join-Path $installed.InstallLocation 'twodrive-dev.exe');log=$sharingLog;result=$exitFile} | ConvertTo-Json | Set-Content -LiteralPath $planFile -Encoding utf8
    $python = (Get-Command python).Source
    $pythonWindowless = Join-Path (Split-Path -Parent $python) 'pythonw.exe'
    if (Test-Path -LiteralPath $pythonWindowless) { $python = $pythonWindowless }
    Invoke-CommandInDesktopPackage -PackageFamilyName $installed.PackageFamilyName -AppId TwoDrive -Command $python -Args ('"' + (Join-Path $PSScriptRoot 'test-msix-context.py') + '" "' + $planFile + '"') -PreventBreakaway
    $deadline = [DateTime]::UtcNow.AddSeconds(190)
    while (-not (Test-Path -LiteralPath $exitFile)) {
        if ([DateTime]::UtcNow -ge $deadline) { throw 'Packaged sharing regression did not report completion.' }
        Start-Sleep -Milliseconds 100
    }
    $contextResult = Get-Content -LiteralPath $exitFile -Raw | ConvertFrom-Json
    if (-not $contextResult.package_identity_verified) { throw 'Debug process did not retain the MSIX identity.' }
    if ($contextResult.exit_code -ne 0) { throw 'Packaged-context IPC/QUIC sharing regression failed; inspect msix-installed-sharing.json.' }
    $result.checks.installed_engine_sharing = $true
} finally {
    if ($serverProcess -and -not $serverProcess.HasExited) {
        $serverProcess.Kill()
    }
    if ($applicationPid -and (Get-Process -Id $applicationPid -ErrorAction SilentlyContinue)) {
        [TwoDriveMsixSmoke.Native]::Close($applicationPid)
        $deadline = [DateTime]::UtcNow.AddSeconds(10)
        while ((Get-Process -Id $applicationPid -ErrorAction SilentlyContinue) -and [DateTime]::UtcNow -lt $deadline) { Start-Sleep -Milliseconds 100 }
        if (Get-Process -Id $applicationPid -ErrorAction SilentlyContinue) { Stop-Process -Id $applicationPid -Force }
    }
    if ($enginePid) {
        $deadline = [DateTime]::UtcNow.AddSeconds(10)
        while ((Get-Process -Id $enginePid -ErrorAction SilentlyContinue) -and [DateTime]::UtcNow -lt $deadline) { Start-Sleep -Milliseconds 100 }
        $result.checks.gui_exit_releases_engine = -not [bool](Get-Process -Id $enginePid -ErrorAction SilentlyContinue)
        if (-not $result.checks.gui_exit_releases_engine) { Stop-Process -Id $enginePid -Force }
    }
    # Only this otherwise-absent, reserved test identity can be removed.
    $remaining = @(Get-AppxPackage -AllUsers -Name $packageName)
    foreach ($item in $remaining) { Remove-AppxPackage -Package $item.PackageFullName -AllUsers }
    $result.checks.uninstall = @(Get-AppxPackage -AllUsers -Name $packageName).Count -eq 0
    if ($addedDependency) { Remove-AppxPackage -Package $addedDependency.PackageFullName }
    $result.checks.dependency_registration_restored = @(Get-AppxPackage -Name $dependencyName | Where-Object { $_.PackageFullName -notin $beforeDependencies }).Count -eq 0
    $result.passed = $result.checks.separate_start_menu_entries -eq $true -and $result.checks.Count -ge 8 -and -not @($result.checks.Values | Where-Object { $_ -ne $true }).Count
    $result | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath $Report -Encoding utf8
}
if (-not $result.passed) { throw 'An MSIX runtime or cleanup assertion failed.' }
Get-Content -LiteralPath $Report

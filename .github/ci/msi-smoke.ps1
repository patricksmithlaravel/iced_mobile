# The Windows release smoke test (design §17 item 11): the .msi installs
# silently, puts the app's executable under Program Files (or the per-user
# Programs folder), and uninstalls silently without leaving it behind.
# release.sh runs it on the Windows runner; msiexec's verbose logs go to
# -LogDir.
#
#   pwsh -File .github/ci/msi-smoke.ps1 -Msi <file.msi> -Exe <app.exe> -LogDir <dir>
param(
    [Parameter(Mandatory = $true)] [string] $Msi,
    [Parameter(Mandatory = $true)] [string] $Exe,
    [Parameter(Mandatory = $true)] [string] $LogDir
)

$ErrorActionPreference = 'Stop'
$Msi = (Resolve-Path -LiteralPath $Msi).Path

# msiexec as a waited-for process: 0 is success, 3010 success with a reboot
# pending. Start-Process joins the arguments with spaces, so paths are quoted.
function Invoke-Msiexec([string] $Action, [string] $Log) {
    $arguments = @($Action, "`"$Msi`"", '/qn', '/norestart', '/l*v', "`"$Log`"")
    Write-Output "msiexec $($arguments -join ' ')"
    $process = Start-Process -FilePath 'msiexec.exe' -ArgumentList $arguments -Wait -PassThru
    if ($process.ExitCode -ne 0 -and $process.ExitCode -ne 3010) {
        Get-Content -LiteralPath $Log -Tail 40 -ErrorAction SilentlyContinue
        throw "msiexec $Action exited $($process.ExitCode); log: $Log"
    }
}

# Every file named $Exe up to two folders below where installers put apps.
function Find-Exe {
    $roots = @($env:ProgramFiles, ${env:ProgramFiles(x86)}, (Join-Path $env:LOCALAPPDATA 'Programs')) |
        Where-Object { $_ -and (Test-Path -LiteralPath $_) }
    Get-ChildItem -LiteralPath $roots -Filter $Exe -Recurse -Depth 2 -File -ErrorAction SilentlyContinue |
        ForEach-Object { $_.FullName }
}

$before = @(Find-Exe)
Invoke-Msiexec '/i' (Join-Path $LogDir 'msi-install.log')
$installed = @(Find-Exe | Where-Object { $before -notcontains $_ })
if ($installed.Count -eq 0) {
    throw "the .msi installed no $Exe under Program Files or the per-user Programs folder"
}
$path = $installed[0]
$item = Get-Item -LiteralPath $path
Write-Output "installed: $path ($($item.Length) bytes)"
Write-Output "VERSIONINFO: $($item.VersionInfo.ProductName) $($item.VersionInfo.ProductVersion), file $($item.VersionInfo.FileVersion)"

Invoke-Msiexec '/x' (Join-Path $LogDir 'msi-uninstall.log')
if (Test-Path -LiteralPath $path) {
    throw "$path is still there after uninstalling"
}
Write-Output "uninstalled: $path is gone"

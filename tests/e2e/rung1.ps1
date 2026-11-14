# M11 rung 1: install and uninstall (ARFC 0017 clauses 3.15, 3.16, 3.17).
#
# Run it on the Windows client, from an elevated PowerShell (Windows PowerShell
# 5.1 or PowerShell 7, 64-bit), with the OpenSSH client (`ssh.exe`) able to log
# in to every Linux node as root without a prompt:
#
#   powershell -NoProfile -ExecutionPolicy Bypass -File tests\e2e\rung1.ps1 `
#       -Version <version> -LinuxNode root@<node-a>, root@<node-b> -ProxmoxNode root@<node-b>
#
# Each machine must start clean: no Alloy install and no Alloy data directory.
# For each Linux node over SSH, then for this Windows machine, it takes a host
# snapshot (clause 3.17.6), runs the one-line install from the release's pinned
# URL (clause 3.15), checks the install and the snapshot, runs the install again
# and checks that it changed nothing, runs the uninstall (clause 3.16), checks
# that the snapshot equals the first one, and runs the uninstall again.
#
# On a remote machine it runs only the snapshot script, the probe script (the
# checks), the install, and the uninstall. It sends the snapshot and probe
# scripts on the standard input of ssh.exe to `sh -s`, so nothing is copied to
# the node. It prints one PASS or FAIL line per
# check, then a summary, and exits 0 when every check passed, 1 when any
# failed, and 2 on a usage error.

[CmdletBinding()]
param(
    # The release to install, such as 0.0.1-preview.1.
    [Parameter(Mandatory = $true)]
    [string]$Version,

    # The SSH destination of each Linux node, such as root@<node-a>.
    [string[]]$LinuxNode = @(),

    # The SSH destinations, among -LinuxNode, of the nodes that are Proxmox VE
    # hosts.
    [string[]]$ProxmoxNode = @()
)

Set-StrictMode -Version 2.0
$ErrorActionPreference = 'Stop'

$script:Passed = 0
$script:Failed = 0
$ReleaseBase = 'https://github.com/tundrasgg/alloy/releases/download'

# The options of every SSH connection. Each is a word with no space or quote.
$SshOptions = @('-o', 'BatchMode=yes', '-o', 'ConnectTimeout=15', '-o', 'ServerAliveInterval=15')

function Exit-Usage {
    param([string]$Message)
    [Console]::Error.WriteLine("rung1.ps1: $Message")
    exit 2
}

# ---------------------------------------------------------------- arguments

if ($Version -notmatch '^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?(\+[0-9A-Za-z.]+)?$') {
    Exit-Usage "-Version '$Version' is not a release version"
}
foreach ($destination in @($LinuxNode) + @($ProxmoxNode)) {
    if ($destination -notmatch '^[A-Za-z0-9_][A-Za-z0-9._@:/\[\]-]*$') {
        Exit-Usage "'$destination' is not an SSH destination this script accepts"
    }
}
foreach ($destination in $ProxmoxNode) {
    if ($LinuxNode -cnotcontains $destination) {
        Exit-Usage "-ProxmoxNode '$destination' is not one of -LinuxNode"
    }
}

if (@($LinuxNode).Count -gt 0 -and $null -eq (Get-Command 'ssh.exe' -ErrorAction SilentlyContinue)) {
    Exit-Usage 'ssh.exe, the OpenSSH client, is not on the search path'
}

$InstallShUrl = "$ReleaseBase/v$Version/install.sh"
$InstallPs1Url = "$ReleaseBase/v$Version/install.ps1"

try {
    [Console]::OutputEncoding = [Text.Encoding]::UTF8
} catch {
    # The host has no console to set, as in some remote sessions. The snapshots
    # are then decoded with the host's encoding, the same for every snapshot.
    $null = $_
}

. (Join-Path $PSScriptRoot 'e2e-lib.ps1')

# ---------------------------------------------------------------- one machine

function Test-Machine {
    param($Machine)
    $name = $Machine.Name
    $isWindows = ($Machine.Kind -eq 'windows')
    $kind = if ($isWindows) { 'client' } else { 'node' }
    $serviceName = if ($isWindows) { 'service alloyd' } else { 'unit alloyd.service' }

    # Before the install.
    $p0 = Get-Probe $Machine
    $privilege = if ($isWindows) { 'runs elevated' } else { 'probe runs as root' }
    Write-Check $name $privilege ($p0.Ran -and $p0.Root) @('the probe did not run with the privileges the install needs')
    if (-not ($p0.Ran -and $p0.Root)) { return }

    $present = @(Get-PresentPaths $p0)
    $clean = ($present.Count -eq 0) -and ($p0.TempNames.Count -eq 0) -and ($p0.ServiceState -eq '(none)' -or $p0.ServiceState -eq 'not-found')
    $detail = @($present | ForEach-Object { "exists: $_" }) + @($p0.TempNames | ForEach-Object { "exists: $_" }) +
        @("$serviceName state: $($p0.ServiceState)")
    Write-Check $name 'starts clean: no install, no data directory, no alloyd service' $clean $detail
    if (-not $clean) { return }

    $s0 = Get-Snapshot $Machine
    Write-Check $name 'snapshot before install' ($null -ne $s0) @('the snapshot output did not end with its end section')

    # Install.
    $run = Invoke-Install $Machine
    Write-Check $name 'install exits 0' ($run.Exit -eq 0) (Get-RunDetail $run)

    $p1 = Get-Probe $Machine
    Write-Check $name "alloy version prints $Version" (($p1.VersionExit -eq '0') -and $p1.VersionMatch) @(
        "exit code $($p1.VersionExit)", "output: $($p1.VersionOut)")
    $wantHead = @('alloy-manifest 1', "version $Version", "kind $kind", 'state complete')
    Write-Check $name "manifest says complete, version $Version, kind $kind" (Test-SameLines $wantHead $p1.Head) (
        @('want:') + @($wantHead | ForEach-Object { "  $_" }) + @('found:') + @($p1.Head | ForEach-Object { "  $_" }))
    $badFiles = @($p1.Files | Where-Object { $_ -notlike 'ok *' })
    Write-Check $name 'every file the manifest lists has its recorded digest' (($p1.Files.Count -gt 0) -and ($badFiles.Count -eq 0)) (
        @("files listed: $($p1.Files.Count)") + $badFiles)
    if ($isWindows) {
        Write-Check $name 'service alloyd exists and its start type is Disabled' ($p1.ServiceState -eq 'Disabled') @("start type: $($p1.ServiceState)")
        Write-Check $name 'service alloyd is not started' ($p1.ServiceRun -eq 'Stopped') @("status: $($p1.ServiceRun)")
    } else {
        $unitFile = '/etc/systemd/system/alloyd.service'
        $unitPresent = $p1.Paths.Contains($unitFile) -and $p1.Paths[$unitFile] -eq 'yes'
        Write-Check $name 'unit alloyd.service exists and is disabled' ($unitPresent -and $p1.ServiceState -eq 'disabled') @(
            "unit file present: $unitPresent", "is-enabled: $($p1.ServiceState)")
        Write-Check $name 'unit alloyd.service is not started' ($p1.ServiceRun -eq 'inactive') @("is-active: $($p1.ServiceRun)")
    }
    $dataDirs = if ($isWindows) { @(Join-Path $Machine.ProgramData 'Alloy') } else { @('/var/lib/alloyd', '/run/alloy') }
    $made = @($dataDirs | Where-Object { $p1.Paths.Contains($_) -and $p1.Paths[$_] -ne 'no' })
    Write-Check $name 'install created no data directory' ($made.Count -eq 0) @($made | ForEach-Object { "exists: $_" })
    Write-Check $name 'no temporary name remains after install' ($p1.TempNames.Count -eq 0) $p1.TempNames

    $s1 = Get-Snapshot $Machine
    $allowed = Get-AllowedAfterInstall $Machine $s0 $s1
    $what = if ($isWindows) { 'the service alloyd and the PATH entry' } else { 'the unit alloyd.service' }
    Write-SnapshotChecks $Machine "snapshot after install differs only by $what" $allowed[0] $allowed[1]

    # Install again.
    $run = Invoke-Install $Machine
    Write-Check $name 'second install exits 0' ($run.Exit -eq 0) (Get-RunDetail $run)
    $p2 = Get-Probe $Machine
    $same = (Test-SameLines $p1.Head $p2.Head) -and (Test-SameLines $p1.Fingerprints $p2.Fingerprints) -and
        ($p1.ServiceState -eq $p2.ServiceState) -and ($p1.ServiceRun -eq $p2.ServiceRun)
    Write-Check $name 'second install changed no installed file and left the service as it was' $same (
        @(Get-LineDiff (@($p1.Head) + @($p1.Fingerprints) + @("service $($p1.ServiceState) $($p1.ServiceRun)")) (
            @($p2.Head) + @($p2.Fingerprints) + @("service $($p2.ServiceState) $($p2.ServiceRun)"))))
    $s2 = Get-Snapshot $Machine
    Write-SnapshotChecks $Machine 'snapshot after second install equals the one after the first' $s1 $s2

    # Uninstall.
    $installed = @($p1.Paths.Keys)
    $run = Invoke-Uninstall $Machine
    Write-Check $name 'uninstall exits 0' ($run.Exit -eq 0) (Get-RunDetail $run)
    $p3 = Get-Probe $Machine $installed
    $left = @(Get-PresentPaths $p3)
    Write-Check $name 'uninstall removed every installed path' ($left.Count -eq 0) @($left | ForEach-Object { "exists: $_" })
    Write-Check $name 'no temporary name remains after uninstall' ($p3.TempNames.Count -eq 0) $p3.TempNames
    $gone = ($p3.ServiceState -eq '(none)' -or $p3.ServiceState -eq 'not-found')
    Write-Check $name "$serviceName is gone" $gone @("state: $($p3.ServiceState)")
    $s3 = Get-Snapshot $Machine
    Write-SnapshotChecks $Machine 'snapshot after uninstall equals the one before install' $s0 $s3

    # Uninstall again.
    $run = Invoke-UninstallFromScript $Machine
    Write-Check $name 'second uninstall exits 0' ($run.Exit -eq 0) (Get-RunDetail $run)
    $p4 = Get-Probe $Machine $installed
    $left = @(Get-PresentPaths $p4)
    Write-Check $name 'second uninstall left no installed path' ($left.Count -eq 0) @($left | ForEach-Object { "exists: $_" })
    $s4 = Get-Snapshot $Machine
    Write-SnapshotChecks $Machine 'snapshot after second uninstall equals the one before install' $s0 $s4
}

# ---------------------------------------------------------------- main

$machines = @()
foreach ($destination in $LinuxNode) {
    $machines += [pscustomobject]@{
        Name    = $destination
        Kind    = 'linux'
        Proxmox = ($ProxmoxNode -ccontains $destination)
    }
}

$windows = [pscustomobject]@{
    Name         = 'local-windows'
    Kind         = 'windows'
    Proxmox      = $false
    ProgramFiles = $env:ProgramFiles
    ProgramData  = $env:ProgramData
}
$machines += $windows

Write-Output ("Alloy M11 rung 1: install and uninstall of {0} on {1} machine(s)" -f $Version, $machines.Count)
if (@($LinuxNode).Count -gt 0) {
    # A client built on the Cygwin or MSYS runtime, such as the one in Git for
    # Windows, splits its command line by other rules than the Windows OpenSSH
    # client. Name the one in use, for the record.
    Write-Output ("SSH client: {0}" -f @(Get-Command 'ssh.exe' -CommandType Application)[0].Path)
}

foreach ($machine in $machines) {
    if ($machine.Kind -eq 'windows' -and [Environment]::Is64BitOperatingSystem -and -not [Environment]::Is64BitProcess) {
        Write-Check $machine.Name 'runs in a 64-bit PowerShell' $false @('run the 64-bit PowerShell, so that %ProgramFiles% is the folder Windows reports')
        continue
    }
    try {
        Test-Machine $machine
    } catch {
        Write-Check $machine.Name 'the check ran to its end' $false @($_.Exception.Message, $_.InvocationInfo.PositionMessage)
    }
}

Write-Output ''
Write-Output ('SUMMARY  {0} passed, {1} failed' -f $script:Passed, $script:Failed)
if ($script:Failed -gt 0) {
    exit 1
}
exit 0

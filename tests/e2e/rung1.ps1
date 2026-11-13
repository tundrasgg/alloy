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
# checks), the install, and the uninstall. It prints one PASS or FAIL line per
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

# ---------------------------------------------------------------- reporting

function Write-Check {
    param(
        [string]$Machine,
        [string]$Name,
        [bool]$Ok,
        [string[]]$Detail = @()
    )
    if ($Ok) {
        $script:Passed += 1
        Write-Output ('PASS  {0}  {1}' -f $Machine, $Name)
    } else {
        $script:Failed += 1
        Write-Output ('FAIL  {0}  {1}' -f $Machine, $Name)
        foreach ($line in @($Detail | Select-Object -First 40)) {
            Write-Output ('      {0}' -f $line)
        }
    }
}

# Lines in $Before and not in $After (prefixed `- `) and the reverse (`+ `),
# counted as multisets, or a note that only the order differs.
function Get-LineDiff {
    param(
        [string[]]$Before,
        [string[]]$After
    )
    $counts = New-Object 'System.Collections.Generic.Dictionary[string,int]' ([StringComparer]::Ordinal)
    foreach ($line in $Before) {
        if ($counts.ContainsKey($line)) { $counts[$line] += 1 } else { $counts[$line] = 1 }
    }
    $added = @()
    foreach ($line in $After) {
        if ($counts.ContainsKey($line) -and $counts[$line] -gt 0) {
            $counts[$line] -= 1
        } else {
            $added += "+ $line"
        }
    }
    $removed = @()
    foreach ($line in $Before) {
        if ($counts.ContainsKey($line) -and $counts[$line] -gt 0) {
            $counts[$line] -= 1
            $removed += "- $line"
        }
    }
    $diff = @($removed) + @($added)
    if ($diff.Count -eq 0) {
        return @('(same lines, different order)')
    }
    return $diff
}

function Test-SameLines {
    param(
        [string[]]$Before,
        [string[]]$After
    )
    $b = @($Before)
    $a = @($After)
    if ($b.Count -ne $a.Count) { return $false }
    for ($i = 0; $i -lt $b.Count; $i++) {
        if ($b[$i] -cne $a[$i]) { return $false }
    }
    return $true
}

# ---------------------------------------------------------------- native commands

# Runs a native command with its arguments and returns its exit code and its
# standard output lines. With -MergeError standard error is captured too, as
# text; otherwise it goes to the console.
function Invoke-Native {
    param(
        [string]$FilePath,
        [string[]]$Arguments,
        [switch]$MergeError
    )
    $previous = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try {
        if ($MergeError) {
            $lines = @(& $FilePath @Arguments 2>&1 | ForEach-Object { "$_" })
        } else {
            $lines = @(& $FilePath @Arguments | ForEach-Object { "$_" })
        }
        $code = $LASTEXITCODE
    } catch {
        $lines = @("could not run ${FilePath}: $($_.Exception.Message)")
        $code = -1
    } finally {
        $ErrorActionPreference = $previous
    }
    return [pscustomobject]@{ Exit = $code; Lines = $lines }
}

function Invoke-Ssh {
    param(
        [string]$Destination,
        [string]$Command,
        [switch]$MergeError
    )
    $arguments = @('-n', '-o', 'BatchMode=yes', '-o', 'ConnectTimeout=15',
        '-o', 'ServerAliveInterval=15', $Destination, $Command)
    return Invoke-Native -FilePath 'ssh.exe' -Arguments $arguments -MergeError:$MergeError
}

# Runs one of the shell scripts next to this one on a Linux node, passing its
# text in the command line so that nothing is copied to the node. Every
# argument is a word with no shell metacharacters.
function Invoke-RemoteScript {
    param(
        [string]$Destination,
        [string]$ScriptName,
        [string[]]$Arguments = @()
    )
    $text = [IO.File]::ReadAllText((Join-Path $PSScriptRoot $ScriptName)) -replace "`r", ''
    $encoded = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($text))
    $command = "printf %s '$encoded' | base64 -d | sh -s -- " + (@($Arguments) -join ' ')
    return Invoke-Ssh -Destination $Destination -Command $command
}

# ---------------------------------------------------------------- snapshots

# Splits snapshot output into an ordered map from section name to lines, or
# returns $null when the output does not end with the `end` section.
function ConvertTo-Snapshot {
    param([string[]]$Lines)
    $sections = [ordered]@{}
    $current = $null
    $complete = $false
    foreach ($line in $Lines) {
        if ($line -match '^### (.*)$') {
            $current = $Matches[1]
            if ($current -eq 'end') {
                $complete = $true
                break
            }
            $sections[$current] = New-Object 'System.Collections.Generic.List[string]'
        } elseif ($null -ne $current) {
            $sections[$current].Add($line)
        }
    }
    if (-not $complete) {
        return $null
    }
    return $sections
}

function Get-Snapshot {
    param($Machine)
    if ($Machine.Kind -eq 'windows') {
        $lines = @(& (Join-Path $PSScriptRoot 'host-snapshot.ps1') | ForEach-Object { "$_" })
    } else {
        $arguments = @()
        if ($Machine.Proxmox) { $arguments = @('--proxmox') }
        $result = Invoke-RemoteScript -Destination $Machine.Name -ScriptName 'host-snapshot.sh' -Arguments $arguments
        $lines = $result.Lines
    }
    return ConvertTo-Snapshot $lines
}

function Copy-Snapshot {
    param($Snapshot)
    $copy = [ordered]@{}
    foreach ($name in $Snapshot.Keys) {
        $copy[$name] = New-Object 'System.Collections.Generic.List[string]'
        foreach ($line in $Snapshot[$name]) { $copy[$name].Add($line) }
    }
    return $copy
}

# Compares the sections of two snapshots whose names $Filter accepts and
# returns the differences as detail lines, empty when they are equal.
function Compare-Snapshot {
    param(
        $Before,
        $After,
        [scriptblock]$Filter
    )
    if ($null -eq $Before) { return @('the earlier snapshot is incomplete') }
    if ($null -eq $After) { return @('the later snapshot is incomplete') }
    $names = @(@($Before.Keys) + @($After.Keys) | Where-Object { & $Filter $_ } | Select-Object -Unique)
    $detail = @()
    foreach ($name in $names) {
        if (-not $Before.Contains($name)) { $detail += "section '$name' appeared"; continue }
        if (-not $After.Contains($name)) { $detail += "section '$name' disappeared"; continue }
        $b = @($Before[$name])
        $a = @($After[$name])
        if (-not (Test-SameLines $b $a)) {
            $detail += "section '$name':"
            $detail += @(Get-LineDiff $b $a | Select-Object -First 20 | ForEach-Object { "  $_" })
        }
    }
    return $detail
}

$HostSections = { param($name) $name -notlike 'pve *' }
$PveChecks = @(
    @{ Section = 'pve guests'; Label = 'guest list, states, and configurations' },
    @{ Section = 'pve /etc/network/interfaces'; Label = '/etc/network/interfaces' },
    @{ Section = 'pve /etc/pve digests'; Label = '/etc/pve digests' }
)

# Writes the checks of one snapshot comparison: one for the host sections and,
# on a Proxmox VE node, one for each Proxmox VE section.
function Write-SnapshotChecks {
    param(
        $Machine,
        [string]$Name,
        $Before,
        $After
    )
    $detail = @(Compare-Snapshot $Before $After $HostSections)
    Write-Check $Machine.Name $Name ($detail.Count -eq 0) $detail
    if ($Machine.Proxmox) {
        foreach ($check in $PveChecks) {
            $section = $check.Section
            $filter = { param($name) $name -eq $section }.GetNewClosure()
            $detail = @(Compare-Snapshot $Before $After $filter)
            Write-Check $Machine.Name ("{0}: {1} unchanged" -f $Name, $check.Label) ($detail.Count -eq 0) $detail
        }
    }
}

# The snapshot taken before the install, adjusted by the differences clause
# 3.17.6 allows after an install on a machine with no data directory: the unit
# or service `alloyd` and, on Windows, the search path entry. The unit or
# service lines are dropped from both snapshots; the PATH of the earlier one is
# replaced by the PATH the install must leave (clause 3.15 step 6).
function Get-AllowedAfterInstall {
    param(
        $Machine,
        $Before,
        $After
    )
    if ($null -eq $Before -or $null -eq $After) { return @($Before, $After) }
    $b = Copy-Snapshot $Before
    $a = Copy-Snapshot $After
    if ($Machine.Kind -eq 'windows') {
        foreach ($s in @($b, $a)) {
            if ($s.Contains('Get-Service')) {
                $kept = @($s['Get-Service'] | Where-Object { $_ -cnotlike 'Name=alloyd;*' })
                $s['Get-Service'] = $kept
            }
        }
        if ($b.Contains('machine PATH') -and @($b['machine PATH']).Count -eq 1) {
            $path = ([string]@($b['machine PATH'])[0]).Substring('PATH='.Length)
            $entry = Join-Path $Machine.ProgramFiles 'Alloy'
            $present = @($path -split ';' | Where-Object { $_ -ieq $entry }).Count -gt 0
            if (-not $present) {
                if ($path -eq '' -or $path.EndsWith(';')) {
                    $path = $path + $entry
                } else {
                    $path = $path + ';' + $entry
                }
            }
            $b['machine PATH'] = @("PATH=$path")
        }
    } else {
        foreach ($s in @($b, $a)) {
            $name = 'systemctl list-unit-files'
            if ($s.Contains($name)) {
                $s[$name] = @($s[$name] | Where-Object { $_ -cnotmatch '^alloyd\.service\s' })
            }
        }
    }
    return @($b, $a)
}

# ---------------------------------------------------------------- probes

# The install state of a machine: the footprint paths and whether each exists,
# the temporary names found, the manifest, the fingerprints of the installed
# files, the service, and `alloy version`.
function New-Probe {
    return [pscustomobject]@{
        Ran          = $false
        Root         = $false
        Paths        = [ordered]@{}
        TempNames    = @()
        Head         = @()
        Files        = @()
        Fingerprints = @()
        ServiceState = '(none)'
        ServiceRun   = '(none)'
        VersionExit  = '(absent)'
        VersionOut   = ''
        VersionMatch = $false
    }
}

function ConvertTo-Base64Lines {
    param([string[]]$Lines)
    $text = (@($Lines) -join "`n")
    return [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($text))
}

function Get-LinuxProbe {
    param(
        $Machine,
        [string[]]$CheckPaths = @()
    )
    $arguments = @($Version)
    if (@($CheckPaths).Count -gt 0) {
        $arguments += @('--check-b64', (ConvertTo-Base64Lines $CheckPaths))
    }
    $result = Invoke-RemoteScript -Destination $Machine.Name -ScriptName 'rung1-probe.sh' -Arguments $arguments
    $probe = New-Probe
    foreach ($line in $result.Lines) {
        $word, $rest = $line -split ' ', 2
        if ($null -eq $rest) { $rest = '' }
        switch -CaseSensitive ($word) {
            'uid' { $probe.Ran = $true; $probe.Root = ($rest -eq '0') }
            'path' {
                $state, $path = $rest -split ' ', 2
                $probe.Paths[$path] = $state
            }
            'tempname' { $probe.TempNames += $rest }
            'mhead' {
                $number, $text = $rest -split ' ', 2
                if ($null -eq $text) { $text = '' }
                $probe.Head += $text
            }
            'mfile' { $probe.Files += $rest }
            'fp' { $probe.Fingerprints += $rest }
            'unit_enabled' { $probe.ServiceState = $rest }
            'unit_active' { $probe.ServiceRun = $rest }
            'version_exit' { $probe.VersionExit = $rest }
            'version_out' { $probe.VersionOut = $rest }
            'version_match' { $probe.VersionMatch = ($rest -eq 'yes') }
        }
    }
    $probe.TempNames = @($probe.TempNames | Select-Object -Unique)
    return $probe
}

function Test-PathPresent {
    param([string]$Path)
    return [bool](Test-Path -LiteralPath $Path)
}

function Get-WindowsFingerprint {
    param([string]$Path)
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        return "- - - $Path"
    }
    $item = Get-Item -LiteralPath $Path
    $hash = (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
    return ('{0} {1} {2} {3}' -f $item.Length, $item.LastWriteTimeUtc.Ticks, $hash, $Path)
}

function Get-WindowsProbe {
    param(
        $Machine,
        [string[]]$CheckPaths = @()
    )
    $probe = New-Probe
    $probe.Ran = $true
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = New-Object Security.Principal.WindowsPrincipal $identity
    $probe.Root = $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)

    $dir = Join-Path $Machine.ProgramFiles 'Alloy'
    $manifest = Join-Path $dir 'manifest'
    $paths = @(
        $dir,
        (Join-Path $dir 'alloy.exe'),
        (Join-Path $dir 'alloyd.exe'),
        (Join-Path $dir 'alloy-net.exe'),
        (Join-Path $dir 'wintun.dll'),
        (Join-Path $dir 'uninstall.ps1'),
        $manifest,
        (Join-Path $Machine.ProgramData 'Alloy')
    )
    $listed = @()
    if (Test-Path -LiteralPath $manifest -PathType Leaf) {
        $lines = @([IO.File]::ReadAllLines($manifest))
        $probe.Head = @($lines | Select-Object -First 4)
        $probe.Fingerprints += Get-WindowsFingerprint $manifest
        foreach ($line in @($lines | Select-Object -Skip 4)) {
            if ($line -cmatch '^file ([0-9a-f]{64}) (.+)$') {
                $want = $Matches[1]
                $path = $Matches[2]
                $listed += $path
                if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
                    $probe.Files += "missing $path"
                } elseif ((Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant() -ceq $want) {
                    $probe.Files += "ok $path"
                } else {
                    $probe.Files += "bad $path"
                }
                $probe.Fingerprints += Get-WindowsFingerprint $path
            } elseif ($line -cmatch '^dir (.+)$') {
                $listed += $Matches[1]
            }
        }
    }
    foreach ($path in @($paths) + @($listed) + @($CheckPaths) | Select-Object -Unique) {
        if ([string]::IsNullOrEmpty($path)) { continue }
        $probe.Paths[$path] = if (Test-PathPresent $path) { 'yes' } else { 'no' }
        foreach ($temp in @("$path.alloy-new", "$path.alloy-old")) {
            if (Test-PathPresent $temp) { $probe.TempNames += $temp }
        }
    }
    if (Test-Path -LiteralPath $dir -PathType Container) {
        foreach ($item in @(Get-ChildItem -LiteralPath $dir -Recurse -Force -ErrorAction SilentlyContinue)) {
            if ($item.Name -like '*.alloy-new' -or $item.Name -like '*.alloy-old') {
                $probe.TempNames += $item.FullName
            }
        }
    }
    $probe.TempNames = @($probe.TempNames | Select-Object -Unique)

    $service = Get-Service -Name 'alloyd' -ErrorAction SilentlyContinue
    if ($null -ne $service) {
        $probe.ServiceState = [string]$service.StartType
        $probe.ServiceRun = [string]$service.Status
    }

    $exe = Join-Path $dir 'alloy.exe'
    if (Test-Path -LiteralPath $exe -PathType Leaf) {
        $result = Invoke-Native -FilePath $exe -Arguments @('version') -MergeError
        $probe.VersionExit = [string]$result.Exit
        $probe.VersionOut = (@($result.Lines) -join ' ')
        $words = @($probe.VersionOut -split '\s+')
        $probe.VersionMatch = ($words -ccontains $Version) -or ($words -ccontains "v$Version")
    }
    return $probe
}

function Get-Probe {
    param(
        $Machine,
        [string[]]$CheckPaths = @()
    )
    if ($Machine.Kind -eq 'windows') {
        return Get-WindowsProbe -Machine $Machine -CheckPaths $CheckPaths
    }
    return Get-LinuxProbe -Machine $Machine -CheckPaths $CheckPaths
}

function Get-PresentPaths {
    param($Probe)
    return @($Probe.Paths.Keys | Where-Object { $Probe.Paths[$_] -eq 'yes' })
}

# ---------------------------------------------------------------- install and uninstall

function Invoke-Install {
    param($Machine)
    if ($Machine.Kind -eq 'windows') {
        return Invoke-Native -FilePath 'powershell.exe' -MergeError -Arguments @(
            '-NoProfile', '-Command', "irm '$InstallPs1Url' | iex")
    }
    return Invoke-Ssh -Destination $Machine.Name -MergeError -Command "curl -fsSL $InstallShUrl | sh"
}

# The uninstall command of clause 3.16, run from the installed uninstall script.
function Invoke-Uninstall {
    param($Machine)
    if ($Machine.Kind -eq 'windows') {
        $script = Join-Path (Join-Path $Machine.ProgramFiles 'Alloy') 'uninstall.ps1'
        return Invoke-Native -FilePath 'powershell.exe' -MergeError -Arguments @(
            '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', $script)
    }
    return Invoke-Ssh -Destination $Machine.Name -MergeError -Command 'sh /usr/local/share/alloy/uninstall.sh'
}

# The same uninstall through the install script's uninstall form, which needs
# no installed file: the installed uninstall script is gone after the first
# uninstall.
function Invoke-UninstallFromScript {
    param($Machine)
    if ($Machine.Kind -eq 'windows') {
        return Invoke-Native -FilePath 'powershell.exe' -MergeError -Arguments @(
            '-NoProfile', '-Command', "& ([scriptblock]::Create((irm '$InstallPs1Url'))) -Uninstall")
    }
    return Invoke-Ssh -Destination $Machine.Name -MergeError -Command "curl -fsSL $InstallShUrl | sh -s -- --uninstall"
}

function Get-RunDetail {
    param($Result)
    $tail = @($Result.Lines | Select-Object -Last 30)
    return @("exit code $($Result.Exit)") + $tail
}

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

# M11 rung 2: join and node list (ARFC 0002 sections 3.1, 3.3, 3.9, 3.11;
# ARFC 0006 clause 3.15; ARFC 0019 clause 3.1.15; ARFC 0017 clauses 3.16 and
# 3.17.6).
#
# Run it on the Windows client, from an elevated PowerShell (Windows PowerShell
# 5.1 or PowerShell 7, 64-bit), with the OpenSSH client (`ssh.exe`) able to log
# in to every Linux node as root without a prompt:
#
#   powershell -NoProfile -ExecutionPolicy Bypass -File tests\e2e\rung2.ps1 `
#       -Version <version> -LinuxNode root@<node-a>, root@<node-b> -ProxmoxNode root@<node-b>
#
# Every machine must start clean: no Alloy install and no Alloy data directory.
# It takes a host snapshot of each machine (clause 3.17.6) and installs the
# release on each. It runs `alloy init` on the first -LinuxNode, takes the join
# token and login code from its output, logs in there, and issues one login
# code for each other machine. It joins every other Linux node, then this
# Windows machine as a client node, and logs each in. Then `alloy node list`
# on each machine must show every node `active` with its roles, within
# -TimeoutSeconds. While joined, each snapshot may differ from the first only
# in reserved objects (clause 3.17.6). Last it uninstalls with --purge, this
# machine and the joined Linux nodes first (clause 3.16), and checks that each
# snapshot equals the first.
#
# On a remote machine it runs only the snapshot and probe scripts, sent on the
# standard input of ssh.exe to `sh -s`, the install, the uninstall, and the
# `alloy` CLI. It prints one PASS or FAIL line per check, then a summary, and
# exits 0 when every check passed, 1 when any failed, and 2 on a usage error.
# No join token or login code appears in its output.

[CmdletBinding()]
param(
    # The release to install, such as 0.0.1-preview.2.
    [Parameter(Mandatory = $true)]
    [string]$Version,

    # The SSH destination of each Linux node, such as root@<node-a>. `init`
    # runs on the first, and the others join.
    [string[]]$LinuxNode = @(),

    # The SSH destinations, among -LinuxNode, of the nodes that are Proxmox VE
    # hosts.
    [string[]]$ProxmoxNode = @(),

    # How long each machine's node list has to reach the expected state, and
    # how long a node removed by its uninstall has to leave the list.
    [int]$TimeoutSeconds = 30
)

Set-StrictMode -Version 2.0
$ErrorActionPreference = 'Stop'

$script:Passed = 0
$script:Failed = 0
$ReleaseBase = 'https://github.com/tundrasgg/alloy/releases/download'

# The options of every SSH connection. Each is a word with no space or quote.
$SshOptions = @('-o', 'BatchMode=yes', '-o', 'ConnectTimeout=15', '-o', 'ServerAliveInterval=15')

# The installed CLI and uninstall script on a Linux node (ARFC 0017 clauses
# 3.15 and 3.16).
$AlloyLinux = '/usr/local/bin/alloy'
$UninstallLinux = 'sh /usr/local/share/alloy/uninstall.sh'

# Time bounds of the spec: `init` from start to exit (ARFC 0002 section 3.1),
# and `join` from running the command to an `active` node (section 3.3 step 8).
$InitSeconds = 30
$JoinSeconds = 10

function Exit-Usage {
    param([string]$Message)
    [Console]::Error.WriteLine("rung2.ps1: $Message")
    exit 2
}

# ---------------------------------------------------------------- arguments

if ($Version -notmatch '^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?(\+[0-9A-Za-z.]+)?$') {
    Exit-Usage "-Version '$Version' is not a release version"
}
if (@($LinuxNode).Count -eq 0) {
    Exit-Usage '-LinuxNode names no node: init runs on the first one'
}
foreach ($destination in @($LinuxNode) + @($ProxmoxNode)) {
    if ($destination -notmatch '^[A-Za-z0-9_][A-Za-z0-9._@:/\[\]-]*$') {
        Exit-Usage "'$destination' is not an SSH destination this script accepts"
    }
}
if (@($LinuxNode | Select-Object -Unique).Count -ne @($LinuxNode).Count) {
    Exit-Usage '-LinuxNode names a node twice'
}
foreach ($destination in $ProxmoxNode) {
    if ($LinuxNode -cnotcontains $destination) {
        Exit-Usage "-ProxmoxNode '$destination' is not one of -LinuxNode"
    }
}
if ($TimeoutSeconds -lt 1 -or $TimeoutSeconds -gt 3600) {
    Exit-Usage "-TimeoutSeconds $TimeoutSeconds is not between 1 and 3600"
}
if ($null -eq (Get-Command 'ssh.exe' -ErrorAction SilentlyContinue)) {
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

# ---------------------------------------------------------------- the CLI

# Runs the installed `alloy` with these arguments on a machine. On a Linux node
# each argument must be a word that needs no quoting in a shell.
function Invoke-Alloy {
    param(
        $Machine,
        [string[]]$Arguments
    )
    if ($Machine.Kind -eq 'windows') {
        $exe = Join-Path (Join-Path $Machine.ProgramFiles 'Alloy') 'alloy.exe'
        return Invoke-Process -FilePath $exe -Arguments $Arguments
    }
    foreach ($word in $Arguments) {
        if ($word -cnotmatch '^[A-Za-z0-9._,:/=@+-]+$') {
            throw "the argument '$(Hide-Secret $word)' for alloy is not a plain word"
        }
    }
    return Invoke-SshProcess -Destination $Machine.Name -Command ((@($AlloyLinux) + @($Arguments)) -join ' ')
}

# `alloy node list --output json` on a machine, parsed.
function Get-NodeList {
    param($Machine)
    $run = Invoke-Alloy $Machine @('node', 'list', '--output', 'json')
    $parsed = ConvertFrom-NodeListJson $run.Out
    $problem = $parsed.Error
    if ($run.Exit -ne 0) { $problem = "exit code $($run.Exit)" }
    return [pscustomobject]@{
        Ok    = ($run.Exit -eq 0) -and $parsed.Ok
        Error = $problem
        Nodes = $parsed.Nodes
        Run   = $run
    }
}

# Reads a machine's node list once a second until $Until, given the nodes and
# $Argument, returns true, or $Seconds pass. Returns whether it did, the last
# list, and the seconds it waited.
function Wait-NodeList {
    param(
        $Machine,
        [int]$Seconds,
        [scriptblock]$Until,
        $Argument
    )
    $clock = [System.Diagnostics.Stopwatch]::StartNew()
    while ($true) {
        $list = Get-NodeList $Machine
        if ($list.Ok -and (& $Until $list.Nodes $Argument)) {
            return [pscustomobject]@{ Met = $true; List = $list; Seconds = $clock.Elapsed.TotalSeconds }
        }
        if ($clock.Elapsed.TotalSeconds -ge $Seconds) {
            return [pscustomobject]@{ Met = $false; List = $list; Seconds = $clock.Elapsed.TotalSeconds }
        }
        Start-Sleep -Milliseconds 1000
    }
}

# The conditions Wait-NodeList waits for: the named node is listed `active`;
# the named node is not listed; the list is exactly the expected nodes, each
# with its roles and `active`.
$UntilActive = {
    param($Nodes, $Name)
    return @($Nodes | Where-Object { $_.Name -ceq $Name -and $_.State -ceq 'active' }).Count -eq 1
}
$UntilAbsent = {
    param($Nodes, $Name)
    return @($Nodes | Where-Object { $_.Name -ceq $Name }).Count -eq 0
}
$UntilExpected = {
    param($Nodes, $Expected)
    $p = Get-NodeListProblems $Nodes $Expected
    return (@($p.Members) + @($p.Roles) + @($p.States)).Count -eq 0
}

function Get-ListDetail {
    param($List)
    $detail = @()
    if (-not $List.Ok) {
        $detail += "node list failed: $($List.Error)"
        $detail += @(Get-ProcessDetail $List.Run)
    }
    $detail += @($List.Nodes | ForEach-Object { "listed: $(Format-Node $_)" })
    return $detail
}

# The secrets `init` and `alloy login --issue-code` print once each on standard
# output (ARFC 0002 sections 3.1 and 3.7). Returns every match in the lines.
function Find-Secret {
    param(
        [string[]]$Lines,
        [string]$Prefix
    )
    $found = @()
    foreach ($line in @($Lines)) {
        foreach ($match in [regex]::Matches($line, "$Prefix[a-z2-7]+")) {
            $found += $match.Value
        }
    }
    return $found
}

# A join token that differs from $Token in one character of its secret field,
# so that its checksum no longer matches (ARFC 0002 section 3.2).
function Get-CorruptToken {
    param([string]$Token)
    $index = [Math]::Min(75, $Token.Length - 2)
    $replacement = if ($Token[$index] -ceq [char]'a') { 'b' } else { 'a' }
    return $Token.Substring(0, $index) + $replacement + $Token.Substring($index + 1)
}

# ---------------------------------------------------------------- uninstall

# The uninstall of clause 3.16 with --purge and --yes, and --force when asked,
# from the installed uninstall script, or with -FromScript from the release's
# install script, which needs no installed file.
function Invoke-PurgeUninstall {
    param(
        $Machine,
        [switch]$Force,
        [switch]$FromScript
    )
    if ($Machine.Kind -eq 'windows') {
        $powershell = @(Get-Command 'powershell.exe' -CommandType Application)[0].Path
        $flags = @('-Purge', '-Yes')
        if ($Force) { $flags += '-Force' }
        if ($FromScript) {
            return Invoke-Process -FilePath $powershell -Arguments @(
                '-NoProfile', '-Command', "& ([scriptblock]::Create((irm '$InstallPs1Url'))) -Uninstall $($flags -join ' ')")
        }
        $script = Join-Path (Join-Path $Machine.ProgramFiles 'Alloy') 'uninstall.ps1'
        return Invoke-Process -FilePath $powershell -Arguments (@(
                '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', $script) + $flags)
    }
    $flags = @('--purge', '--yes')
    if ($Force) { $flags += '--force' }
    if ($FromScript) {
        return Invoke-SshProcess -Destination $Machine.Name -Command "curl -fsSL $InstallShUrl | sh -s -- --uninstall $($flags -join ' ')"
    }
    return Invoke-SshProcess -Destination $Machine.Name -Command "$UninstallLinux $($flags -join ' ')"
}

# Checks that the uninstall left nothing: no installed path, data directory, or
# temporary name, no service, and a snapshot equal to the one taken before the
# install (clause 3.17.6).
function Test-Removed {
    param($Machine)
    $name = $Machine.Name
    $installed = @()
    if ($null -ne $Machine.Installed) { $installed = @($Machine.Installed.Paths.Keys) }
    $probe = Get-Probe $Machine $installed
    $left = @(Get-PresentPaths $probe)
    Write-Check $name 'uninstall removed every installed path and the data directory' ($left.Count -eq 0) @(
        $left | ForEach-Object { "exists: $_" })
    Write-Check $name 'no temporary name remains after uninstall' ($probe.TempNames.Count -eq 0) $probe.TempNames
    $gone = ($probe.ServiceState -eq '(none)' -or $probe.ServiceState -eq 'not-found')
    Write-Check $name 'the alloyd service is gone' $gone @("state: $($probe.ServiceState)")
    $after = Get-Snapshot $Machine
    Write-SnapshotChecks $Machine 'snapshot after uninstall --purge equals the one before install' $Machine.Before $after
}

# ---------------------------------------------------------------- machines

$machines = @()
$first = $true
foreach ($destination in $LinuxNode) {
    $machines += [pscustomobject]@{
        Name      = $destination
        Kind      = 'linux'
        Role      = if ($first) { 'init' } else { 'join' }
        Proxmox   = ($ProxmoxNode -ccontains $destination)
        HostName  = ''
        Kvm       = ''
        NodeName  = ''
        Roles     = @()
        Before    = $null
        Installed = $null
        Tried     = $false
        Joined    = $false
        LoginCode = ''
        LoggedIn  = $false
    }
    $first = $false
}
$windows = [pscustomobject]@{
    Name         = 'local-windows'
    Kind         = 'windows'
    Role         = 'client'
    Proxmox      = $false
    ProgramFiles = $env:ProgramFiles
    ProgramData  = $env:ProgramData
    HostName     = ''
    Kvm          = ''
    NodeName     = ''
    Roles        = @()
    Before       = $null
    Installed    = $null
    Tried        = $false
    Joined       = $false
    LoginCode    = ''
    LoggedIn     = $false
}
$machines += $windows
$initNode = $machines[0]
$joinNodes = @($machines | Where-Object { $_.Role -eq 'join' })

Write-Output ("Alloy M11 rung 2: join and node list of {0} on {1} machine(s)" -f $Version, $machines.Count)
Write-Output ("SSH client: {0}" -f @(Get-Command 'ssh.exe' -CommandType Application)[0].Path)

# ---------------------------------------------------------------- phases
#
# Each phase writes its checks to the output, so it returns nothing and leaves
# its result in a variable of the script scope.

$script:Ready = $false
$script:InstallOk = $false
$script:Token = ''

# Each machine starts clean, and its snapshot is taken. Sets $script:Ready
# when every machine is ready.
function Invoke-Preflight {
    $ready = $true
    foreach ($machine in $machines) {
        $name = $machine.Name
        if ($machine.Kind -eq 'windows') {
            if ([Environment]::Is64BitOperatingSystem -and -not [Environment]::Is64BitProcess) {
                Write-Check $name 'runs in a 64-bit PowerShell' $false @('run the 64-bit PowerShell, so that %ProgramFiles% is the folder Windows reports')
                $ready = $false
                continue
            }
            $machine.HostName = [System.Net.Dns]::GetHostName()
        } else {
            $result = Invoke-RemoteScript -Destination $name -ScriptName 'rung2-probe.sh'
            foreach ($line in $result.Lines) {
                $word, $rest = $line -split ' ', 2
                if ($null -eq $rest) { $rest = '' }
                switch -CaseSensitive ($word) {
                    'hostname' { $machine.HostName = $rest }
                    'kvm' { $machine.Kvm = $rest }
                }
            }
            $ok = ($result.Exit -eq 0) -and ($machine.HostName -ne '') -and ($machine.Kvm -ne '')
            Write-Check $name 'rung 2 probe reads the hostname and the virtualization check' $ok (
                @("exit code $($result.Exit)") + @($result.Lines | Select-Object -Last 10))
            if (-not $ok) { $ready = $false; continue }
        }
        $p0 = Get-Probe $machine
        $privilege = if ($machine.Kind -eq 'windows') { 'runs elevated' } else { 'probe runs as root' }
        Write-Check $name $privilege ($p0.Ran -and $p0.Root) @('the probe did not run with the privileges the install needs')
        if (-not ($p0.Ran -and $p0.Root)) { $ready = $false; continue }
        $present = @(Get-PresentPaths $p0)
        $clean = ($present.Count -eq 0) -and ($p0.TempNames.Count -eq 0) -and ($p0.ServiceState -eq '(none)' -or $p0.ServiceState -eq 'not-found')
        Write-Check $name 'starts clean: no install, no data directory, no alloyd service' $clean (
            @($present | ForEach-Object { "exists: $_" }) + @($p0.TempNames | ForEach-Object { "exists: $_" }) +
            @("alloyd service state: $($p0.ServiceState)"))
        if (-not $clean) { $ready = $false; continue }
        $machine.Before = Get-Snapshot $machine
        Write-Check $name 'snapshot before install' ($null -ne $machine.Before) @('the snapshot output did not end with its end section')
        if ($null -eq $machine.Before) { $ready = $false }
    }
    $script:Ready = $ready
}

# The name and roles each node should get: the default name of its hostname in
# join order (ARFC 0002 section 3.3); for the init node every role, and for a
# joined Linux node `compute,storage`, each without `compute` when the
# virtualization check is `warn` (sections 3.1, 3.3); for the Windows client
# none (section 3.11).
function Set-Expected {
    $names = Get-JoinedNodeNames @($machines | ForEach-Object { ConvertTo-DefaultNodeName $_.HostName })
    for ($i = 0; $i -lt $machines.Count; $i++) {
        $machine = $machines[$i]
        $machine.NodeName = @($names)[$i]
        switch ($machine.Role) {
            'init' { $machine.Roles = @('anchor', 'control', 'compute', 'storage') }
            'join' { $machine.Roles = @('compute', 'storage') }
            'client' { $machine.Roles = @() }
        }
        if ($machine.Kvm -eq 'warn') {
            $machine.Roles = @($machine.Roles | Where-Object { $_ -ne 'compute' })
        }
        Write-Output ("      {0} is expected as node {1}, roles {2}" -f $machine.Name, $machine.NodeName, (Format-RolesColumn $machine.Roles))
    }
}

function Get-Expected {
    param($Machines)
    return @($Machines | ForEach-Object { [pscustomobject]@{ Name = $_.NodeName; Roles = $_.Roles } })
}

# Installs the release on every machine. Sets $script:InstallOk when every
# install exited 0.
function Invoke-InstallAll {
    $ok = $true
    foreach ($machine in $machines) {
        $machine.Tried = $true
        $run = Invoke-Install $machine
        Write-Check $machine.Name 'install exits 0' ($run.Exit -eq 0) (Get-RunDetail $run)
        $machine.Installed = Get-Probe $machine
        if ($run.Exit -ne 0) { $ok = $false }
    }
    $script:InstallOk = $ok
}

# `init` on the first Linux node, then its login and the login codes for the
# other machines. Sets $script:Token to the join token when the run can go on.
function Invoke-Init {
    $machine = $initNode
    $name = $machine.Name
    $run = Invoke-Alloy $machine @('init')
    Write-Check $name 'init exits 0' ($run.Exit -eq 0) (Get-ProcessDetail $run)
    $machine.Joined = ($run.Exit -eq 0)
    Write-Check $name "init takes under $InitSeconds seconds, SSH included" ($run.Seconds -lt $InitSeconds) @(
        "took $([Math]::Round($run.Seconds, 1)) s")
    $tokens = @(Find-Secret $run.Out 'alyt_')
    $codes = @(Find-Secret $run.Out 'alyc_')
    $leaked = @(Find-Secret $run.Err 'alyt_') + @(Find-Secret $run.Err 'alyc_')
    Write-Check $name 'init prints the join token once, on standard output, at most 512 characters' (
        ($tokens.Count -eq 1) -and ($tokens[0].Length -le 512)) @(
        "join tokens on standard output: $($tokens.Count)", "on standard error: $(@(Find-Secret $run.Err 'alyt_').Count)")
    Write-Check $name 'init prints the login code once, on standard output, at most 226 characters' (
        ($codes.Count -eq 1) -and ($codes[0].Length -le 226)) @(
        "login codes on standard output: $($codes.Count)", "on standard error: $(@(Find-Secret $run.Err 'alyc_').Count)")
    Write-Check $name 'init prints no join token or login code on standard error' ($leaked.Count -eq 0) @(
        "found $($leaked.Count)")
    if ($run.Exit -ne 0 -or $tokens.Count -lt 1 -or $codes.Count -lt 1) { return }

    $login = Invoke-Alloy $machine @('login', $codes[0])
    Write-Check $name 'alloy login with the login code from init exits 0' ($login.Exit -eq 0) (Get-ProcessDetail $login)
    if ($login.Exit -ne 0) { return }
    $machine.LoggedIn = $true

    # init waits until its node is active (section 3.1 step 4).
    $list = Get-NodeList $machine
    $problems = Get-NodeListProblems $list.Nodes (Get-Expected @($machine))
    $ok = $list.Ok -and (@($problems.Members) + @($problems.Roles) + @($problems.States)).Count -eq 0
    Write-Check $name "right after init, node list shows only $($machine.NodeName), active, roles $(Format-RolesColumn $machine.Roles)" $ok (
        @($problems.Members) + @($problems.Roles) + @($problems.States) + @(Get-ListDetail $list))

    $again = Invoke-Alloy $machine @('init')
    Write-Check $name 'a second init is refused with Conflict: exit 5' ($again.Exit -eq 5) (Get-ProcessDetail $again)

    foreach ($other in @($machines | Where-Object { $_.Role -ne 'init' })) {
        $issue = Invoke-Alloy $machine @('login', '--issue-code')
        $issued = @(Find-Secret $issue.Out 'alyc_')
        Write-Check $name "alloy login --issue-code prints one login code for $($other.Name)" (
            ($issue.Exit -eq 0) -and ($issued.Count -eq 1)) (@("login codes on standard output: $($issued.Count)") + @(Get-ProcessDetail $issue))
        if ($issued.Count -eq 1) { $other.LoginCode = $issued[0] }
    }
    $script:Token = $tokens[0]
}

# Joins one machine with the token, checks the time to `active` on the init
# node's list, a second join, and the machine's login.
function Invoke-Join {
    param(
        $Machine,
        [string]$Token
    )
    $name = $Machine.Name
    $clock = [System.Diagnostics.Stopwatch]::StartNew()
    $run = Invoke-Alloy $Machine @('join', $Token)
    Write-Check $name 'join exits 0' ($run.Exit -eq 0) (Get-ProcessDetail $run)
    if ($run.Exit -ne 0) { return }
    $Machine.Joined = $true
    $node = $Machine.NodeName
    $wait = Wait-NodeList $initNode $TimeoutSeconds $UntilActive $node
    $seconds = $clock.Elapsed.TotalSeconds
    Write-Check $name "node list on $($initNode.Name) shows $node active" $wait.Met (Get-ListDetail $wait.List)
    Write-Check $name "join reaches an active node in under $JoinSeconds seconds, SSH and polling included" (
        $wait.Met -and $seconds -lt $JoinSeconds) @("active after $([Math]::Round($seconds, 1)) s, or not within $TimeoutSeconds s")

    $again = Invoke-Alloy $Machine @('join', $Token)
    Write-Check $name 'a second join is refused with Conflict: exit 5' ($again.Exit -eq 5) (Get-ProcessDetail $again)

    if ($Machine.LoginCode -eq '') {
        Write-Check $name 'alloy login with an issued login code exits 0' $false @('no login code was issued for this machine')
        return
    }
    $login = Invoke-Alloy $Machine @('login', $Machine.LoginCode)
    Write-Check $name 'alloy login with an issued login code exits 0' ($login.Exit -eq 0) (Get-ProcessDetail $login)
    $Machine.LoggedIn = ($login.Exit -eq 0)
}

# Every machine's node list shows every node, active, with its roles.
function Test-NodeLists {
    $expected = Get-Expected $machines
    $names = (@($expected | ForEach-Object { $_.Name }) -join ', ')
    foreach ($machine in $machines) {
        $name = $machine.Name
        if (-not $machine.LoggedIn) {
            Write-Check $name 'node list shows every node healthy' $false @('this machine is not logged in')
            continue
        }
        $wait = Wait-NodeList $machine $TimeoutSeconds $UntilExpected $expected
        $list = $wait.List
        Write-Check $name 'node list --output json exits 0 and prints one JSON array' $list.Ok (Get-ListDetail $list)
        $problems = Get-NodeListProblems $list.Nodes $expected
        $detail = @(Get-ListDetail $list)
        Write-Check $name "node list shows exactly the nodes $names" ($list.Ok -and @($problems.Members).Count -eq 0) (
            @($problems.Members) + $detail)
        Write-Check $name 'node list shows each node with its roles' ($list.Ok -and @($problems.Roles).Count -eq 0) (
            @($problems.Roles) + $detail)
        Write-Check $name "node list shows every node active (healthy) within $TimeoutSeconds s" (
            $list.Ok -and @($problems.States).Count -eq 0) (@($problems.States) + $detail)

        $table = Invoke-Alloy $machine @('node', 'list')
        $tableProblems = @(Get-NodeTableProblems $table.Out $expected)
        Write-Check $name 'node list prints the table of ARFC 0019 clause 3.1.15' (
            ($table.Exit -eq 0) -and $tableProblems.Count -eq 0) (
            $tableProblems + @($table.Out | ForEach-Object { "| $_" }) + @("exit code $($table.Exit)"))
    }
}

# While joined, each snapshot differs from the first only in reserved objects.
function Test-JoinedSnapshots {
    foreach ($machine in $machines) {
        if (-not $machine.Joined) { continue }
        $after = Get-Snapshot $machine
        $adjusted = Get-SnapshotWithoutReserved $machine $machine.Before $after
        Write-SnapshotChecks $machine 'snapshot while joined differs from the one before install only in reserved objects' $machine.Before $adjusted
    }
}

# Uninstalls with --purge, the client node and the joined Linux nodes first,
# when every node joined: the client leaves and drops out of the list; a node
# whose `leave` the control plane refuses (ARFC 0002 section 3.9: the last
# control node, or a node with a disk in a pool, as each joined Linux node's
# `file` disk is in pool `default` by section 3.3 step 7) makes the uninstall
# exit 5 and change nothing, and `--force` then completes it (clause 3.16 step
# 4). Otherwise each machine that the install was tried on is uninstalled with
# --force from the install script.
function Invoke-UninstallAll {
    param([bool]$AllJoined)
    $order = @($windows) + @($joinNodes) + @($initNode)
    foreach ($machine in $order) {
        if (-not $machine.Tried) { continue }
        $name = $machine.Name
        if (-not $AllJoined) {
            $run = Invoke-PurgeUninstall $machine -Force -FromScript
            Write-Check $name 'uninstall --purge --force exits 0' ($run.Exit -eq 0) (Get-ProcessDetail $run)
            Test-Removed $machine
            continue
        }
        if ($machine.Role -eq 'client') {
            $run = Invoke-PurgeUninstall $machine
            Write-Check $name 'uninstall --purge of the client node exits 0' ($run.Exit -eq 0) (Get-ProcessDetail $run)
            if ($run.Exit -ne 0) {
                $run = Invoke-PurgeUninstall $machine -Force
                Write-Check $name 'uninstall --purge --force exits 0' ($run.Exit -eq 0) (Get-ProcessDetail $run)
            } else {
                $node = $machine.NodeName
                $wait = Wait-NodeList $initNode $TimeoutSeconds $UntilAbsent $node
                Write-Check $name "after it leaves, node list on $($initNode.Name) no longer shows $node" $wait.Met (Get-ListDetail $wait.List)
            }
            Test-Removed $machine
            continue
        }
        $why = if ($machine.Role -eq 'init') { 'it is the last control node' } else { 'its disk is in pool default' }
        $run = Invoke-PurgeUninstall $machine
        Write-Check $name "uninstall --purge without --force exits 5: leave is refused, $why" ($run.Exit -eq 5) (Get-ProcessDetail $run)
        if ($run.Exit -ne 0) {
            $node = $machine.NodeName
            $list = Get-NodeList $initNode
            $still = @($list.Nodes | Where-Object { $_.Name -ceq $node }).Count -eq 1
            Write-Check $name "after the refused uninstall, node list on $($initNode.Name) still shows $node" ($list.Ok -and $still) (
                Get-ListDetail $list)
            $run = Invoke-PurgeUninstall $machine -Force
            Write-Check $name 'uninstall --purge --force exits 0' ($run.Exit -eq 0) (Get-ProcessDetail $run)
        }
        Test-Removed $machine
    }
}

# ---------------------------------------------------------------- main

try {
    Invoke-Preflight
    if ($script:Ready) {
        Set-Expected
        Invoke-InstallAll
        if ($script:InstallOk) {
            # `init` refuses on Windows at its host check (ARFC 0002 sections 3.8
            # and 3.11), and writes nothing.
            $run = Invoke-Alloy $windows @('init')
            $dataDir = Join-Path $windows.ProgramData 'Alloy'
            $made = Test-Path -LiteralPath $dataDir
            Write-Check $windows.Name 'init on Windows exits 1 at its host check and creates no data directory' (
                ($run.Exit -eq 1) -and -not $made) (@("data directory exists: $made") + @(Get-ProcessDetail $run))

            Invoke-Init
            $token = $script:Token
            if ($token -ne '') {
                foreach ($machine in @($joinNodes) + @($windows)) {
                    if ($machine.Kind -eq 'linux') {
                        # A token whose checksum does not match is a usage error,
                        # found before any machine is contacted, and nothing is
                        # written (ARFC 0002 sections 3.2 and 3.8).
                        $bad = Invoke-Alloy $machine @('join', (Get-CorruptToken $token))
                        $probe = Get-Probe $machine
                        $made = $probe.Paths.Contains('/var/lib/alloyd') -and $probe.Paths['/var/lib/alloyd'] -ne 'no'
                        Write-Check $machine.Name 'join with a corrupted token exits 2 and creates no data directory' (
                            ($bad.Exit -eq 2) -and -not $made) (@("data directory exists: $made") + @(Get-ProcessDetail $bad))
                    }
                    Invoke-Join $machine $token
                }
                Test-NodeLists
                Test-JoinedSnapshots
            }
        }
    }
} catch {
    Write-Check 'driver' 'the check ran to its end' $false @($_.Exception.Message, $_.InvocationInfo.PositionMessage)
}

try {
    $allJoined = @($machines | Where-Object { -not $_.Joined }).Count -eq 0
    Invoke-UninstallAll $allJoined
} catch {
    Write-Check 'driver' 'the uninstall ran to its end' $false @($_.Exception.Message, $_.InvocationInfo.PositionMessage)
}

Write-Output ''
Write-Output ('SUMMARY  {0} passed, {1} failed' -f $script:Passed, $script:Failed)
if ($script:Failed -gt 0) {
    exit 1
}
exit 0

# Shared functions of the M11 drivers under tests/e2e/ (rung1.ps1, rung2.ps1).
#
# A driver dot-sources this file after it has checked its arguments. The
# functions read these variables of the driver's script scope when they run:
#
#   $Version        the release under test (the probes)
#   $SshOptions     the options of every SSH connection
#   $InstallShUrl   the pinned URL of install.sh of that release
#   $InstallPs1Url  the pinned URL of install.ps1 of that release
#
# and count checks in $script:Passed and $script:Failed. A machine is an
# object with Name, Kind ('linux' or 'windows'), and Proxmox, and on Windows
# ProgramFiles and ProgramData.

# ---------------------------------------------------------------- reporting

# Replaces every join token (ARFC 0002 section 3.2) and login code (section
# 3.7) in a line, so that no secret reaches the output of a check, which a
# report may quote.
function Hide-Secret {
    param([string]$Line)
    $Line = [regex]::Replace($Line, 'alyt_[A-Za-z0-9]+', 'alyt_(hidden)')
    return [regex]::Replace($Line, 'alyc_[A-Za-z0-9]+', 'alyc_(hidden)')
}

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
            Write-Output ('      {0}' -f (Hide-Secret $line))
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
    $arguments = @('-n') + @($SshOptions) + @($Destination, $Command)
    return Invoke-Native -FilePath 'ssh.exe' -Arguments $arguments -MergeError:$MergeError
}

# Quotes one argument for a Windows command line by the rules the C runtime
# uses to split one, which ssh.exe uses, so that the program receives it
# unchanged.
function ConvertTo-CommandLineArgument {
    param([string]$Argument)
    if ($Argument.Length -gt 0 -and $Argument -notmatch '[\s"]') {
        return $Argument
    }
    $out = New-Object System.Text.StringBuilder
    [void]$out.Append('"')
    $slashes = 0
    foreach ($c in $Argument.ToCharArray()) {
        if ($c -eq [char]'\') {
            $slashes += 1
            continue
        }
        if ($c -eq [char]'"') {
            [void]$out.Append([char]'\', 2 * $slashes + 1)
        } elseif ($slashes -gt 0) {
            [void]$out.Append([char]'\', $slashes)
        }
        $slashes = 0
        [void]$out.Append($c)
    }
    if ($slashes -gt 0) {
        [void]$out.Append([char]'\', 2 * $slashes)
    }
    [void]$out.Append('"')
    return $out.ToString()
}

# Runs one of the shell scripts next to this one on a Linux node as
# `sh -s -- <arguments>`, writing the script's bytes to the standard input of
# ssh.exe, so that nothing is copied to the node and the script's text never
# passes through a command line. The command line holds only the SSH options,
# the destination, and words that need no quoting on Windows or in a shell.
# Standard error goes to the console. Returns the exit code and the standard
# output lines.
function Invoke-RemoteScript {
    param(
        [string]$Destination,
        [string]$ScriptName,
        [string[]]$Arguments = @()
    )
    foreach ($word in $Arguments) {
        if ($word -cnotmatch '^[A-Za-z0-9._+/=:@-]+$') {
            throw "the argument '$word' for $ScriptName is not a plain word"
        }
    }
    $text = [IO.File]::ReadAllText((Join-Path $PSScriptRoot $ScriptName)) -replace "`r", ''
    $utf8 = New-Object System.Text.UTF8Encoding $false
    $bytes = $utf8.GetBytes($text)

    $ssh = @(Get-Command 'ssh.exe' -CommandType Application)[0].Path
    $words = @($SshOptions) + @('-T', $Destination, 'sh', '-s', '--') + @($Arguments)
    $info = New-Object System.Diagnostics.ProcessStartInfo
    $info.FileName = $ssh
    $info.Arguments = (@($words | ForEach-Object { ConvertTo-CommandLineArgument $_ }) -join ' ')
    $info.UseShellExecute = $false
    $info.RedirectStandardInput = $true
    $info.RedirectStandardOutput = $true
    $info.StandardOutputEncoding = $utf8

    # Windows PowerShell 5.1 runs on the .NET Framework, whose redirected
    # standard input begins with the preamble of the console input encoding:
    # a byte order mark when that is UTF-8, which the shell on the node would
    # read as part of the script's first line. The console input encoding is
    # set to UTF-8 without a preamble while the process starts. PowerShell 7
    # writes no preamble.
    $inputEncoding = $null
    try {
        $inputEncoding = [Console]::InputEncoding
    } catch {
        $null = $_
    }
    $restore = $false
    if ($null -ne $inputEncoding -and $inputEncoding.GetPreamble().Length -gt 0) {
        try {
            [Console]::InputEncoding = $utf8
            $restore = $true
        } catch {
            throw "cannot send $ScriptName without a byte order mark: the console input encoding cannot be set ($($_.Exception.Message))"
        }
    }
    try {
        $process = [System.Diagnostics.Process]::Start($info)
    } finally {
        if ($restore) {
            [Console]::InputEncoding = $inputEncoding
        }
    }

    try {
        # Read standard output while the script is written, so that neither
        # side waits on a full pipe.
        $reading = $process.StandardOutput.ReadToEndAsync()
        try {
            $stream = $process.StandardInput.BaseStream
            $stream.Write($bytes, 0, $bytes.Length)
            $stream.Flush()
        } catch [System.IO.IOException] {
            # ssh.exe exited before it read the whole script. Its exit code and
            # standard error say why.
            $null = $_
        } finally {
            # The end of standard input ends the script on the node.
            try {
                $process.StandardInput.Close()
            } catch [System.IO.IOException] {
                $null = $_
            }
        }
        $output = $reading.GetAwaiter().GetResult()
        $process.WaitForExit()
        $code = $process.ExitCode
    } finally {
        $process.Dispose()
    }
    $lines = @($output -split "`n" | ForEach-Object { $_.TrimEnd("`r") })
    if ($lines.Count -gt 0 -and $lines[$lines.Count - 1] -eq '') {
        $lines = @($lines | Select-Object -First ($lines.Count - 1))
    }
    return [pscustomobject]@{ Exit = $code; Lines = $lines }
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

# ---------------------------------------------------------------- processes

# Runs a program with its arguments, each quoted for the Windows command line,
# and returns its exit code, its standard output and standard error lines
# apart, and the seconds it ran. Standard input is inherited, so a program run
# through ssh.exe needs `-n`.
function Invoke-Process {
    param(
        [string]$FilePath,
        [string[]]$Arguments = @()
    )
    $utf8 = New-Object System.Text.UTF8Encoding $false
    $info = New-Object System.Diagnostics.ProcessStartInfo
    $info.FileName = $FilePath
    $info.Arguments = (@($Arguments | ForEach-Object { ConvertTo-CommandLineArgument $_ }) -join ' ')
    $info.UseShellExecute = $false
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardError = $true
    $info.StandardOutputEncoding = $utf8
    $info.StandardErrorEncoding = $utf8
    $clock = [System.Diagnostics.Stopwatch]::StartNew()
    try {
        $process = [System.Diagnostics.Process]::Start($info)
    } catch {
        return [pscustomobject]@{ Exit = -1; Out = @(); Err = @("could not run ${FilePath}: $($_.Exception.Message)"); Seconds = 0.0 }
    }
    try {
        # Read both streams at once, so that neither side waits on a full pipe.
        $out = $process.StandardOutput.ReadToEndAsync()
        $err = $process.StandardError.ReadToEndAsync()
        $outText = $out.GetAwaiter().GetResult()
        $errText = $err.GetAwaiter().GetResult()
        $process.WaitForExit()
        $code = $process.ExitCode
    } finally {
        $process.Dispose()
    }
    $seconds = $clock.Elapsed.TotalSeconds
    return [pscustomobject]@{
        Exit    = $code
        Out     = @(ConvertTo-Lines $outText)
        Err     = @(ConvertTo-Lines $errText)
        Seconds = $seconds
    }
}

function ConvertTo-Lines {
    param([string]$Text)
    if ([string]::IsNullOrEmpty($Text)) { return @() }
    $lines = @($Text -split "`n" | ForEach-Object { $_.TrimEnd("`r") })
    if ($lines[$lines.Count - 1] -eq '') {
        $lines = @($lines | Select-Object -First ($lines.Count - 1))
    }
    return $lines
}

# Runs a command on a Linux node over SSH, with standard input from the null
# device, as Invoke-Process does.
function Invoke-SshProcess {
    param(
        [string]$Destination,
        [string]$Command
    )
    $ssh = @(Get-Command 'ssh.exe' -CommandType Application)[0].Path
    return Invoke-Process -FilePath $ssh -Arguments (@('-n') + @($SshOptions) + @($Destination, $Command))
}

# The detail of a failed run of Invoke-Process: its exit code, then the last
# lines of its standard error and of its standard output.
function Get-ProcessDetail {
    param($Result)
    $detail = @("exit code $($Result.Exit), after $([Math]::Round($Result.Seconds, 1)) s")
    $detail += @($Result.Err | Select-Object -Last 15 | ForEach-Object { "stderr: $_" })
    $detail += @($Result.Out | Select-Object -Last 15 | ForEach-Object { "stdout: $_" })
    return $detail
}

# ---------------------------------------------------------------- reserved objects

# Whether a network interface or adapter name is one that ARFC 0017 clause 3.17
# reserves: `al` then a digit, or `alv` then a digit.
function Test-ReservedInterface {
    param([string]$Name)
    return $Name -cmatch '^alv?[0-9]'
}

# The lines of `ip -d link show` or `ip address show` without the blocks of the
# reserved interfaces. A block starts with a line `<index>: <name>[@<link>]:`
# and holds the indented lines after it.
function Remove-ReservedLinkBlocks {
    param([string[]]$Lines)
    $keep = $true
    $out = @()
    foreach ($line in $Lines) {
        if ($line -cmatch '^[0-9]+: ([^:@ ]+)(@[^: ]*)?:') {
            $keep = -not (Test-ReservedInterface $Matches[1])
        }
        if ($keep) { $out += $line }
    }
    return $out
}

# The lines of `ip route show table all` or `ip -6 route show table all`
# without the routes clause 3.17 lets Alloy hold: every device the route names
# is reserved, and the route is in the table `main`. A route in the table
# `local` with `proto kernel` on a reserved device also counts, since the
# kernel adds it for an address on that device. A route starts with an
# unindented line and holds the indented lines after it (the next hops of a
# multipath route).
function Remove-ReservedRoutes {
    param([string[]]$Lines)
    $entries = @()
    $current = $null
    foreach ($line in $Lines) {
        if ($null -eq $current -or $line -notmatch '^\s') {
            $current = New-Object 'System.Collections.Generic.List[string]'
            $entries += ,$current
        }
        $current.Add($line)
    }
    $out = @()
    foreach ($entry in $entries) {
        $text = (@($entry) -join ' ')
        $devices = @([regex]::Matches($text, '(?:^|\s)dev\s+(\S+)') | ForEach-Object { $_.Groups[1].Value })
        $reserved = ($devices.Count -gt 0) -and (@($devices | Where-Object { -not (Test-ReservedInterface $_) }).Count -eq 0)
        $table = 'main'
        $match = [regex]::Match($text, '(?:^|\s)table\s+(\S+)')
        if ($match.Success) { $table = $match.Groups[1].Value }
        $kernelLocal = ($table -ceq 'local') -and ($text -cmatch '(?:^|\s)proto\s+kernel(?:\s|$)')
        if (-not ($reserved -and (($table -ceq 'main') -or $kernelLocal))) {
            $out += @($entry)
        }
    }
    return $out
}

# The lines of `nft list ruleset` without the reserved tables, those named
# `alloy` or starting with `alloy_`, in any family. A table runs from its line
# `table <family> <name> {` to the next line `}`.
function Remove-ReservedNftTables {
    param([string[]]$Lines)
    $out = @()
    $inTable = $false
    $keep = $true
    foreach ($line in $Lines) {
        if (-not $inTable -and $line -cmatch '^table\s+\S+\s+(\S+)\s*\{') {
            $inTable = $true
            $keep = -not ($Matches[1] -cmatch '^alloy(_.*)?$')
        }
        if ($keep) { $out += $line }
        if ($inTable -and $line -ceq '}') {
            $inTable = $false
            $keep = $true
        }
    }
    return $out
}

# The search path the install leaves (clause 3.15 step 6): the entry appended
# when absent.
function Get-PathWithEntry {
    param(
        [string]$Path,
        [string]$Entry
    )
    if (@($Path -split ';' | Where-Object { $_ -ieq $Entry }).Count -gt 0) { return $Path }
    if ($Path -eq '' -or $Path.EndsWith(';')) { return $Path + $Entry }
    return $Path + ';' + $Entry
}

# A copy of the snapshot $After without the reserved objects of ARFC 0017
# clause 3.17, for the rule of clause 3.17.6 that a joined node's snapshot
# differs from the one taken before the install only in reserved objects.
# Compared with $Before, the snapshot taken before the install, any difference
# left is in an object that is not reserved. $Before is left as it is: a clean
# machine holds no reserved object, so one found there shows as a difference.
function Get-SnapshotWithoutReserved {
    param(
        $Machine,
        $Before,
        $After
    )
    if ($null -eq $After) { return $null }
    $a = Copy-Snapshot $After
    if ($Machine.Kind -eq 'windows') {
        if ($a.Contains('Get-NetAdapter')) {
            $a['Get-NetAdapter'] = @($a['Get-NetAdapter'] | Where-Object {
                    -not (($_ -cmatch '^Name=([^;]*)(;|$)') -and (Test-ReservedInterface $Matches[1])) })
        }
        foreach ($name in @('Get-NetIPAddress', 'Get-NetRoute')) {
            if ($a.Contains($name)) {
                $a[$name] = @($a[$name] | Where-Object {
                        -not (($_ -cmatch '(^|; )InterfaceAlias=([^;]*)(;|$)') -and (Test-ReservedInterface $Matches[2])) })
            }
        }
        if ($a.Contains('Get-Service')) {
            $a['Get-Service'] = @($a['Get-Service'] | Where-Object { $_ -cnotlike 'Name=alloyd;*' })
        }
        if ($null -ne $Before -and $Before.Contains('machine PATH') -and $a.Contains('machine PATH') -and
            @($Before['machine PATH']).Count -eq 1 -and @($a['machine PATH']).Count -eq 1) {
            $before = ([string]@($Before['machine PATH'])[0]).Substring('PATH='.Length)
            $entry = Join-Path $Machine.ProgramFiles 'Alloy'
            if (('PATH=' + (Get-PathWithEntry $before $entry)) -ceq [string]@($a['machine PATH'])[0]) {
                $a['machine PATH'] = @("PATH=$before")
            }
        }
        return $a
    }
    foreach ($name in @('ip -d link show', 'ip address show')) {
        if ($a.Contains($name)) { $a[$name] = @(Remove-ReservedLinkBlocks @($a[$name])) }
    }
    foreach ($name in @('ip route show table all', 'ip -6 route show table all')) {
        if ($a.Contains($name)) { $a[$name] = @(Remove-ReservedRoutes @($a[$name])) }
    }
    if ($a.Contains('nft list ruleset')) {
        $a['nft list ruleset'] = @(Remove-ReservedNftTables @($a['nft list ruleset']))
    }
    if ($a.Contains('sysctl -a (net.)')) {
        $a['sysctl -a (net.)'] = @($a['sysctl -a (net.)'] | Where-Object {
                -not (($_ -cmatch '^net\.ipv[46]\.(conf|neigh)\.([^.\s]+)\.') -and (Test-ReservedInterface $Matches[2])) })
    }
    if ($a.Contains('systemctl list-unit-files')) {
        $a['systemctl list-unit-files'] = @($a['systemctl list-unit-files'] | Where-Object {
                $_ -cnotmatch '^(alloyd\.service|alloyd-\S+)(\s|$)' })
    }
    return $a
}

# ---------------------------------------------------------------- nodes

# The default node name of a hostname, by the rule of ARFC 0002 section 3.3.
function ConvertTo-DefaultNodeName {
    param([string]$HostName)
    $name = $HostName.ToLowerInvariant()
    $dot = $name.IndexOf('.')
    if ($dot -ge 0) { $name = $name.Substring(0, $dot) }
    $name = [regex]::Replace($name, '[^a-z0-9-]', '-')
    if ($name -cnotmatch '^[a-z]') { $name = 'node-' + $name }
    if ($name.Length -gt 63) { $name = $name.Substring(0, 63) }
    return $name
}

# The names nodes with these default names get when they join in this order:
# a name in use gets the lowest free suffix of -2, -3, and so on, the base cut
# so that the result stays within 63 characters (ARFC 0002 section 3.3).
function Get-JoinedNodeNames {
    param([string[]]$DefaultNames)
    $taken = New-Object 'System.Collections.Generic.HashSet[string]' ([StringComparer]::Ordinal)
    $names = @()
    foreach ($base in $DefaultNames) {
        $name = $base
        $n = 2
        while ($taken.Contains($name)) {
            $suffix = "-$n"
            $cut = $base
            if ($cut.Length + $suffix.Length -gt 63) { $cut = $cut.Substring(0, 63 - $suffix.Length) }
            $name = $cut + $suffix
            $n += 1
        }
        [void]$taken.Add($name)
        $names += $name
    }
    return $names
}

# The roles in the order ARFC 0019 clause 3.1.15 prints them.
$RoleOrder = @('anchor', 'control', 'compute', 'storage')

# The ROLES column of a node with these roles (ARFC 0019 clause 3.1.15).
function Format-RolesColumn {
    param([string[]]$Roles)
    $ordered = @($RoleOrder | Where-Object { @($Roles) -ccontains $_ })
    if ($ordered.Count -eq 0) { return '-' }
    return ($ordered -join ',')
}

# A field of a Node in the JSON form of the API, found on the object itself or
# in its part (`spec`, `status`, or `metadata`): ARFC 0006 section 2.6 defers
# the JSON spelling to ARFC 0008 section 2, which does not give it.
function Get-JsonField {
    param(
        $Object,
        [string]$Name,
        [string]$Part
    )
    $property = $Object.PSObject.Properties[$Name]
    if ($null -ne $property) { return $property.Value }
    $container = $Object.PSObject.Properties[$Part]
    if ($null -ne $container -and $container.Value -is [System.Management.Automation.PSCustomObject]) {
        $property = $container.Value.PSObject.Properties[$Name]
        if ($null -ne $property) { return $property.Value }
    }
    return $null
}

# Parses the standard output of `alloy node list --output json`: one JSON
# array of Node objects (ARFC 0019 clause 3.1.15). Returns Ok, Error, and the
# nodes, each with Name (without a leading `nodes/`), State, and Roles. A Node
# with no roles field has no roles, as the JSON form of an empty list may leave
# the field out.
function ConvertFrom-NodeListJson {
    param([string[]]$Lines)
    $result = [pscustomobject]@{ Ok = $false; Error = ''; Nodes = @() }
    $text = (@($Lines) -join "`n").Trim()
    if (-not $text.StartsWith('[')) {
        $result.Error = 'standard output is not a JSON array'
        return $result
    }
    try {
        $parsed = ConvertFrom-Json -InputObject $text
    } catch {
        $result.Error = "standard output is not JSON: $($_.Exception.Message)"
        return $result
    }
    $items = @()
    if ($null -ne $parsed) { $items = @($parsed) }
    $nodes = @()
    foreach ($item in $items) {
        if ($item -isnot [System.Management.Automation.PSCustomObject]) {
            $result.Error = 'an element of the array is not an object'
            return $result
        }
        $name = [string](Get-JsonField $item 'name' 'metadata')
        if ($name.StartsWith('nodes/')) { $name = $name.Substring('nodes/'.Length) }
        $roles = @(@(Get-JsonField $item 'roles' 'spec') | Where-Object { $null -ne $_ } | ForEach-Object { [string]$_ })
        $nodes += [pscustomobject]@{
            Name  = $name
            State = [string](Get-JsonField $item 'state' 'status')
            Roles = $roles
        }
    }
    $result.Nodes = $nodes
    $result.Ok = $true
    return $result
}

function Format-Node {
    param($Node)
    return ('{0} state={1} roles={2}' -f $Node.Name, $Node.State, (@($Node.Roles) -join ','))
}

# Compares listed nodes with the expected ones, each with Name and Roles.
# Returns the problems found as three lists: Members (a node missing, extra,
# or listed twice), Roles, and States (a node that is not `active`, which is
# how the node list of M11 rung 2 shows a node healthy, ARFC 0006 clause
# 3.15).
function Get-NodeListProblems {
    param(
        $Nodes,
        $Expected
    )
    $members = @()
    $roles = @()
    $states = @()
    $listed = @($Nodes)
    foreach ($want in @($Expected)) {
        $found = @($listed | Where-Object { $_.Name -ceq $want.Name })
        if ($found.Count -eq 0) {
            $members += "missing: $($want.Name)"
            continue
        }
        if ($found.Count -gt 1) { $members += "listed $($found.Count) times: $($want.Name)" }
        $have = @($found[0].Roles | Sort-Object)
        $need = @($want.Roles | Sort-Object)
        if ((@($have) -join ',') -cne (@($need) -join ',')) {
            $roles += "$($want.Name): want roles '$(Format-RolesColumn $want.Roles)', found '$(@($found[0].Roles) -join ',')'"
        }
    }
    foreach ($node in $listed) {
        if (@($Expected | Where-Object { $_.Name -ceq $node.Name }).Count -eq 0) {
            $members += "not expected: $(Format-Node $node)"
        }
        if ($node.State -cne 'active') {
            $states += "not active: $(Format-Node $node)"
        }
    }
    return [pscustomobject]@{ Members = $members; Roles = $roles; States = $states }
}

# Checks the table of `alloy node list` against ARFC 0019 clauses 3.1.4 and
# 3.1.15 for nodes in one zone: the header, each value starting at its column,
# each column as wide as its widest value or name and followed by two spaces,
# no line ending in a space, no escape character in output to a pipe, one row
# per expected node sorted by name in ascending byte order, each `active` with
# its roles. Returns the problems found, empty when there is none.
function Get-NodeTableProblems {
    param(
        [string[]]$Lines,
        $Expected
    )
    $problems = @()
    $all = @($Lines)
    if ($all.Count -eq 0) { return @('standard output is empty') }
    foreach ($line in $all) {
        if ($line.IndexOf([char]27) -ge 0) { $problems += "an escape character in output to a pipe: $line" }
        if ($line -cmatch '\s$') { $problems += "a line ends in a space: '$line'" }
    }
    $header = $all[0]
    $columns = @([regex]::Matches($header, '\S+'))
    $want = @('NAME', 'STATE', 'ROLES', 'MESH', 'RELEASE', 'ADDRESS')
    $found = @($columns | ForEach-Object { $_.Value })
    if ((@($found) -join ' ') -cne ($want -join ' ')) {
        $problems += "header: want the columns '$($want -join ' ')', found '$header'"
        return $problems
    }
    $starts = @($columns | ForEach-Object { $_.Index })
    $last = $starts.Count - 1
    $rows = @()
    foreach ($row in @($all | Select-Object -Skip 1)) {
        $values = @()
        for ($i = 0; $i -le $last; $i++) {
            $start = $starts[$i]
            if ($row.Length -le $start) {
                $problems += "row '$row': no value in column $($want[$i])"
                $values += ''
                continue
            }
            $end = $row.Length
            if ($i -lt $last -and $starts[$i + 1] -lt $end) { $end = $starts[$i + 1] }
            $cell = $row.Substring($start, $end - $start)
            if ($cell.StartsWith(' ')) {
                $problems += "row '$row': column $($want[$i]) does not start where its name does"
            }
            if ($i -lt $last -and -not $cell.EndsWith('  ')) {
                $problems += "row '$row': column $($want[$i]) is not followed by two spaces"
            }
            $values += $cell.Trim()
        }
        $rows += ,$values
    }
    for ($i = 0; $i -lt $last; $i++) {
        $width = $want[$i].Length
        foreach ($values in $rows) {
            if ($values[$i].Length -gt $width) { $width = $values[$i].Length }
        }
        $actual = $starts[$i + 1] - $starts[$i] - 2
        if ($actual -ne $width) {
            $problems += "column $($want[$i]) is $actual wide, want $width, its widest value or name"
        }
    }
    $names = @($rows | ForEach-Object { $_[0] })
    for ($i = 1; $i -lt $names.Count; $i++) {
        if ([string]::CompareOrdinal($names[$i - 1], $names[$i]) -ge 0) {
            $problems += "rows not sorted by name: '$($names[$i - 1])' before '$($names[$i])'"
        }
    }
    if ($rows.Count -ne @($Expected).Count) {
        $problems += "want $(@($Expected).Count) rows, found $($rows.Count)"
    }
    foreach ($node in @($Expected)) {
        $match = @($rows | Where-Object { $_[0] -ceq $node.Name })
        if ($match.Count -eq 0) {
            $problems += "no row for $($node.Name)"
            continue
        }
        if ($match[0][1] -cne 'active') { $problems += "$($node.Name): STATE '$($match[0][1])', want 'active'" }
        $roles = Format-RolesColumn $node.Roles
        if ($match[0][2] -cne $roles) { $problems += "$($node.Name): ROLES '$($match[0][2])', want '$roles'" }
    }
    foreach ($name in $names) {
        if (@($Expected | Where-Object { $_.Name -ceq $name }).Count -eq 0) { $problems += "row not expected: $name" }
    }
    return $problems
}

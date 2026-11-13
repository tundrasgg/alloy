# Alloy install script for Windows (ARFC 0017 clauses 3.13 to 3.17).
#
#   irm https://github.com/tundrasgg/alloy/releases/download/v<version>/install.ps1 | iex
#
# Run as a file it takes -Version <version>, -Uninstall, -Purge, -Force, and
# -Yes, and exits with the codes of clause 3.15. Run through Invoke-Expression
# it never calls exit: a failure ends with a terminating error whose message is
# the one it prints, and the PowerShell session stays open.
#
# This file in the source tree is a template. The release workflow writes the
# release's version and the SHA-256 of each of its archives into the
# placeholders in Get-AlloyRelease, and a script whose placeholders are not
# filled in refuses to run.
#
# Every top-level statement before the last line only defines a function or
# sets a variable, and the last line starts the work inside braces, so that
# Invoke-Expression given any prefix of this file runs nothing (clause 3.15).
#
# Test only: when the environment variable ALLOY_INSTALL_TEST_RELEASES names a
# directory, the script reads each release asset from
# <directory>\v<version>\<asset> instead of downloading it from GitHub. The
# digests written into the script are checked all the same. It exists for dry
# runs and must never be set on a machine that is not a disposable test
# machine.
#
# The file is ASCII, so that Windows PowerShell 5.1 reads it the same with or
# without a byte order mark.

$AlloyScriptArgs = $args
$AlloyScriptPath = $MyInvocation.MyCommand.Path

function Get-AlloyRelease {
    @{
        Version = '@ALLOY_VERSION@'
        Sha256 = @{
            'linux-x86_64' = '@ALLOY_SHA256_LINUX_X86_64@'
            'linux-aarch64' = '@ALLOY_SHA256_LINUX_AARCH64@'
            'macos-x86_64' = '@ALLOY_SHA256_MACOS_X86_64@'
            'macos-aarch64' = '@ALLOY_SHA256_MACOS_AARCH64@'
            'windows-x86_64' = '@ALLOY_SHA256_WINDOWS_X86_64@'
            'windows-aarch64' = '@ALLOY_SHA256_WINDOWS_AARCH64@'
        }
    }
}

# BEGIN SHARED: the text from here to END SHARED is identical in install.ps1
# and uninstall.ps1. packaging/check.sh compares the two.

$AlloyReleasesUrl = 'https://github.com/tundrasgg/alloy/releases/download'

function Get-AlloyPaths {
    $pf = $env:ProgramW6432
    if (-not $pf) { $pf = [Environment]::GetFolderPath('ProgramFiles') }
    $pd = [Environment]::GetFolderPath('CommonApplicationData')
    $root = Join-Path $pf 'Alloy'
    @{
        Root = $root
        Manifest = Join-Path $root 'manifest'
        DataDir = Join-Path $pd 'Alloy'
        Service = 'alloyd'
    }
}

function Write-AlloySay([string]$Text) {
    Write-Host $Text
}

# A step line: two spaces, a status word padded to six columns, the text.
function Write-AlloyStep([string]$Word, [string]$Text) {
    Write-Host ('  {0,-6} {1}' -f $Word, $Text)
}

# Ends the script with an exit code, a message, and an optional Try: line.
function Stop-Alloy([int]$Code, [string]$Message, [string]$Try) {
    $text = $Message
    if ($Try) { $text = $text + [Environment]::NewLine + 'Try: ' + $Try }
    $ex = New-Object System.Exception $text
    $ex.Data['AlloyExitCode'] = $Code
    throw $ex
}

function New-AlloyTemp {
    $p = Join-Path ([IO.Path]::GetTempPath()) ('alloy.' + [Guid]::NewGuid().ToString('N').Substring(0, 12))
    [void](New-Item -ItemType Directory -Path $p -Force)
    $AlloyState.Tmp = $p
}

function Remove-AlloyTemp {
    if ($AlloyState.Tmp -and (Test-Path -LiteralPath $AlloyState.Tmp)) {
        Remove-Item -LiteralPath $AlloyState.Tmp -Recurse -Force -ErrorAction SilentlyContinue
    }
    $AlloyState.Tmp = $null
}

function Test-AlloyElevated {
    $id = [Security.Principal.WindowsIdentity]::GetCurrent()
    (New-Object Security.Principal.WindowsPrincipal $id).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
}

function Test-AlloyWindows {
    if ($PSVersionTable.PSEdition -eq 'Core') { return [bool](Get-Variable -Name IsWindows -ValueOnly -ErrorAction SilentlyContinue) }
    return $true
}

function Test-AlloyPreview([string]$Version) {
    return $Version -match '-preview\.'
}

function Test-AlloyExists([string]$Path) {
    return [bool](Get-Item -LiteralPath $Path -Force -ErrorAction SilentlyContinue)
}

function ConvertTo-AlloyLines([string]$Text) {
    if ($Text.Length -eq 0 -or -not $Text.EndsWith("`n")) { return $null }
    return ,($Text.Substring(0, $Text.Length - 1).Split([char]10))
}

function Write-AlloyText([string]$Path, [string]$Text) {
    $enc = New-Object System.Text.UTF8Encoding $false
    $fs = [IO.File]::Open($Path, [IO.FileMode]::Create, [IO.FileAccess]::Write, [IO.FileShare]::None)
    try {
        $bytes = $enc.GetBytes($Text)
        $fs.Write($bytes, 0, $bytes.Length)
        $fs.Flush($true)
    } finally {
        $fs.Close()
    }
}

# --- Manifest (section 4.1) ----------------------------------------------------

function Test-AlloyManifestLines($Lines) {
    if ($null -eq $Lines -or $Lines.Count -lt 4) { return $false }
    if ($Lines[0] -cne 'alloy-manifest 1') { return $false }
    if ($Lines[1] -cnotmatch '^version [0-9A-Za-z.+-]+$') { return $false }
    if ($Lines[2] -cnotmatch '^kind (node|client|cli)$') { return $false }
    if ($Lines[3] -cnotmatch '^state (pending|complete)$') { return $false }
    for ($i = 4; $i -lt $Lines.Count; $i++) {
        $l = $Lines[$i]
        if ($l -cmatch '^dir .' -or $l -cmatch '^file [0-9a-f]{64} .' -or $l -cmatch '^unit [A-Za-z0-9@._-]+$' -or
            $l -cmatch '^service .' -or $l -cmatch '^path .') { continue }
        return $false
    }
    return $true
}

# An entry of a manifest line: Kind, Path (or name), and Sha for a file.
function ConvertTo-AlloyEntry([string]$Line) {
    $kind = $Line.Substring(0, $Line.IndexOf(' '))
    $rest = $Line.Substring($kind.Length + 1)
    $sha = $null
    if ($kind -ceq 'file') {
        $sha = $rest.Substring(0, 64)
        $rest = $rest.Substring(65)
    }
    New-Object PSObject -Property @{ Kind = $kind; Path = $rest; Sha = $sha; Line = $Line }
}

# An uninstall moves the install directory to its .alloy-old name only when it
# holds nothing but the manifest, and moves it back when something else
# appeared in it in the meantime. Interrupted between those two moves, it
# leaves the manifest in a directory under the .alloy-old name that is not a
# marker. Returns the manifest's path there, or $null.
function Get-AlloyDisplaced {
    $root = $AlloyState.Paths.Root
    $old = "$root.alloy-old"
    if (Test-AlloyExists $root) { return $null }
    $item = Get-Item -LiteralPath $old -Force -ErrorAction SilentlyContinue
    if (-not $item -or -not $item.PSIsContainer) { return $null }
    if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) { return $null }
    if (Test-AlloyMarker $old) { return $null }
    $dm = Join-Path $old 'manifest'
    $mi = Get-Item -LiteralPath $dm -Force -ErrorAction SilentlyContinue
    if (-not $mi -or $mi.PSIsContainer) { return $null }
    if ($mi.Attributes -band [IO.FileAttributes]::ReparsePoint) { return $null }
    return $dm
}

# Moves the directory that Get-AlloyDisplaced found back to its path, so that
# the manifest is where it belongs again.
function Restore-AlloyDisplaced {
    if (-not $AlloyState.Displaced) { return }
    $root = $AlloyState.Paths.Root
    $moved = $false
    if (-not (Test-AlloyExists $root)) {
        try {
            [IO.Directory]::Move("$root.alloy-old", $root)
            $moved = $true
        } catch {
        }
    }
    if (-not $moved) {
        Stop-Alloy 1 "Could not move $root.alloy-old, left by an interrupted uninstall, back to $root." 'run this again'
    }
    Write-AlloyStep 'done' "moved $root.alloy-old, left by an interrupted uninstall, back to $root"
    $AlloyState.Displaced = $false
}

# Reads the manifest, from where an interrupted uninstall left it when it is
# not at its path (Get-AlloyDisplaced).
function Read-AlloyManifest {
    $AlloyState.Manifest = $null
    $AlloyState.Displaced = $false
    $m = $AlloyState.Paths.Manifest
    if (-not (Test-AlloyExists $m)) {
        $m = Get-AlloyDisplaced
        if (-not $m) { return }
        $AlloyState.Displaced = $true
    }
    if (-not (Test-Path -LiteralPath $m -PathType Leaf)) {
        Stop-Alloy 1 "The install manifest $m is not a file, so it cannot be read. Nothing was changed."
    }
    try {
        $text = [IO.File]::ReadAllText($m)
    } catch {
        Stop-Alloy 1 "Could not read the install manifest $m."
    }
    $lines = ConvertTo-AlloyLines $text
    if (-not (Test-AlloyManifestLines $lines)) {
        Stop-Alloy 1 "The install manifest $m is damaged: its format is not one this script reads. Nothing was changed."
    }
    $entries = @()
    for ($i = 4; $i -lt $lines.Count; $i++) { $entries += ConvertTo-AlloyEntry $lines[$i] }
    $state = $lines[3].Substring(6)
    if ($AlloyState.Displaced) {
        $root = $AlloyState.Paths.Root
        if (-not ($entries | Where-Object { $_.Kind -ceq 'dir' -and $_.Path -ieq $root })) {
            Stop-Alloy 1 "The install manifest $m does not list $root, so it was not left there by an uninstall and cannot be trusted. Nothing was changed."
        }
        # An uninstall was under way, so the install it records is not complete.
        $state = 'pending'
    }
    $AlloyState.Manifest = @{
        Version = $lines[1].Substring(8)
        Kind = $lines[2].Substring(5)
        State = $state
        Entries = $entries
    }
}

function Test-AlloyListed([string]$Kind, [string]$Path) {
    if (-not $AlloyState.Manifest) { return $false }
    foreach ($e in $AlloyState.Manifest.Entries) {
        if ($e.Kind -ceq $Kind -and $e.Path -ieq $Path) { return $true }
    }
    return $false
}

# --- Interrupted first installs (T-0092, review finding F3) ------------------
#
# A first install creates the install directory and the manifest in it in one
# rename: it builds them as <directory>.alloy-new and renames that into place,
# and an uninstall removes them the same way through <directory>.alloy-old.
# Such a directory is a marker: it holds nothing but, at most, the manifest.
# Every install and uninstall recognizes and removes it. An uninstall moves the
# directory to its .alloy-old name only when it already holds nothing else, so
# that no interruption leaves another product's file away from its path.

function Test-AlloyMarker([string]$Path) {
    $item = Get-Item -LiteralPath $Path -Force -ErrorAction SilentlyContinue
    if (-not $item -or -not $item.PSIsContainer) { return $false }
    if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) { return $false }
    foreach ($c in @(Get-ChildItem -LiteralPath $Path -Force)) {
        if ($c.Name -cne 'manifest' -or $c.PSIsContainer) { return $false }
        if ($c.Attributes -band [IO.FileAttributes]::ReparsePoint) { return $false }
    }
    return $true
}

# A function that ends with "return ,$out" returns its array as one object, so
# its caller assigns the result to a variable and loops over that: a foreach
# over the call itself sees the whole array, even an empty one, as one item.
function Get-AlloyMarkers {
    $out = @()
    foreach ($t in @("$($AlloyState.Paths.Root).alloy-new", "$($AlloyState.Paths.Root).alloy-old")) {
        if ((Test-AlloyExists $t) -and (Test-AlloyMarker $t)) { $out += $t }
    }
    return ,$out
}

# --- Data directory and host file (ARFC 0002 section 3.12, section 4.2) -------

$AlloySystemSid = 'S-1-5-18'
$AlloyAdminsSid = 'S-1-5-32-544'

function Test-AlloyHostLines($Lines) {
    if ($null -eq $Lines -or $Lines.Count -lt 3) { return $false }
    if ($Lines[0] -cne 'alloy-host 1') { return $false }
    if ($Lines[1] -cnotmatch '^join (unjoined|joining|joined)$') { return $false }
    if ($Lines[2] -cnotmatch '^release [0-9A-Za-z.+-]+$') { return $false }
    for ($i = 3; $i -lt $Lines.Count; $i++) {
        $l = $Lines[$i]
        if ($l -cmatch '^(link|netns|run) [0-9a-f-]{36} .' -or $l -cmatch '^nft [0-9a-f-]{36} \S+ .' -or
            $l -cmatch '^adapter \{[0-9a-f-]+\} .') { continue }
        return $false
    }
    return $true
}

function Test-AlloyOwner($Acl) {
    $owner = $Acl.GetOwner([Security.Principal.SecurityIdentifier]).Value
    return ($owner -eq $AlloySystemSid -or $owner -eq $AlloyAdminsSid)
}

# Sets DD to absent, other, or alloy, and for an Alloy data directory HostJoin,
# HostRelease, and HostLines. Ends with exit 1 when the host file of a
# directory that meets the other conditions cannot be parsed.
function Read-AlloyDataDir {
    $AlloyState.DD = 'absent'
    $AlloyState.HostJoin = 'unjoined'
    $AlloyState.HostRelease = 'none'
    $AlloyState.HostLines = @()
    $dd = $AlloyState.Paths.DataDir
    $item = Get-Item -LiteralPath $dd -Force -ErrorAction SilentlyContinue
    if (-not $item) { return }
    $AlloyState.DD = 'other'
    if (-not $item.PSIsContainer) { return }
    if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) { return }
    $acl = Get-Acl -LiteralPath $dd
    if (-not (Test-AlloyOwner $acl)) { return }
    if (-not $acl.AreAccessRulesProtected) { return }
    $full = @{}
    foreach ($r in $acl.GetAccessRules($true, $true, [Security.Principal.SecurityIdentifier])) {
        if ($r.AccessControlType -ne [Security.AccessControl.AccessControlType]::Allow) { continue }
        $sid = $r.IdentityReference.Value
        if ($sid -ne $AlloySystemSid -and $sid -ne $AlloyAdminsSid) { return }
        $fc = [Security.AccessControl.FileSystemRights]::FullControl
        if (($r.FileSystemRights -band $fc) -eq $fc) { $full[$sid] = $true }
    }
    if (-not ($full[$AlloySystemSid] -and $full[$AlloyAdminsSid])) { return }
    $hf = Join-Path $dd 'alloy-host'
    $hitem = Get-Item -LiteralPath $hf -Force -ErrorAction SilentlyContinue
    if (-not $hitem -or $hitem.PSIsContainer) { return }
    if ($hitem.Attributes -band [IO.FileAttributes]::ReparsePoint) { return }
    $hacl = Get-Acl -LiteralPath $hf
    if (-not (Test-AlloyOwner $hacl)) { return }
    if ($hacl.AreAccessRulesProtected) { return }
    foreach ($r in $hacl.GetAccessRules($true, $true, [Security.Principal.SecurityIdentifier])) {
        if (-not $r.IsInherited) { return }
    }
    try {
        $text = [IO.File]::ReadAllText($hf)
    } catch {
        Stop-Alloy 1 "Could not read the host file $hf."
    }
    $lines = ConvertTo-AlloyLines $text
    if (-not (Test-AlloyHostLines $lines)) {
        Stop-Alloy 1 "The host file $hf cannot be read: its format is not one this script reads. Nothing was changed."
    }
    $AlloyState.DD = 'alloy'
    $AlloyState.HostJoin = $lines[1].Substring(5)
    $AlloyState.HostRelease = $lines[2].Substring(8)
    $AlloyState.HostLines = $lines
}

function Get-AlloyAdapters {
    return @(Get-NetAdapter -IncludeHidden -ErrorAction SilentlyContinue)
}

# Each host file line that matches its object: Description (as
# Get-AlloyReserved names it), Line, and the adapter.
function Get-AlloyHostMatches {
    $out = @()
    if ($AlloyState.DD -ne 'alloy') { return ,$out }
    $adapters = Get-AlloyAdapters
    for ($i = 3; $i -lt $AlloyState.HostLines.Count; $i++) {
        $l = $AlloyState.HostLines[$i]
        if ($l -cnotmatch '^adapter (\{[0-9a-f-]+\}) (.+)$') { continue }
        $guid = $Matches[1]
        $name = $Matches[2]
        if ($name -notmatch '^al[0-9]') { continue }
        foreach ($a in $adapters) {
            if ($a.Name -eq $name -and ([string]$a.InterfaceGuid).ToLowerInvariant() -ceq $guid) {
                $out += New-Object PSObject -Property @{ Description = "network adapter $name"; Line = $l; Adapter = $a }
            }
        }
    }
    return ,$out
}

# One line for each object with a name that clause 3.17 reserves, other than
# files, the search path entry, and the data directory.
function Get-AlloyReserved {
    $out = @()
    foreach ($a in Get-AlloyAdapters) {
        if ($a.Name -match '^al[0-9]') { $out += "network adapter $($a.Name)" }
    }
    if (Get-Service -Name $AlloyState.Paths.Service -ErrorAction SilentlyContinue) {
        $out += "service $($AlloyState.Paths.Service)"
    }
    return ,$out
}

# --- Search path ------------------------------------------------------------------

$AlloyEnvKey = 'SYSTEM\CurrentControlSet\Control\Session Manager\Environment'

function Get-AlloyMachinePath {
    $k = [Microsoft.Win32.Registry]::LocalMachine.OpenSubKey($AlloyEnvKey)
    try {
        return [string]$k.GetValue('Path', '', [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
    } finally {
        $k.Close()
    }
}

function Test-AlloyPathHas([string]$Value, [string]$Entry) {
    foreach ($p in $Value.Split(';')) {
        if ($p.TrimEnd('\') -ieq $Entry.TrimEnd('\')) { return $true }
    }
    return $false
}

function Send-AlloySettingChange {
    if (-not ('Alloy.Native' -as [type])) {
        Add-Type -Namespace Alloy -Name Native -MemberDefinition @'
[DllImport("user32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
public static extern IntPtr SendMessageTimeout(IntPtr hWnd, uint Msg, UIntPtr wParam, string lParam, uint fuFlags, uint uTimeout, out UIntPtr lpdwResult);
'@
    }
    $r = [UIntPtr]::Zero
    [void][Alloy.Native]::SendMessageTimeout([IntPtr]0xffff, 0x1A, [UIntPtr]::Zero, 'Environment', 2, 5000, [ref]$r)
}

function Set-AlloyMachinePath([string]$Value) {
    $k = [Microsoft.Win32.Registry]::LocalMachine.OpenSubKey($AlloyEnvKey, $true)
    try {
        $k.SetValue('Path', $Value, [Microsoft.Win32.RegistryValueKind]::ExpandString)
    } finally {
        $k.Close()
    }
    Send-AlloySettingChange
}

function Remove-AlloyPathEntry([string]$Entry) {
    $value = Get-AlloyMachinePath
    if (-not (Test-AlloyPathHas $value $Entry)) { return }
    $kept = @()
    foreach ($p in $value.Split(';')) {
        if ($p.TrimEnd('\') -ine $Entry.TrimEnd('\')) { $kept += $p }
    }
    Set-AlloyMachinePath ($kept -join ';')
    Write-AlloyStep 'done' "removed $Entry from the machine search path"
}

# --- Removal helpers ----------------------------------------------------------

function Test-AlloyEmptyDir([string]$Path) {
    return (@(Get-ChildItem -LiteralPath $Path -Force -ErrorAction SilentlyContinue).Count -eq 0)
}

# Removes a temporary name of a manifest path: a file, an empty directory, or a
# marker. Returns $false when it exists and could not be removed.
function Remove-AlloyTempName([string]$Path) {
    $item = Get-Item -LiteralPath $Path -Force -ErrorAction SilentlyContinue
    if (-not $item) { return $true }
    try {
        if ($item.PSIsContainer) {
            if ((Test-AlloyMarker $Path) -or (Test-AlloyEmptyDir $Path)) {
                Remove-Item -LiteralPath $Path -Recurse -Force -ErrorAction Stop
            } else {
                Write-AlloyStep 'warn' "left $Path in place: it is not empty"
                return $false
            }
        } else {
            Remove-Item -LiteralPath $Path -Force -ErrorAction Stop
        }
    } catch {
        return $false
    }
    return $true
}

function Remove-AlloyTempNames([string]$Path) {
    $a = Remove-AlloyTempName "$Path.alloy-new"
    $b = Remove-AlloyTempName "$Path.alloy-old"
    return ($a -and $b)
}

# Removes one manifest entry with its temporary names. Returns 'removed',
# 'failed' when it could not be removed, or 'left' for a directory left in
# place because it holds files the install did not place (clause 3.16 step 7
# removes directories that are empty).
function Remove-AlloyEntry($Entry) {
    $p = $Entry.Path
    switch -CaseSensitive ($Entry.Kind) {
        'file' {
            $item = Get-Item -LiteralPath $p -Force -ErrorAction SilentlyContinue
            if ($item -and $item.PSIsContainer) {
                Write-AlloyStep 'warn' "left $p in place: the manifest lists a file there, and it is a directory"
                return 'failed'
            }
            if ($item) {
                try {
                    Remove-Item -LiteralPath $p -Force -ErrorAction Stop
                } catch {
                    Write-AlloyStep 'warn' "could not remove $p, which is in use"
                    return 'failed'
                }
                Write-AlloyStep 'done' "removed $p"
            }
            [void](Remove-AlloyTempNames $p)
        }
        'dir' {
            $item = Get-Item -LiteralPath $p -Force -ErrorAction SilentlyContinue
            if ($item -and $item.PSIsContainer) {
                if (-not (Test-AlloyEmptyDir $p)) {
                    [void](Remove-AlloyTempNames $p)
                    Write-AlloyStep 'warn' "left $p in place: it holds files the install did not place"
                    return 'left'
                }
                try { Remove-Item -LiteralPath $p -Force -ErrorAction Stop } catch { return 'failed' }
                Write-AlloyStep 'done' "removed $p"
            }
            [void](Remove-AlloyTempNames $p)
        }
        'service' {
            if (Get-Service -Name $p -ErrorAction SilentlyContinue) {
                $out = & sc.exe delete $p 2>&1
                if ($LASTEXITCODE -ne 0) {
                    Write-AlloyStep 'warn' "could not delete the service ${p}: $out"
                    return 'failed'
                }
                Write-AlloyStep 'done' "removed the service $p"
            }
        }
        'path' {
            Remove-AlloyPathEntry $p
        }
        default {
            Write-AlloyStep 'warn' "skipped the manifest entry '$($Entry.Line)', which Windows does not have"
        }
    }
    return 'removed'
}

# Removes the manifest and the install directory that holds it. When nothing
# else is in the directory, it goes in one rename, so that an interruption
# leaves a marker that the next run recognizes. A directory that holds anything
# else is never moved: the manifest is removed from it, and it is left in place.
function Remove-AlloyManifest {
    $root = $AlloyState.Paths.Root
    $m = $AlloyState.Paths.Manifest
    [void](Remove-AlloyTempNames $m)
    if ((Test-AlloyListed 'dir' $root) -and (Test-AlloyMarker $root) -and (Remove-AlloyTempName "$root.alloy-old")) {
        $moved = $false
        try {
            [IO.Directory]::Move($root, "$root.alloy-old")
            $moved = $true
        } catch {
        }
        if ($moved) {
            if (Test-AlloyMarker "$root.alloy-old") {
                Remove-Item -LiteralPath "$root.alloy-old" -Recurse -Force
                Write-AlloyStep 'done' "removed $root"
                return
            }
            # Another file appeared in it after the check. Get-AlloyDisplaced
            # finds it on the next run.
            $back = $false
            try {
                [IO.Directory]::Move("$root.alloy-old", $root)
                $back = $true
            } catch {
            }
            if (-not $back) { Stop-Alloy 1 "Could not move $root.alloy-old back to $root." 'run this uninstall again' }
        }
    }
    Remove-Item -LiteralPath $m -Force
    Write-AlloyStep 'done' "removed $m"
    if ((Test-AlloyListed 'dir' $root) -and (Test-AlloyExists $root)) {
        if (Test-AlloyEmptyDir $root) {
            Remove-Item -LiteralPath $root -Force
        } else {
            Write-AlloyStep 'warn' "left $root in place: it holds files the install did not place"
        }
    }
}

function Format-AlloySize([double]$Bytes) {
    $units = 'B', 'KiB', 'MiB', 'GiB', 'TiB', 'PiB'
    $i = 0
    while ($Bytes -ge 1024 -and $i -lt 5) { $Bytes = $Bytes / 1024; $i++ }
    if ($Bytes -lt 10 -and $i -gt 0) { return ('{0:0.0} {1}' -f $Bytes, $units[$i]) }
    return ('{0:0} {1}' -f [Math]::Floor($Bytes), $units[$i])
}

# Sends each process a termination request, waits up to 30 seconds for all of
# them to end, and kills those still running (clause 3.16 step 5).
function Stop-AlloyProcesses($Ids) {
    if (@($Ids).Count -eq 0) { return }
    foreach ($id in $Ids) { [void](& taskkill.exe /PID $id 2>&1) }
    $deadline = (Get-Date).AddSeconds(30)
    while ((Get-Date) -lt $deadline) {
        $alive = @($Ids | Where-Object { Get-Process -Id $_ -ErrorAction SilentlyContinue })
        if ($alive.Count -eq 0) { return }
        Start-Sleep -Seconds 1
    }
    foreach ($id in $Ids) {
        if (Get-Process -Id $id -ErrorAction SilentlyContinue) {
            Stop-Process -Id $id -Force -ErrorAction SilentlyContinue
            Write-AlloyStep 'done' "killed process $id after 30 s"
        }
    }
}

# --- Uninstall (clause 3.16) -----------------------------------------------------

function Invoke-AlloyUninstall([string]$Try) {
    # Step 1.
    if (-not (Test-AlloyElevated)) {
        Stop-Alloy 4 'Uninstalling Alloy needs an elevated PowerShell. Nothing was changed.' $Try
    }
    New-AlloyTemp
    Write-AlloySay 'Uninstalling Alloy'
    $paths = $AlloyState.Paths

    # Step 2.
    Read-AlloyManifest
    Read-AlloyDataDir
    $markers = Get-AlloyMarkers
    if (-not $AlloyState.Manifest -and $AlloyState.DD -ne 'alloy' -and $markers.Count -eq 0) {
        if ($AlloyState.DD -eq 'other') { Write-AlloySay "Left $($paths.DataDir) in place: it is not an Alloy data directory." }
        Write-AlloySay 'Alloy is not installed on this machine.'
        return
    }

    # Step 3: a Windows machine runs no VMM (ARFC 0002 section 3.11).

    # Step 4.
    if ($AlloyState.Purge -and $AlloyState.DD -eq 'alloy') {
        if (-not $AlloyState.Yes) {
            if ([Console]::IsInputRedirected -or -not [Environment]::UserInteractive) {
                Stop-Alloy 2 "-Purge deletes the data directory $($paths.DataDir) and asks first, and there is no terminal to ask on. Nothing was changed." 'add -Yes to confirm'
            }
            $answer = Read-Host "Delete the data directory $($paths.DataDir), with this node's keys and state? Type $($paths.DataDir) to confirm"
            if ($answer -ne $paths.DataDir) { Stop-Alloy 1 'Not confirmed. Nothing was changed.' }
        }
        if ($AlloyState.HostJoin -eq 'joined' -or $AlloyState.HostJoin -eq 'joining') {
            $alloy = Join-Path $paths.Root 'alloy.exe'
            if ((Test-AlloyListed 'file' $alloy) -and (Test-Path -LiteralPath $alloy -PathType Leaf)) {
                Write-AlloySay 'Leaving the cluster'
                & $alloy node leave --yes
                $rc = $LASTEXITCODE
                if ($rc -ne 0) {
                    if ($AlloyState.Force) {
                        Write-AlloyStep 'warn' "alloy node leave failed with exit code $rc; going on because of -Force. The node record lasts until it expires or a cloud admin deletes it."
                    } else {
                        Stop-Alloy $rc "alloy node leave failed with exit code $rc, so this computer is still a node of its cluster. Nothing more was changed." 'fix what alloy node leave reported, or add -Force to remove the node anyway'
                    }
                }
            } elseif ($AlloyState.Force) {
                Write-AlloyStep 'warn' 'alloy is not installed, so the node cannot leave its cluster; going on because of -Force. The node record lasts until it expires or a cloud admin deletes it.'
            } else {
                Stop-Alloy 5 'This computer is a node of a cluster, and alloy is not installed, so it cannot leave the cluster. Nothing was changed.' 'add -Force to remove the node anyway'
            }
        }
    }

    # Step 5.
    Write-AlloySay 'Stopping Alloy'
    $exes = @()
    if ($AlloyState.Manifest) {
        $svc = Get-Service -Name $paths.Service -ErrorAction SilentlyContinue
        if ((Test-AlloyListed 'service' $paths.Service) -and $svc) {
            Set-Service -Name $paths.Service -StartupType Disabled
            if ($svc.Status -ne 'Stopped') {
                $cim = Get-CimInstance Win32_Service -Filter "Name='$($paths.Service)'"
                [void](& sc.exe stop $paths.Service 2>&1)
                $deadline = (Get-Date).AddSeconds(30)
                while ((Get-Date) -lt $deadline -and (Get-Service -Name $paths.Service).Status -ne 'Stopped') { Start-Sleep -Seconds 1 }
                if ((Get-Service -Name $paths.Service).Status -ne 'Stopped' -and $cim.ProcessId) {
                    Stop-Process -Id $cim.ProcessId -Force -ErrorAction SilentlyContinue
                }
            }
            Write-AlloyStep 'done' "stopped and disabled the service $($paths.Service)"
        }
        foreach ($e in $AlloyState.Manifest.Entries) {
            if ($e.Kind -ceq 'file') { $exes += $e.Path; $exes += "$($e.Path).alloy-old" }
        }
    }
    $ids = @()
    foreach ($proc in @(Get-CimInstance Win32_Process)) {
        $exe = $proc.ExecutablePath
        if (-not $exe) { continue }
        foreach ($x in $exes) {
            if ($exe -ieq $x) { $ids += $proc.ProcessId }
        }
    }
    if ($ids.Count -gt 0) {
        Write-AlloyStep 'done' "asked $($ids.Count) processes to stop"
        Stop-AlloyProcesses $ids
    }

    # Step 6.
    if ($AlloyState.DD -eq 'alloy') {
        Write-AlloySay 'Removing host objects'
        $found = Get-AlloyHostMatches
        foreach ($m in $found) {
            $out = & pnputil.exe /remove-device $m.Adapter.PnPDeviceID 2>&1
            if ($LASTEXITCODE -ne 0) {
                Stop-Alloy 1 "Could not remove $($m.Description): $out" 'run this uninstall again'
            }
            Write-AlloyStep 'done' "removed $($m.Description)"
        }
        if ($AlloyState.HostLines.Count -gt 3) {
            $hf = Join-Path $paths.DataDir 'alloy-host'
            $text = ($AlloyState.HostLines[0..2] -join "`n") + "`n"
            try {
                Write-AlloyText "$hf.new" $text
                [IO.File]::Replace("$hf.new", $hf, [NullString]::Value)
            } catch {
                Stop-Alloy 1 "Could not rewrite the host file $hf." 'run this uninstall again'
            }
            Write-AlloyStep 'done' 'rewrote the host file'
        }
    }

    # Step 7.
    if ($AlloyState.Manifest) {
        Write-AlloySay 'Removing installed files'
        Restore-AlloyDisplaced
        $entries = @($AlloyState.Manifest.Entries)
        [array]::Reverse($entries)
        $left = $false
        foreach ($e in $entries) {
            if ($e.Kind -ceq 'dir' -and $e.Path -ieq $paths.Root) { continue }
            if ((Remove-AlloyEntry $e) -ceq 'failed') { $left = $true }
        }
        if ($left) {
            Stop-Alloy 1 "Some installed files could not be removed, so the install manifest $($paths.Manifest) is kept." 'run this uninstall again'
        }
        Remove-AlloyManifest
    }
    $markers = Get-AlloyMarkers
    foreach ($mk in $markers) {
        Remove-Item -LiteralPath $mk -Recurse -Force
        Write-AlloyStep 'done' "removed $mk, left by an interrupted install"
    }

    # Step 8.
    if ($AlloyState.DD -eq 'alloy') {
        if ($AlloyState.Purge) {
            Remove-Item -LiteralPath $paths.DataDir -Recurse -Force
            Write-AlloyStep 'done' "removed the data directory $($paths.DataDir)"
        } else {
            $sum = (Get-ChildItem -LiteralPath $paths.DataDir -Recurse -Force -ErrorAction SilentlyContinue |
                Where-Object { -not $_.PSIsContainer } | Measure-Object -Property Length -Sum).Sum
            if (-not $sum) { $sum = 0 }
            Write-AlloySay "Kept the data directory $($paths.DataDir) ($(Format-AlloySize $sum)). It holds this node's keys and state."
            Write-AlloySay "To delete it, from an elevated PowerShell: Remove-Item -Recurse -Force '$($paths.DataDir)'"
        }
    } elseif ($AlloyState.DD -eq 'other') {
        Write-AlloySay "Left $($paths.DataDir) in place: it is not an Alloy data directory."
    }
    $reserved = Get-AlloyReserved
    foreach ($o in $reserved) {
        Write-AlloySay "Left $o in place: Alloy did not create it."
    }
    Write-AlloySay 'Kept each user''s CLI configuration in %APPDATA%\alloy and the access keys the CLI stored in Windows Credential Manager.'
    Write-AlloySay 'Alloy is uninstalled.'
}

# END SHARED

# --- Install (clause 3.15) -------------------------------------------------------

function Test-AlloyVersion([string]$Version) {
    return $Version -cmatch '^[0-9]+\.[0-9]+\.[0-9]+(-preview\.[0-9]+)?$'
}

# A script whose placeholders the release workflow did not fill in refuses to
# run. The checks name no placeholder, so the workflow's replacement cannot
# change them.
function Assert-AlloyRelease {
    $r = Get-AlloyRelease
    $bad = -not (Test-AlloyVersion $r.Version)
    foreach ($d in $r.Sha256.Values) {
        if ($d -and $d -cnotmatch '^[0-9a-f]{64}$') { $bad = $true }
    }
    if ($bad) {
        Stop-Alloy 1 'This install script is a template from the Alloy source tree, not the script of a release: its version and digests are not filled in.' 'use the install.ps1 of a release, from https://github.com/tundrasgg/alloy/releases'
    }
}

function Stop-AlloyUsage([string]$Message) {
    Stop-Alloy 2 ($Message + [Environment]::NewLine + 'Usage: install.ps1 [-Version <version>] [-Uninstall [-Purge] [-Force] [-Yes]]')
}

function Read-AlloyArguments($Arguments) {
    $o = @{ Version = $null; Uninstall = $false; Rest = @(); Count = 0 }
    $list = @($Arguments)
    for ($i = 0; $i -lt $list.Count; $i++) {
        $a = [string]$list[$i]
        if ($a -ieq '-Version') {
            if ($i + 1 -ge $list.Count) { Stop-AlloyUsage '-Version needs a version, such as 0.0.1-preview.1.' }
            $v = [string]$list[$i + 1]
            if (-not (Test-AlloyVersion $v)) { Stop-AlloyUsage "$v is not a version: a version is MAJOR.MINOR.PATCH, or 0.0.PATCH-preview.N." }
            $o.Version = $v
            $i++
            continue
        }
        if ($a -ieq '-Uninstall') { $o.Uninstall = $true }
        elseif ($a -ieq '-Purge') { $AlloyState.Purge = $true }
        elseif ($a -ieq '-Force') { $AlloyState.Force = $true }
        elseif ($a -ieq '-Yes') { $AlloyState.Yes = $true }
        else { Stop-AlloyUsage "Unknown argument $a." }
        $o.Rest += $a
        $o.Count++
    }
    if (-not $o.Uninstall -and $o.Count -gt 0) { Stop-AlloyUsage '-Purge, -Force, and -Yes go with -Uninstall.' }
    return $o
}

# Returns 0 when downloaded, 2 when an attempt got an HTTP status of 400 or
# more, 3 when no attempt got an HTTP response, and 4 when an attempt got a
# response and the transfer failed.
function Invoke-AlloyDownload([string]$Rel, [string]$Dest) {
    $testDir = $env:ALLOY_INSTALL_TEST_RELEASES
    if ($testDir) {
        $src = Join-Path $testDir ($Rel.Replace('/', '\'))
        if (Test-Path -LiteralPath $src -PathType Leaf) {
            Copy-Item -LiteralPath $src -Destination $Dest -Force
            return 0
        }
        return 2
    }
    $proto = [Net.ServicePointManager]::SecurityProtocol
    if ([int]$proto -ne 0 -and -not ($proto -band [Net.SecurityProtocolType]::Tls12)) {
        [Net.ServicePointManager]::SecurityProtocol = $proto -bor [Net.SecurityProtocolType]::Tls12
    }
    $url = "$AlloyReleasesUrl/$Rel"
    $status = $null
    for ($attempt = 1; $attempt -le 3; $attempt++) {
        try {
            $req = [Net.HttpWebRequest]::Create($url)
            $req.Timeout = 10000
            $req.ReadWriteTimeout = 30000
            $req.AllowAutoRedirect = $true
            $resp = $req.GetResponse()
            try {
                $status = [int]$resp.StatusCode
                $in = $resp.GetResponseStream()
                $out = [IO.File]::Create($Dest)
                try { $in.CopyTo($out) } finally { $out.Close(); $in.Close() }
                return 0
            } finally {
                $resp.Close()
            }
        } catch {
            $e = $_.Exception
            while ($e) {
                if ($e -is [Net.WebException] -and $e.Response) {
                    $status = [int]$e.Response.StatusCode
                    $e.Response.Close()
                    break
                }
                $e = $e.InnerException
            }
        }
        if ($attempt -lt 3) { Start-Sleep -Seconds 2 }
    }
    if ($null -eq $status) { return 3 }
    if ($status -ge 400) { return 2 }
    return 4
}

function Get-AlloySha256([string]$Path) {
    return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
}

# Step 1.
function Get-AlloyPlatform {
    if (-not (Test-AlloyWindows)) {
        Stop-Alloy 1 'install.ps1 runs on Windows. On Linux and macOS, use install.sh.'
    }
    $envKey = 'HKLM:\SYSTEM\CurrentControlSet\Control\Session Manager\Environment'
    $native = (Get-ItemProperty -Path $envKey -Name PROCESSOR_ARCHITECTURE).PROCESSOR_ARCHITECTURE
    switch ($native) {
        'AMD64' { $AlloyState.Arch = 'x86_64' }
        'ARM64' { $AlloyState.Arch = 'aarch64' }
        default { Stop-Alloy 1 "Alloy runs on x86_64 and aarch64, and this computer is $native." }
    }
    Write-AlloySay "Alloy $($AlloyState.Version) for windows-$($AlloyState.Arch)"
    $build = [int](Get-ItemProperty -Path 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion' -Name CurrentBuildNumber).CurrentBuildNumber
    if ($build -lt 17763) {
        Stop-Alloy 1 "Alloy needs Windows 10 version 1809 (build 17763) or later, and this computer runs build $build."
    }
    $AlloyState.Kind = 'client'
}

# Step 4, for one path: it exists, or a temporary name of it does, and the
# earlier manifest does not list it.
function Get-AlloyPathConflicts([string]$Kind, [string]$Path) {
    $out = @()
    if (Test-AlloyListed $Kind $Path) { return ,$out }
    foreach ($c in @($Path, "$Path.alloy-new", "$Path.alloy-old")) {
        if (-not (Test-AlloyExists $c)) { continue }
        if ($c -ne $Path -and (Test-AlloyMarker $c)) { continue }
        $out += $c
    }
    return ,$out
}

function Stop-AlloyConflicts($Conflicts) {
    Write-AlloySay 'These already exist, and this install did not create them:'
    foreach ($c in $Conflicts) { Write-AlloySay "  $c" }
    Stop-Alloy 5 'Alloy installs only where nothing else is, so nothing was changed.' 'remove or rename what is listed above, then run the install again'
}

# Step 4.
function Test-AlloyConflicts {
    $paths = $AlloyState.Paths
    $conflicts = @()
    if (-not $AlloyState.Manifest) {
        $found = Get-AlloyHostMatches
        $owned = @($found | ForEach-Object { $_.Description })
        $reserved = Get-AlloyReserved
        foreach ($o in $reserved) {
            if ($owned -notcontains $o) { $conflicts += $o }
        }
        if ($AlloyState.DD -eq 'other') { $conflicts += "$($paths.DataDir), which is not an Alloy data directory" }
    } elseif ((Get-Service -Name $paths.Service -ErrorAction SilentlyContinue) -and -not (Test-AlloyListed 'service' $paths.Service)) {
        $conflicts += "service $($paths.Service)"
    }
    $conflicts += Get-AlloyPathConflicts 'dir' $paths.Root
    if ($conflicts.Count -gt 0) { Stop-AlloyConflicts $conflicts }
    $hr = $AlloyState.HostRelease
    $v = $AlloyState.Version
    if (($AlloyState.HostJoin -eq 'joined' -or $AlloyState.HostJoin -eq 'joining') -and $hr -ne 'none' -and $hr -ne $v) {
        if ((Test-AlloyPreview $hr) -or (Test-AlloyPreview $v)) {
            Stop-Alloy 5 "This computer is a node on release $hr, and this install is release $v. A node on a preview release, or moving to one, changes release only by an uninstall with -Purge and a new alloy join. Nothing was changed." "powershell -NoProfile -ExecutionPolicy Bypass -File `"$($paths.Root)\uninstall.ps1`" -Purge, then install $v and run alloy join"
        }
    }
}

function Test-AlloyInstalled {
    $m = $AlloyState.Manifest
    if (-not $m) { return $false }
    if ($m.State -cne 'complete' -or $m.Version -cne $AlloyState.Version -or $m.Kind -cne $AlloyState.Kind) { return $false }
    foreach ($e in $m.Entries) {
        if ($e.Kind -ceq 'file') {
            if (-not (Test-Path -LiteralPath $e.Path -PathType Leaf)) { return $false }
            if ((Get-AlloySha256 $e.Path) -cne $e.Sha) { return $false }
        }
    }
    return [bool](Get-Service -Name $AlloyState.Paths.Service -ErrorAction SilentlyContinue)
}

# Checks every entry of the archive against clause 3.13, then extracts it.
function Expand-AlloyArchive([string]$Archive, [string]$Name) {
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $top = $AlloyState.Top
    try {
        $zip = [IO.Compression.ZipFile]::OpenRead($Archive)
    } catch {
        Stop-Alloy 1 "The archive $Name cannot be read."
    }
    try {
        foreach ($e in $zip.Entries) {
            $n = $e.FullName
            if ($n -match '^[/\\]' -or $n -match '^[A-Za-z]:') { Stop-Alloy 1 "The archive $Name has the absolute entry $n, so nothing was installed." }
            if (('/' + $n.Replace('\', '/') + '/') -match '/\.\./') { Stop-Alloy 1 "The archive $Name has the entry $n, which leaves its directory, so nothing was installed." }
            if ($n -cnotmatch '^[A-Za-z0-9._/+-]+$') { Stop-Alloy 1 "The archive $Name has an entry with an unexpected name, so nothing was installed." }
            if ($n -cne "$top/" -and -not $n.StartsWith("$top/", [StringComparison]::Ordinal)) {
                Stop-Alloy 1 "The archive $Name has the entry $n outside $top/, so nothing was installed."
            }
            if ($e.PSObject.Properties['ExternalAttributes']) {
                $type = ($e.ExternalAttributes -shr 16) -band 0xF000
                if ($type -ne 0 -and $type -ne 0x8000 -and $type -ne 0x4000) {
                    Stop-Alloy 1 "The archive $Name has the entry $n, which is a link or a device, so nothing was installed."
                }
            }
        }
    } finally {
        $zip.Dispose()
    }
    $x = Join-Path $AlloyState.Tmp 'x'
    try {
        [IO.Compression.ZipFile]::ExtractToDirectory($Archive, $x)
    } catch {
        Stop-Alloy 1 "The archive $Name could not be extracted."
    }
    foreach ($item in @(Get-ChildItem -LiteralPath $x -Recurse -Force)) {
        if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) {
            Stop-Alloy 1 "The archive $Name has an entry that is a link, so nothing was installed."
        }
    }
    foreach ($need in @('bin\alloy.exe', 'bin\alloyd.exe', 'uninstall.ps1')) {
        if (-not (Test-Path -LiteralPath (Join-Path (Join-Path $x $top) $need) -PathType Leaf)) {
            Stop-Alloy 1 "The archive $Name has no $($need.Replace('\', '/'))."
        }
    }
}

# Step 5.
function Get-AlloyArchive {
    $release = Get-AlloyRelease
    $key = "windows-$($AlloyState.Arch)"
    $sha = $release.Sha256[$key]
    $asset = "alloy-$($AlloyState.Version)-$key.zip"
    if (-not $sha) { Stop-Alloy 1 "Release $($AlloyState.Version) has no build for $key." }
    Write-AlloySay "Downloading $asset"
    $dest = Join-Path $AlloyState.Tmp $asset
    $rc = Invoke-AlloyDownload "v$($AlloyState.Version)/$asset" $dest
    switch ($rc) {
        0 { }
        2 { Stop-Alloy 1 "Release $($AlloyState.Version) has no build for ${key}: $AlloyReleasesUrl/v$($AlloyState.Version)/$asset was not found." }
        3 { Stop-Alloy 6 "Could not reach github.com to download $asset, after 3 attempts. Nothing was changed." "check this computer's connection to github.com, then run the install again" }
        default { Stop-Alloy 1 "The download of $asset failed, after 3 attempts. Nothing was changed." 'run the install again' }
    }
    $got = Get-AlloySha256 $dest
    if ($got -cne $sha) {
        Stop-Alloy 1 "The archive $asset has the SHA-256 $got, and this release's digest for it is $sha, so nothing was installed."
    }
    Write-AlloyStep 'ok' "checked the SHA-256 of $asset"
    $AlloyState.Top = "alloy-$($AlloyState.Version)-$key"
    Expand-AlloyArchive $dest $asset
    Write-AlloyStep 'ok' 'checked and extracted every entry'
}

function New-AlloyEntry([string]$Kind, [string]$Path, [string]$Source) {
    $sha = $null
    if ($Kind -ceq 'file') { $sha = Get-AlloySha256 $Source }
    $line = "$Kind $Path"
    if ($sha) { $line = "file $sha $Path" }
    New-Object PSObject -Property @{ Kind = $Kind; Path = $Path; Sha = $sha; Source = $Source; Line = $line }
}

# The manifest entries of this release, in creation order.
function Get-AlloyPlan {
    $paths = $AlloyState.Paths
    $root = $paths.Root
    $src = Join-Path (Join-Path $AlloyState.Tmp 'x') $AlloyState.Top
    $plan = @()
    if (-not (Test-AlloyExists $root) -or (Test-AlloyListed 'dir' $root)) { $plan += New-AlloyEntry 'dir' $root $null }
    foreach ($f in @(Get-ChildItem -LiteralPath (Join-Path $src 'bin') -Force | Sort-Object Name)) {
        if ($f.PSIsContainer) { Stop-Alloy 1 "The archive has bin/$($f.Name), and bin/ holds only files." }
        $plan += New-AlloyEntry 'file' (Join-Path $root $f.Name) $f.FullName
    }
    $lic = Join-Path $src 'licenses'
    $plan += New-AlloyEntry 'dir' (Join-Path $root 'licenses') $null
    foreach ($f in @(Get-ChildItem -LiteralPath $lic -Recurse -Force | Sort-Object FullName)) {
        $rel = $f.FullName.Substring($lic.Length + 1)
        $target = Join-Path (Join-Path $root 'licenses') $rel
        if ($f.PSIsContainer) { $plan += New-AlloyEntry 'dir' $target $null }
        else { $plan += New-AlloyEntry 'file' $target $f.FullName }
    }
    $plan += New-AlloyEntry 'file' (Join-Path $root 'uninstall.ps1') (Join-Path $src 'uninstall.ps1')
    $plan += New-AlloyEntry 'service' $paths.Service $null
    if ((Test-AlloyListed 'path' $root) -or -not (Test-AlloyPathHas (Get-AlloyMachinePath) $root)) {
        $plan += New-AlloyEntry 'path' $root $null
    }
    # Step 4, for the paths that only the archive names.
    $conflicts = @()
    foreach ($e in $plan) {
        if ($e.Kind -ceq 'file') { $conflicts += Get-AlloyPathConflicts 'file' $e.Path }
    }
    if ($conflicts.Count -gt 0) { Stop-AlloyConflicts $conflicts }
    return ,$plan
}

function Get-AlloyManifestText([string]$State, $Lines) {
    $head = "alloy-manifest 1`nversion $($AlloyState.Version)`nkind $($AlloyState.Kind)`nstate $State`n"
    $body = ''
    foreach ($l in $Lines) { $body += "$l`n" }
    return $head + $body
}

# Installs the file $Source as $Target: written as its .alloy-new name and
# renamed over the target, or, when the target is in use, after the target is
# renamed to its .alloy-old name.
function Set-AlloyFile([string]$Source, [string]$Target) {
    $new = "$Target.alloy-new"
    $old = "$Target.alloy-old"
    if (-not (Remove-AlloyTempName $new)) { Stop-Alloy 1 "Could not remove $new." 'run the install again' }
    Copy-Item -LiteralPath $Source -Destination $new -Force
    if (Test-Path -LiteralPath $Target) {
        try {
            [IO.File]::Replace($new, $Target, [NullString]::Value)
            return
        } catch {
        }
        if (-not (Remove-AlloyTempName $old)) { Stop-Alloy 1 "Could not replace ${Target}: it and $old are in use." 'stop what runs it, then run the install again' }
        [IO.File]::Move($Target, $old)
    }
    [IO.File]::Move($new, $Target)
}

function Set-AlloyText([string]$Target, [string]$Text) {
    $t = Join-Path $AlloyState.Tmp 'manifest.txt'
    Write-AlloyText $t $Text
    Set-AlloyFile $t $Target
}

# Writes the pending manifest. With no manifest yet, the install directory is
# created as <directory>.alloy-new with the manifest inside, and renamed into
# place in one step.
function Write-AlloyPendingManifest([string]$Text) {
    $paths = $AlloyState.Paths
    if (Test-AlloyExists $paths.Manifest) {
        Set-AlloyText $paths.Manifest $Text
        return
    }
    $root = $paths.Root
    if (Test-AlloyExists $root) { Stop-AlloyConflicts @($root) }
    if (-not (Remove-AlloyTempNames $root)) { Stop-AlloyConflicts @("$root.alloy-new", "$root.alloy-old") }
    $stage = "$root.alloy-new"
    [void](New-Item -ItemType Directory -Path $stage)
    Write-AlloyText (Join-Path $stage 'manifest') $Text
    [IO.Directory]::Move($stage, $root)
}

function Set-AlloyService([string]$Exe) {
    $name = $AlloyState.Paths.Service
    $image = '"' + $Exe + '"'
    if (-not (Get-Service -Name $name -ErrorAction SilentlyContinue)) {
        [void](New-Service -Name $name -BinaryPathName $image -DisplayName 'Alloy' -Description 'Alloy node daemon (alloyd). https://github.com/tundrasgg/alloy' -StartupType Disabled)
        Write-AlloyStep 'done' "created the service $name"
    }
    $cim = Get-CimInstance Win32_Service -Filter "Name='$name'"
    if ($cim.PathName -ne $image) {
        [void](& sc.exe config $name binPath= $image 2>&1)
    }
    [void](& sc.exe failure $name reset= 86400 actions= restart/5000/restart/5000/restart/5000 2>&1)
    [void](& sc.exe failureflag $name 1 2>&1)
    $cim = Get-CimInstance Win32_Service -Filter "Name='$name'"
    if ($cim.PathName -ne $image) {
        Stop-Alloy 1 "The service $name runs $($cim.PathName), not $image." 'run the install again'
    }
}

# Step 6.
function Install-AlloyFiles($Plan) {
    $paths = $AlloyState.Paths
    Write-AlloySay 'Installing'
    $seen = @{}
    $lines = @()
    if ($AlloyState.Manifest) {
        foreach ($e in $AlloyState.Manifest.Entries) {
            $k = "$($e.Kind) $($e.Path)".ToLowerInvariant()
            if (-not $seen.ContainsKey($k)) { $seen[$k] = $true; $lines += $e.Line }
        }
    }
    foreach ($e in $Plan) {
        $k = "$($e.Kind) $($e.Path)".ToLowerInvariant()
        if (-not $seen.ContainsKey($k)) { $seen[$k] = $true; $lines += $e.Line }
    }
    $pending = $lines
    Restore-AlloyDisplaced
    Write-AlloyPendingManifest (Get-AlloyManifestText 'pending' $pending)
    $AlloyState.Changed = $false
    $alloyd = Join-Path $paths.Root 'alloyd.exe'
    foreach ($e in $Plan) {
        switch -CaseSensitive ($e.Kind) {
            'dir' {
                if (-not (Test-AlloyExists $e.Path)) { [void](New-Item -ItemType Directory -Path $e.Path) }
            }
            'file' {
                if ((Test-Path -LiteralPath $e.Path -PathType Leaf) -and (Get-AlloySha256 $e.Path) -ceq $e.Sha) { continue }
                Set-AlloyFile $e.Source $e.Path
                if ($e.Path -notlike (Join-Path $paths.Root 'licenses\*')) { $AlloyState.Changed = $true }
                Write-AlloyStep 'done' $e.Path
            }
            'service' {
                Set-AlloyService $alloyd
            }
            'path' {
                $value = Get-AlloyMachinePath
                if (-not (Test-AlloyPathHas $value $e.Path)) {
                    if ($value -and -not $value.EndsWith(';')) { $value += ';' }
                    Set-AlloyMachinePath ($value + $e.Path)
                    Write-AlloyStep 'done' "added $($e.Path) to the machine search path"
                }
            }
        }
    }
    # Remove what the earlier install has and this one does not.
    $kept = @()
    $planKeys = @{}
    foreach ($e in $Plan) { $planKeys["$($e.Kind) $($e.Path)".ToLowerInvariant()] = $true }
    if ($AlloyState.Manifest) {
        $old = @($AlloyState.Manifest.Entries)
        [array]::Reverse($old)
        foreach ($e in $old) {
            if ($planKeys.ContainsKey("$($e.Kind) $($e.Path)".ToLowerInvariant())) { continue }
            if ((Remove-AlloyEntry $e) -ceq 'removed') { continue }
            if ($e.Kind -ceq 'file' -and (Test-AlloyExists $e.Path)) {
                # In use: renamed to its .alloy-old name.
                try { [IO.File]::Move($e.Path, "$($e.Path).alloy-old") } catch { }
            }
            $kept += $e.Line
        }
    }
    # Remove every temporary name of a manifest path that exists and is not in
    # use. An earlier path whose .alloy-old name is in use stays listed.
    foreach ($l in $pending) {
        $e = ConvertTo-AlloyEntry $l
        if ($e.Kind -cne 'file' -and $e.Kind -cne 'dir') { continue }
        if (-not (Remove-AlloyTempNames $e.Path)) {
            if (-not $planKeys.ContainsKey("$($e.Kind) $($e.Path)".ToLowerInvariant())) {
                if ($kept -notcontains $e.Line) { $kept += $e.Line }
            }
        }
    }
    $complete = @()
    foreach ($e in $Plan) { $complete += $e.Line }
    foreach ($l in $kept) { if ($complete -notcontains $l) { $complete += $l } }
    Set-AlloyText $paths.Manifest (Get-AlloyManifestText 'complete' $complete)
    Write-AlloyStep 'done' $paths.Manifest
}

# Step 7.
function Set-AlloyServiceState {
    $name = $AlloyState.Paths.Service
    $m = $AlloyState.Manifest
    if ($m -and $m.State -ceq 'complete') {
        if ($AlloyState.WasRunning -and $AlloyState.Changed) {
            Restart-Service -Name $name
            Write-AlloyStep 'done' "restarted the service $name"
        }
        return
    }
    if ($AlloyState.HostJoin -eq 'joined' -or $AlloyState.HostJoin -eq 'joining') {
        Set-Service -Name $name -StartupType Automatic
        if ($AlloyState.WasRunning -and $AlloyState.Changed) { Restart-Service -Name $name } else { Start-Service -Name $name }
        Write-AlloyStep 'done' "set the service $name to start automatically and started it: this computer is a node, and it resumes"
    }
}

# Step 8.
function Test-AlloyInstall($Plan) {
    $paths = $AlloyState.Paths
    $alloy = Join-Path $paths.Root 'alloy.exe'
    $out = (& $alloy version 2>$null | Out-String).Trim()
    if ($out -cne $AlloyState.Version) {
        Stop-Alloy 1 "$alloy version printed '$out', not $($AlloyState.Version)." 'run the install again'
    }
    Write-AlloyStep 'ok' "alloy version prints $($AlloyState.Version)"
    Write-AlloySay ''
    Write-AlloySay "Installed Alloy $($AlloyState.Version) ($($AlloyState.Kind)):"
    foreach ($e in $Plan) {
        if ($e.Kind -ceq 'file' -and $e.Path -notlike (Join-Path $paths.Root 'licenses\*')) { Write-AlloySay "  $($e.Path)" }
    }
    Write-AlloySay "  $(Join-Path $paths.Root 'licenses')\"
    Write-AlloySay "  $($paths.Manifest)"
    $svc = Get-Service -Name $paths.Service
    $start = (Get-CimInstance Win32_Service -Filter "Name='$($paths.Service)'").StartMode
    Write-AlloySay "Service $($paths.Service): start type $start, $($svc.Status)"
    if (-not (Test-AlloyPathHas $env:Path $paths.Root)) { $env:Path = $env:Path.TrimEnd(';') + ';' + $paths.Root }
    $all = @(Get-Command alloy -All -CommandType Application -ErrorAction SilentlyContinue)
    foreach ($c in $all) {
        if ($c.Path -ine $alloy) {
            Write-AlloySay "Warning: another alloy is installed at $($c.Path). The search path finds $($all[0].Path) first."
        }
    }
    Write-AlloySay 'Next: alloy join <join token> from an elevated PowerShell, or alloy login <login code>'
}

function Invoke-AlloyInstall {
    Get-AlloyPlatform
    # Step 2.
    if (-not (Test-AlloyElevated)) {
        Stop-Alloy 4 'Installing Alloy needs an elevated PowerShell. Nothing was changed.' 'open PowerShell with Run as administrator, then run the install again'
    }
    # Step 3.
    if ($PSVersionTable.PSVersion -lt [Version]'5.1') {
        Stop-Alloy 1 "This script needs PowerShell 5.1 or later, and this is PowerShell $($PSVersionTable.PSVersion)."
    }
    New-AlloyTemp
    Read-AlloyManifest
    Read-AlloyDataDir
    Test-AlloyConflicts
    if (Test-AlloyInstalled) {
        Write-AlloySay "Alloy $($AlloyState.Version) is already installed. Nothing was changed."
        return
    }
    Get-AlloyArchive
    $plan = Get-AlloyPlan
    $svc = Get-Service -Name $AlloyState.Paths.Service -ErrorAction SilentlyContinue
    $AlloyState.WasRunning = [bool]($svc -and $svc.Status -eq 'Running')
    Install-AlloyFiles $plan
    Set-AlloyServiceState
    Test-AlloyInstall $plan
}

# -Version <version>: runs the install script of that release instead.
function Invoke-AlloyOtherRelease([string]$Version, $Rest) {
    New-AlloyTemp
    Write-AlloySay "Downloading the install script of Alloy $Version"
    $dest = Join-Path $AlloyState.Tmp 'install.ps1'
    $rc = Invoke-AlloyDownload "v$Version/install.ps1" $dest
    switch ($rc) {
        0 { }
        2 { Stop-Alloy 1 "There is no release ${Version}: $AlloyReleasesUrl/v$Version/install.ps1 was not found." }
        3 { Stop-Alloy 6 "Could not reach github.com to download the install script of $Version, after 3 attempts." "check this computer's connection to github.com, then run the install again" }
        default { Stop-Alloy 1 "The download of the install script of $Version failed, after 3 attempts." 'run the install again' }
    }
    $ps = (Get-Process -Id $PID).Path
    & $ps -NoProfile -ExecutionPolicy Bypass -File $dest @Rest
    $code = $LASTEXITCODE
    if ($code -ne 0) {
        Stop-Alloy $code "The install script of Alloy $Version failed with exit code $code."
    }
}

function Invoke-AlloyMain($Arguments) {
    Assert-AlloyRelease
    $AlloyState.Version = (Get-AlloyRelease).Version
    $o = Read-AlloyArguments $Arguments
    if (-not (Test-AlloyWindows)) { Stop-Alloy 1 'install.ps1 runs on Windows. On Linux and macOS, use install.sh.' }
    $AlloyState.Paths = Get-AlloyPaths
    if ($o.Version -and $o.Version -cne $AlloyState.Version) {
        Invoke-AlloyOtherRelease $o.Version $o.Rest
        return
    }
    if ($o.Uninstall) {
        $try = "open PowerShell with Run as administrator, then run: & ([scriptblock]::Create((irm $AlloyReleasesUrl/v$($AlloyState.Version)/install.ps1))) $($o.Rest -join ' ')"
        Invoke-AlloyUninstall $try
        return
    }
    Invoke-AlloyInstall
}

function Invoke-AlloyInstallScript {
    $ErrorActionPreference = 'Stop'
    $AlloyState = @{
        Paths = $null
        AsFile = [bool]$AlloyScriptPath
        Tmp = $null
        Displaced = $false
        Purge = $false
        Force = $false
        Yes = $false
    }
    $code = 0
    $message = $null
    try {
        Invoke-AlloyMain $AlloyScriptArgs
    } catch {
        $ex = $_.Exception
        if ($ex.Data.Contains('AlloyExitCode')) {
            $code = [int]$ex.Data['AlloyExitCode']
            $message = $ex.Message
        } else {
            $code = 1
            $message = "The install script failed unexpectedly: $($ex.Message)"
        }
    } finally {
        Remove-AlloyTemp
    }
    if ($code -ne 0) {
        if ($AlloyState.AsFile) {
            [Console]::Error.WriteLine($message)
            exit $code
        }
        throw $message
    }
    if ($AlloyState.AsFile) { exit 0 }
}

& { Invoke-AlloyInstallScript }

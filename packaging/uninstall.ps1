# Alloy uninstall script for Windows (ARFC 0017 clause 3.16). From an elevated
# PowerShell:
#
#   powershell -NoProfile -ExecutionPolicy Bypass -File "$env:ProgramFiles\Alloy\uninstall.ps1" [-Purge] [-Force] [-Yes]
#
# It reads only the install manifest, the host file of the data directory, and
# the reserved names of clause 3.17, whose formats every release shares, so it
# removes the install of any release. install.ps1 -Uninstall does the same and
# needs only the manifest.
#
# Every top-level statement before the last line only defines a function or
# sets a variable, and the last line starts the work inside braces. The file is
# ASCII.

$AlloyScriptArgs = $args
$AlloyScriptPath = $MyInvocation.MyCommand.Path

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

function Invoke-AlloyUninstallScript {
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
        foreach ($a in @($AlloyScriptArgs)) {
            $a = [string]$a
            if ($a -ieq '-Purge') { $AlloyState.Purge = $true }
            elseif ($a -ieq '-Force') { $AlloyState.Force = $true }
            elseif ($a -ieq '-Yes') { $AlloyState.Yes = $true }
            else { Stop-Alloy 2 ("Unknown argument $a." + [Environment]::NewLine + 'Usage: uninstall.ps1 [-Purge] [-Force] [-Yes]') }
        }
        if (-not (Test-AlloyWindows)) { Stop-Alloy 1 'uninstall.ps1 runs on Windows. On Linux and macOS, use uninstall.sh.' }
        $AlloyState.Paths = Get-AlloyPaths
        $try = "open PowerShell with Run as administrator, then run: powershell -NoProfile -ExecutionPolicy Bypass -File `"$(Join-Path $AlloyState.Paths.Root 'uninstall.ps1')`" $(@($AlloyScriptArgs) -join ' ')"
        Invoke-AlloyUninstall $try.TrimEnd()
    } catch {
        $ex = $_.Exception
        if ($ex.Data.Contains('AlloyExitCode')) {
            $code = [int]$ex.Data['AlloyExitCode']
            $message = $ex.Message
        } else {
            $code = 1
            $message = "The uninstall script failed unexpectedly: $($ex.Message)"
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

& { Invoke-AlloyUninstallScript }

# Prints the host snapshot of a Windows machine, ARFC 0017 clause 3.17.6.
#
# Usage, from an elevated PowerShell (Windows PowerShell 5.1 or PowerShell 7):
#
#   powershell -NoProfile -ExecutionPolicy Bypass -File host-snapshot.ps1
#
# It only reads and changes nothing. The output is one section per command of
# the clause, each headed by a line `### <name>`. Each object is printed as one
# line of `Property=value` pairs, the lines of a section are sorted, and the
# properties that are counters, statistics, or timestamps are left out, so that
# two snapshots of an unchanged machine are equal:
#
# - Get-NetAdapter: LinkSpeed and the receive and transmit link speeds are
#   measured rates.
# - Get-NetIPAddress, Get-NetRoute: ValidLifetime and PreferredLifetime count
#   down.
# - Get-Service: Status is left out. Windows starts and stops demand-start and
#   trigger-start services on its own, so it changes with no cause on the
#   machine. The start type is compared, which is what an install changes, as
#   `systemctl list-unit-files` is on Linux.
# - Get-NetFirewallRule: every prompt rule is left out, whatever its action and
#   whether it is enabled. Windows Firewall creates prompt rules from the
#   user's answer when a program listens and no rule allows it, so they follow
#   what the user answers for other programs. A rule is a prompt rule when its
#   Name is `TCP Query User` or `UDP Query User`, a GUID in braces, then a path
#   equal, ignoring case, to the Program that Get-NetFirewallApplicationFilter
#   reports for it; that program is a full long path (a drive letter, `:\`,
#   then names separated by `\`, none `.` or `..`, no `%`, `~`, or `/`); and it
#   is not under `%ProgramFiles%\Alloy\`, with the 64-bit `%ProgramFiles%`,
#   compared ignoring case. Every other rule stays, a prompt rule for a program
#   of Alloy's included.

[CmdletBinding()]
param()

Set-StrictMode -Version 2.0
$ErrorActionPreference = 'Stop'

function Format-Value {
    param($Value)
    if ($null -eq $Value) {
        return ''
    }
    if ($Value -is [string]) {
        return $Value
    }
    if ($Value -is [System.Collections.IEnumerable]) {
        return (@($Value | ForEach-Object { Format-Value $_ }) -join ',')
    }
    return [string]$Value
}

function Format-Objects {
    param(
        [object[]]$Objects,
        [string[]]$Properties
    )
    $lines = foreach ($object in $Objects) {
        $pairs = foreach ($name in $Properties) {
            $property = $object.PSObject.Properties[$name]
            if ($null -eq $property) {
                '{0}=(none)' -f $name
            } else {
                '{0}={1}' -f $name, (Format-Value $property.Value)
            }
        }
        $pairs -join '; '
    }
    if ($null -ne $lines) {
        [string[]]$sorted = @($lines)
        [Array]::Sort($sorted, [StringComparer]::Ordinal)
        $sorted
    }
}

function Write-Section {
    param(
        [string]$Name,
        [scriptblock]$Body
    )
    '### {0}' -f $Name
    try {
        & $Body
    } catch {
        '(error: {0})' -f $_.Exception.Message
    }
}

Write-Section 'Get-NetAdapter' {
    Format-Objects @(Get-NetAdapter) @(
        'Name', 'InterfaceDescription', 'InterfaceGuid', 'ifIndex', 'MacAddress',
        'Status', 'AdminStatus', 'MtuSize', 'Virtual', 'Hidden')
}

Write-Section 'Get-NetIPAddress' {
    Format-Objects @(Get-NetIPAddress) @(
        'InterfaceIndex', 'InterfaceAlias', 'AddressFamily', 'IPAddress', 'PrefixLength',
        'Type', 'PrefixOrigin', 'SuffixOrigin', 'AddressState', 'SkipAsSource', 'Store')
}

Write-Section 'Get-NetRoute' {
    Format-Objects @(Get-NetRoute) @(
        'DestinationPrefix', 'NextHop', 'InterfaceIndex', 'InterfaceAlias', 'AddressFamily',
        'RouteMetric', 'Protocol', 'TypeOfRoute', 'Publish', 'Store')
}

# The install directory of clause 3.15 step 6, with a trailing `\`, as the
# install script finds it: the folder Windows reports to a 64-bit process.
function Get-AlloyInstallDirectory {
    $pf = $env:ProgramW6432
    if (-not $pf) { $pf = [Environment]::GetFolderPath('ProgramFiles') }
    if (-not $pf) { throw 'cannot find the Program Files folder' }
    return $pf.TrimEnd('\') + '\Alloy\'
}

# True when $Program is a full long path: a drive letter, `:\`, then names
# separated by `\`, none of them `.` or `..`, and no character `%`, `~`, or `/`.
function Test-FullLongPath {
    param([string]$Program)
    # [regex]::IsMatch with no options matches case-sensitively, so only the
    # ASCII letters count as a drive letter: under -match, which ignores case,
    # [A-Za-z] also takes U+212A KELVIN SIGN. \z is the very end of the string,
    # where $ also matches before a final newline.
    if (-not [regex]::IsMatch($Program, '\A[A-Za-z]:\\[^\\]+(?:\\[^\\]+)*\z')) {
        return $false
    }
    if ($Program.IndexOfAny([char[]]@('%', '~', '/')) -ge 0) {
        return $false
    }
    foreach ($name in $Program.Substring(3).Split('\')) {
        if ($name -eq '.' -or $name -eq '..') {
            return $false
        }
    }
    return $true
}

# True when $Rule is a prompt rule of clause 3.17.6, which the snapshot leaves
# out. Any rule in another form stays, so a doubtful rule is a difference.
function Test-PromptRule {
    param($Rule, $Programs, [string]$InstallDirectory)
    # Case-sensitive, and the path is the whole rest of Name: Singleline lets .
    # take a newline and \z is the very end, so a Name whose path differs from
    # Program only by a final newline does not match.
    $match = [regex]::Match([string]$Rule.Name,
        '\A(?:TCP|UDP) Query User\{[0-9A-Fa-f]{8}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{12}\}(.+)\z',
        [System.Text.RegularExpressions.RegexOptions]::Singleline)
    if (-not $match.Success) {
        return $false
    }
    $program = $null
    if (-not $Programs.TryGetValue([string]$Rule.InstanceID, [ref]$program) -or $null -eq $program) {
        return $false
    }
    if (-not [string]::Equals($match.Groups[1].Value, $program, [StringComparison]::OrdinalIgnoreCase)) {
        return $false
    }
    if (-not (Test-FullLongPath $program)) {
        return $false
    }
    return -not $program.StartsWith($InstallDirectory, [StringComparison]::OrdinalIgnoreCase)
}

# The Program of each rule's application filter, by the rule's InstanceID, read
# in one call: asking for one rule's filter takes as long as asking for all.
# An InstanceID with more than one filter maps to $null, so its rule stays.
function Get-RulePrograms {
    $programs = New-Object 'System.Collections.Generic.Dictionary[string,string]' ([StringComparer]::Ordinal)
    foreach ($filter in @(Get-NetFirewallApplicationFilter)) {
        $id = [string]$filter.InstanceID
        if ($programs.ContainsKey($id)) {
            $programs[$id] = $null
        } else {
            $programs[$id] = [string]$filter.Program
        }
    }
    return ,$programs
}

Write-Section 'Get-NetFirewallRule' {
    $installDirectory = Get-AlloyInstallDirectory
    $programs = Get-RulePrograms
    $rules = @(Get-NetFirewallRule | Where-Object { -not (Test-PromptRule $_ $programs $installDirectory) })
    Format-Objects $rules @(
        'Name', 'DisplayName', 'Group', 'Enabled', 'Profile', 'Direction', 'Action',
        'EdgeTraversalPolicy', 'Owner', 'PolicyStoreSourceType')
}

Write-Section 'Get-Service' {
    Format-Objects @(Get-Service) @('Name', 'DisplayName', 'StartType', 'ServiceType')
}

Write-Section 'machine PATH' {
    'PATH={0}' -f [Environment]::GetEnvironmentVariable('Path', 'Machine')
}

Write-Section 'Get-DnsClientServerAddress' {
    Format-Objects @(Get-DnsClientServerAddress) @(
        'InterfaceIndex', 'InterfaceAlias', 'AddressFamily', 'ServerAddresses')
}

Write-Section 'Get-DnsClientNrptRule' {
    Format-Objects @(Get-DnsClientNrptRule) @(
        'Name', 'Namespace', 'NameServers', 'NameEncoding', 'DnsSecEnabled',
        'DnsSecValidationRequired', 'DirectAccessEnabled', 'IPsecEncryptionType',
        'DisplayName', 'Comment')
}

'### end'

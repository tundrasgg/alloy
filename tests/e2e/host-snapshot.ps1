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

Write-Section 'Get-NetFirewallRule' {
    Format-Objects @(Get-NetFirewallRule) @(
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

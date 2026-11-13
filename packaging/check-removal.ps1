# Checks the removal functions of the shared block of an install or uninstall
# script against an install directory it makes under the temporary directory,
# so it needs no elevation and changes nothing else. packaging/check.sh runs it
# with every PowerShell it finds, on install.ps1 and on uninstall.ps1. It runs
# on Windows PowerShell 5.1 and on PowerShell 7, on any operating system.
#
#   pwsh -NoProfile -File packaging/check-removal.ps1 <install.ps1 or uninstall.ps1>
#
# It covers review findings F1 and F2 of T-0092: a directory the install
# created that holds another file is left in place and does not stop the
# uninstall, an install directory that holds another file is never moved, and
# a manifest that an uninstall interrupted between its two moves left under
# the .alloy-old name is found and moved back.

$ErrorActionPreference = 'Stop'
$target = $args[0]
$name = Split-Path -Leaf $target
$edition = "PowerShell $($PSVersionTable.PSVersion)"
$text = [IO.File]::ReadAllText((Resolve-Path -LiteralPath $target).Path)
$start = $text.IndexOf('# BEGIN SHARED')
$end = $text.IndexOf('# END SHARED')
. ([ScriptBlock]::Create($text.Substring($start, $end - $start)))

$base = Join-Path ([IO.Path]::GetTempPath()) ('alloy-check-removal.' + [Guid]::NewGuid().ToString('N').Substring(0, 12))
$root = Join-Path $base 'Alloy'
$bin = Join-Path $base 'bin'
$failed = $false

function Test-Case([string]$Text, [bool]$Ok) {
    if ($Ok) {
        Write-Output "ok    $name with $($edition): $Text"
    } else {
        Write-Output "FAIL  $name with $($edition): $Text"
        $script:failed = $true
    }
}

function Reset-AlloyState {
    $script:AlloyState = @{
        Paths = @{ Root = $root; Manifest = (Join-Path $root 'manifest'); DataDir = (Join-Path $base 'Data'); Service = 'alloyd' }
        Tmp = $null
        Displaced = $false
        Purge = $false
        Force = $false
        Yes = $false
    }
}

# An install that created $root and $bin and placed one file, with the other
# files given.
function New-Install([string[]]$Other) {
    Reset-AlloyState
    foreach ($p in @($root, "$root.alloy-old", "$root.alloy-new", $bin)) {
        if (Test-Path -LiteralPath $p) { Remove-Item -LiteralPath $p -Recurse -Force }
    }
    [void](New-Item -ItemType Directory -Path $root)
    [void](New-Item -ItemType Directory -Path $bin)
    $f = Join-Path $root 'alloy.exe'
    Write-AlloyText $f 'stand-in'
    $sha = (Get-FileHash -Algorithm SHA256 -LiteralPath $f).Hash.ToLowerInvariant()
    Write-AlloyText $AlloyState.Paths.Manifest "alloy-manifest 1`nversion 0.0.1-preview.1`nkind client`nstate complete`ndir $root`ndir $bin`nfile $sha $f`n"
    foreach ($o in $Other) { Write-AlloyText $o 'another product' }
}

[void](New-Item -ItemType Directory -Path $base)
try {
    # F1: a directory the install created holds another file.
    $other = Join-Path $bin 'other.exe'
    New-Install @($other)
    Read-AlloyManifest
    $r = Remove-AlloyEntry (ConvertTo-AlloyEntry "dir $bin") 6>$null
    Test-Case "a created directory that holds another file is left, not failed ($r)" ($r -ceq 'left' -and (Test-Path -LiteralPath $other))
    Remove-Item -LiteralPath $other
    $r = Remove-AlloyEntry (ConvertTo-AlloyEntry "dir $bin") 6>$null
    Test-Case "an empty created directory is removed ($r)" ($r -ceq 'removed' -and -not (Test-Path -LiteralPath $bin))

    # F2: the install directory holds another file, so it is never moved.
    $other = Join-Path $root 'other.txt'
    New-Install @($other)
    Read-AlloyManifest
    Remove-Item -LiteralPath (Join-Path $root 'alloy.exe')
    Remove-AlloyManifest 6>$null
    Test-Case 'an install directory that holds another file keeps it in place, loses the manifest, and is not moved' (
        (Test-Path -LiteralPath $other) -and -not (Test-Path -LiteralPath $AlloyState.Paths.Manifest) -and -not (Test-Path -LiteralPath "$root.alloy-old"))
    Reset-AlloyState
    Read-AlloyManifest
    Test-Case 'after that, a second uninstall finds no manifest' ($null -eq $AlloyState.Manifest -and -not $AlloyState.Displaced)

    # An install directory that holds only the manifest goes in one rename.
    New-Install @()
    Read-AlloyManifest
    Remove-Item -LiteralPath (Join-Path $root 'alloy.exe')
    Remove-AlloyManifest 6>$null
    Test-Case 'an install directory that holds only the manifest is removed' (-not (Test-Path -LiteralPath $root) -and -not (Test-Path -LiteralPath "$root.alloy-old"))

    # An uninstall interrupted between its two moves, after another file
    # appeared in the directory.
    New-Install @($other)
    [IO.Directory]::Move($root, "$root.alloy-old")
    Reset-AlloyState
    Read-AlloyManifest
    Test-Case "a manifest left under the .alloy-old name with another file is read, as pending ($($AlloyState.Manifest.State))" (
        $AlloyState.Displaced -and $AlloyState.Manifest.State -ceq 'pending' -and (Get-AlloyMarkers).Count -eq 0)
    Restore-AlloyDisplaced 6>$null
    Test-Case 'and moved back to its path' (
        (Test-Path -LiteralPath $other) -and (Test-Path -LiteralPath $AlloyState.Paths.Manifest) -and -not (Test-Path -LiteralPath "$root.alloy-old"))

    # A .alloy-old that holds only the manifest is a marker.
    Remove-Item -LiteralPath $root -Recurse -Force
    [void](New-Item -ItemType Directory -Path "$root.alloy-old")
    Write-AlloyText (Join-Path "$root.alloy-old" 'manifest') "alloy-manifest 1`n"
    Reset-AlloyState
    Read-AlloyManifest
    Test-Case 'a .alloy-old that holds only the manifest is a marker' (-not $AlloyState.Displaced -and (Get-AlloyMarkers).Count -eq 1)

    # A manifest under the .alloy-old name that does not list the install
    # directory was not left there by an uninstall.
    Write-AlloyText (Join-Path "$root.alloy-old" 'other.txt') 'another product'
    Write-AlloyText (Join-Path "$root.alloy-old" 'manifest') "alloy-manifest 1`nversion 0.0.1-preview.1`nkind client`nstate complete`n"
    Reset-AlloyState
    $code = 0
    try { Read-AlloyManifest } catch { $code = $_.Exception.Data['AlloyExitCode'] }
    Test-Case "a manifest under the .alloy-old name that does not list the directory ends with exit 1 ($code)" ($code -eq 1)
} finally {
    Remove-Item -LiteralPath $base -Recurse -Force -ErrorAction SilentlyContinue
}

if ($failed) { exit 1 }
exit 0

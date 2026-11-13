# Parses each PowerShell file given, and, for each install or uninstall
# script, checks that Invoke-Expression given a prefix of it runs nothing
# (ARFC 0017 clause 3.15). packaging/check.sh runs it with every PowerShell it
# finds. It runs on Windows PowerShell 5.1 and on PowerShell 7.
#
#   pwsh -NoProfile -File packaging/check.ps1 <file>...
#
# A prefix runs nothing when it does not parse, since Invoke-Expression parses
# the whole string before it runs any of it, or when every top-level statement
# in it defines a function or sets a variable from an expression that calls no
# command and no method. The prefixes checked end at every line boundary and at
# every character of the first two lines and of the last line.

$failed = $false
$edition = "PowerShell $($PSVersionTable.PSVersion)"

function Test-AlloyInert($Ast) {
    foreach ($s in $Ast.EndBlock.Statements) {
        if ($s -is [System.Management.Automation.Language.FunctionDefinitionAst]) { continue }
        $calls = $s.FindAll({
                param($n)
                $n -is [System.Management.Automation.Language.CommandAst] -or
                $n -is [System.Management.Automation.Language.InvokeMemberExpressionAst] -or
                $n -is [System.Management.Automation.Language.ScriptBlockExpressionAst]
            }, $true)
        if (@($calls).Count -gt 0) { return $false }
        if ($s -isnot [System.Management.Automation.Language.AssignmentStatementAst] -and
            $s -isnot [System.Management.Automation.Language.PipelineAst]) { return $false }
    }
    if ($Ast.BeginBlock -or $Ast.ProcessBlock -or $Ast.ParamBlock) { return $false }
    return $true
}

foreach ($f in $args) {
    $path = (Resolve-Path -LiteralPath $f).Path
    $tokens = $null
    $errors = $null
    [void][System.Management.Automation.Language.Parser]::ParseFile($path, [ref]$tokens, [ref]$errors)
    if ($errors.Count -gt 0) {
        foreach ($e in $errors) { Write-Output "FAIL  $f parses with $($edition): line $($e.Extent.StartLineNumber): $($e.Message)" }
        $failed = $true
        continue
    }
    Write-Output "ok    $f parses with $edition"
    if ((Split-Path -Leaf $f) -notmatch '^(un)?install\.ps1$') { continue }

    $text = [IO.File]::ReadAllText($path)
    $full = $text.TrimEnd("`n")
    $cuts = New-Object System.Collections.Generic.SortedSet[int]
    for ($i = 0; $i -lt $text.Length; $i++) { if ($text[$i] -eq "`n") { [void]$cuts.Add($i + 1) } }
    [void]$cuts.Add(0)
    $second = $text.IndexOf("`n", $text.IndexOf("`n") + 1)
    for ($i = 0; $i -le $second; $i++) { [void]$cuts.Add($i) }
    $lastStart = $full.LastIndexOf("`n") + 1
    for ($i = $lastStart; $i -lt $full.Length; $i++) { [void]$cuts.Add($i) }
    $bad = @()
    foreach ($c in $cuts) {
        if ($c -ge $full.Length) { continue }
        $prefix = $text.Substring(0, $c)
        $t = $null
        $e = $null
        $ast = [System.Management.Automation.Language.Parser]::ParseInput($prefix, [ref]$t, [ref]$e)
        if ($e.Count -gt 0) { continue }
        if (-not (Test-AlloyInert $ast)) { $bad += $c }
    }
    if ($bad.Count -gt 0) {
        Write-Output "FAIL  $f with $($edition): the prefixes ending at characters $($bad -join ', ') would run a command"
        $failed = $true
    } else {
        Write-Output "ok    $f with $($edition): none of its $($cuts.Count) prefixes runs a command"
    }
    $wt = $null
    $we = $null
    $whole = [System.Management.Automation.Language.Parser]::ParseInput($text, [ref]$wt, [ref]$we)
    if (Test-AlloyInert $whole) {
        Write-Output "FAIL  $f with $($edition): the whole script runs nothing, so the prefix check proves nothing"
        $failed = $true
    }
}

if ($failed) { exit 1 }
exit 0

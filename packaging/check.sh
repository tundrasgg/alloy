#!/bin/sh
# Checks the release scripts in packaging/. It needs only a POSIX sh and the
# usual text tools, and it also runs shellcheck and the PowerShell parser where
# they are installed. Every check prints one line: ok, FAIL, or skip. The exit
# code is 1 when any check fails.
#
#   sh packaging/check.sh
#
# It changes nothing outside a temporary directory: the scripts it runs end at
# their template refusal or at a usage error, before their first step, and the
# PowerShell run through Invoke-Expression ends at its platform or privilege
# check, or at a release with no archive for the platform.

set -u
here=$(cd "$(dirname "$0")" && pwd)
SH=$(command -v sh)
work=$(mktemp -d "${TMPDIR:-/tmp}/alloy-check.XXXXXX") || exit 1
trap 'rm -rf "$work"' EXIT
failed=0

ok() { printf 'ok    %s\n' "$*"; }
bad() {
  printf 'FAIL  %s\n' "$*"
  failed=1
}
skip() { printf 'skip  %s\n' "$*"; }

cd "$here" || exit 1
sh_scripts='install.sh uninstall.sh check.sh collect-licenses.sh dry-run.sh make-archive.sh set-version.sh stamp.sh sync-shared.sh'
ps_scripts='install.ps1 uninstall.ps1 check.ps1 check-removal.ps1'
placeholders='VERSION SHA256_LINUX_X86_64 SHA256_LINUX_AARCH64 SHA256_MACOS_X86_64 SHA256_MACOS_AARCH64 SHA256_WINDOWS_X86_64 SHA256_WINDOWS_AARCH64'

# ASCII text with LF line ends, so that every shell and both PowerShell
# editions read the scripts the same.
for f in $sh_scripts $ps_scripts alloyd.service README.md; do
  n=$(LC_ALL=C tr -d '\011\012\040-\176' <"$f" | wc -c | tr -d ' ')
  if [ "$n" = 0 ]; then
    ok "$f is ASCII text with LF line ends"
  else
    bad "$f has $n bytes that are not ASCII text, or carriage returns"
  fi
done

# Syntax.
for f in $sh_scripts; do
  if "$SH" -n "$f" 2>"$work/err"; then
    ok "sh -n $f"
  else
    bad "sh -n $f: $(cat "$work/err")"
  fi
done

if command -v shellcheck >/dev/null 2>&1; then
  for f in $sh_scripts; do
    if shellcheck -s sh "$f" >"$work/sc" 2>&1; then
      ok "shellcheck $f ($(shellcheck --version | sed -n 's/^version: //p'))"
    else
      bad "shellcheck $f"
      cat "$work/sc"
    fi
  done
else
  skip "shellcheck is not installed"
fi

# The shared blocks of each install script and its uninstall script.
shared() { sed -n '/^# BEGIN SHARED/,/^# END SHARED/p' "$1"; }
for pair in 'install.sh uninstall.sh' 'install.ps1 uninstall.ps1'; do
  a=${pair% *}
  b=${pair#* }
  shared "$a" >"$work/a"
  shared "$b" >"$work/b"
  if [ -s "$work/a" ] && cmp -s "$work/a" "$work/b"; then
    ok "the shared blocks of $a and $b are identical ($(wc -l <"$work/a" | tr -d ' ') lines)"
  else
    bad "the shared blocks of $a and $b differ: edit $a, then run sh packaging/sync-shared.sh"
  fi
done

# The unit that install.sh writes is alloyd.service.
sed -n '/^alloy_unit() {$/,/^}$/p' install.sh | sed '1,2d' | sed '$d' | sed '$d' >"$work/unit"
if cmp -s "$work/unit" alloyd.service; then
  ok "the unit in install.sh is alloyd.service"
else
  bad "the unit in install.sh differs from alloyd.service"
fi

# Placeholders: one of each in the install scripts, none in the uninstall
# scripts, which go inside the archives whose digests the install scripts hold.
for f in install.sh install.ps1; do
  missing=''
  for p in $placeholders; do
    [ "$(grep -c "@ALLOY_$p@" "$f")" = 1 ] || missing="$missing $p"
  done
  if [ -z "$missing" ]; then
    ok "$f holds each placeholder once"
  else
    bad "$f does not hold each of these placeholders once:$missing"
  fi
done
for f in uninstall.sh uninstall.ps1; do
  if grep -q '@ALLOY_' "$f"; then
    bad "$f holds a placeholder"
  else
    ok "$f holds no placeholder"
  fi
done

# expect <name> <exit code> <text the output must hold> <command>...
expect() {
  _name=$1
  _code=$2
  _text=$3
  shift 3
  "$@" </dev/null >"$work/out" 2>&1
  _rc=$?
  if [ "$_rc" = "$_code" ] && grep -q -- "$_text" "$work/out"; then
    ok "$_name: exit $_rc, says \"$_text\""
  else
    bad "$_name: exit $_rc, expected $_code and \"$_text\":"
    sed 's/^/      /' "$work/out"
  fi
}

# A template refuses to run, whatever its arguments.
expect 'install.sh, a template, refuses' 1 'template' "$SH" install.sh
expect 'install.sh --uninstall, a template, refuses' 1 'template' "$SH" install.sh --uninstall

# Usage errors, on a copy with a version and no archive digests. These end
# before the first step.
sed 's/@ALLOY_VERSION@/0.0.0-preview.1/; s/@ALLOY_SHA256_[A-Z0-9_]*@//' install.sh >"$work/install.sh"
sed 's/@ALLOY_VERSION@/0.0.0-preview.1/; s/@ALLOY_SHA256_[A-Z0-9_]*@//' install.ps1 >"$work/install.ps1"
expect 'install.sh --bogus is a usage error' 2 'Unknown argument --bogus' "$SH" "$work/install.sh" --bogus
expect 'install.sh --purge without --uninstall is a usage error' 2 'go with --uninstall' "$SH" "$work/install.sh" --purge
expect 'install.sh --version without a version is a usage error' 2 'needs a version' "$SH" "$work/install.sh" --version
expect 'install.sh --version 1.2 is a usage error' 2 'is not a version' "$SH" "$work/install.sh" --version 1.2
expect 'uninstall.sh --bogus is a usage error' 2 'Unknown argument --bogus' "$SH" uninstall.sh --bogus

# Cut at any point, a script runs nothing (clause 3.15). Each prefix ending at
# a line boundary or inside the last line is fed to sh on standard input, as
# curl | sh does, with uninstall arguments and an empty PATH. A prefix that ran
# any part of the script would print a line naming Alloy, a Try: line, or a
# usage line; the whole script, run the same way, must, or the check proves
# nothing.
mkdir "$work/empty"
prefix_check() {
  _f=$1
  shift
  _lines=$(wc -l <"$_f" | tr -d ' ')
  _size=$(wc -c <"$_f" | tr -d ' ')
  _last=$(tail -n 1 "$_f" | wc -c | tr -d ' ')
  _bad=''
  _count=0
  _i=0
  while [ "$_i" -lt "$_lines" ]; do
    head -n "$_i" "$_f" >"$work/prefix"
    PATH="$work/empty" "$SH" -s -- "$@" <"$work/prefix" >"$work/out" 2>&1
    grep -qi 'alloy\|try:\|usage' "$work/out" && _bad="$_bad line$_i"
    _count=$((_count + 1))
    _i=$((_i + 1))
  done
  _b=$((_size - _last))
  while [ "$_b" -lt $((_size - 1)) ]; do
    head -c "$_b" "$_f" >"$work/prefix"
    PATH="$work/empty" "$SH" -s -- "$@" <"$work/prefix" >"$work/out" 2>&1
    grep -qi 'alloy\|try:\|usage' "$work/out" && _bad="$_bad byte$_b"
    _count=$((_count + 1))
    _b=$((_b + 1))
  done
  PATH="$work/empty" "$SH" -s -- "$@" <"$_f" >"$work/out" 2>&1
  if ! grep -qi 'alloy\|try:\|usage' "$work/out"; then
    bad "$_f, run whole with an empty PATH, printed nothing a prefix may not print, so its prefix check proves nothing"
  elif [ -n "$_bad" ]; then
    bad "$_f: these prefixes ran part of the script:$_bad"
  else
    ok "$_f: none of its $_count prefixes runs anything"
  fi
}
prefix_check install.sh --uninstall --purge --yes
prefix_check uninstall.sh --purge --yes

# PowerShell: the parser, the prefix check of check.ps1, the removal checks of
# check-removal.ps1, and how the install script ends when it runs as a file
# and through Invoke-Expression.
native_path() {
  if command -v cygpath >/dev/null 2>&1; then
    cygpath -w "$1"
  else
    printf '%s\n' "$1"
  fi
}
found_ps=0
for ps in pwsh powershell; do
  command -v "$ps" >/dev/null 2>&1 || continue
  found_ps=1
  # shellcheck disable=SC2086
  if "$ps" -NoProfile -NonInteractive -ExecutionPolicy Bypass -File check.ps1 $ps_scripts >"$work/ps" 2>&1; then
    cat "$work/ps"
  else
    cat "$work/ps"
    bad "check.ps1 with $ps"
  fi
  for target in install.ps1 uninstall.ps1; do
    if "$ps" -NoProfile -NonInteractive -ExecutionPolicy Bypass -File check-removal.ps1 "$target" >"$work/ps" 2>&1; then
      cat "$work/ps"
    else
      cat "$work/ps"
      bad "check-removal.ps1 $target with $ps"
    fi
  done
  expect "install.ps1, a template, refuses ($ps)" 1 'template' \
    "$ps" -NoProfile -NonInteractive -ExecutionPolicy Bypass -File install.ps1
  expect "install.ps1 -Bogus is a usage error ($ps)" 2 'Unknown argument -Bogus' \
    "$ps" -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "$work/install.ps1" -Bogus
  expect "install.ps1 -Purge without -Uninstall is a usage error ($ps)" 2 'go with -Uninstall' \
    "$ps" -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "$work/install.ps1" -Purge
  expect "uninstall.ps1 -Bogus is a usage error ($ps)" 2 'Unknown argument -Bogus' \
    "$ps" -NoProfile -NonInteractive -ExecutionPolicy Bypass -File uninstall.ps1 -Bogus
  # Through Invoke-Expression a failure is a terminating error, and the
  # session goes on. The copy has no archive digest, so even an elevated run
  # stops at step 5 without downloading.
  mkdir -p "$work/releases"
  ALLOY_INSTALL_TEST_RELEASES=$(native_path "$work/releases")
  export ALLOY_INSTALL_TEST_RELEASES
  expect "install.ps1 through Invoke-Expression ends with a terminating error, not exit ($ps)" 0 'session still open' \
    "$ps" -NoProfile -NonInteractive -Command "try { [IO.File]::ReadAllText('$(native_path "$work/install.ps1")') | Invoke-Expression; 'no error' } catch { 'terminating error: ' + \$_.Exception.Message }; 'session still open'"
  unset ALLOY_INSTALL_TEST_RELEASES
  grep '^terminating error' "$work/out" | sed 's/^/      /'
done
[ "$found_ps" = 1 ] || skip "PowerShell is not installed: the .ps1 files were not parsed"

if [ "$failed" = 0 ]; then
  echo "check.sh: every check passed"
else
  echo "check.sh: some checks failed"
fi
exit "$failed"

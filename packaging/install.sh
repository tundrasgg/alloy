#!/bin/sh
# Alloy install script for Linux and macOS (ARFC 0017 clauses 3.13 to 3.17).
#
#   curl -fsSL https://github.com/tundrasgg/alloy/releases/download/v<version>/install.sh | sh
#
# Options: --version <version>, --uninstall, --purge, --force, --yes.
#
# This file in the source tree is a template. The release workflow writes the
# release's version and the SHA-256 of each of its archives into the
# placeholders below, and a script whose placeholders are not filled in refuses
# to run.
#
# Every top-level command before the last line only defines a function or sets
# a variable. The last line is the one command that starts the work, inside a
# brace group, so a shell that gets only part of this file runs nothing
# (clause 3.15).
#
# Test only: when ALLOY_INSTALL_TEST_RELEASES names a directory, the script
# reads each release asset from <directory>/v<version>/<asset> instead of
# downloading it from GitHub. The digests written into the script are checked
# all the same. It exists for the dry runs of packaging/dry-run.sh and must
# never be set on a machine that is not a disposable test machine.

ALLOY_VERSION='@ALLOY_VERSION@'
ALLOY_SHA256_LINUX_X86_64='@ALLOY_SHA256_LINUX_X86_64@'
ALLOY_SHA256_LINUX_AARCH64='@ALLOY_SHA256_LINUX_AARCH64@'
ALLOY_SHA256_MACOS_X86_64='@ALLOY_SHA256_MACOS_X86_64@'
ALLOY_SHA256_MACOS_AARCH64='@ALLOY_SHA256_MACOS_AARCH64@'
ALLOY_SHA256_WINDOWS_X86_64='@ALLOY_SHA256_WINDOWS_X86_64@'
ALLOY_SHA256_WINDOWS_AARCH64='@ALLOY_SHA256_WINDOWS_AARCH64@'

# BEGIN SHARED: the text from here to END SHARED is identical in install.sh and
# uninstall.sh. packaging/check.sh compares the two.

ALLOY_BIN_DIR='/usr/local/bin'
ALLOY_SHARE_DIR='/usr/local/share/alloy'
ALLOY_MANIFEST='/usr/local/share/alloy/manifest'
ALLOY_UNIT_DIR='/etc/systemd/system'
ALLOY_UNIT_NAME='alloyd.service'
ALLOY_DATA_DIR='/var/lib/alloyd'
ALLOY_GUEST_ID_MIN=1900000000
ALLOY_GUEST_ID_MAX=1900065535
# The directories that hold the manifest, outermost first. A first install
# creates the missing ones together with the manifest (see alloy_write_manifest).
ALLOY_MANIFEST_CHAIN='/usr/local /usr/local/share /usr/local/share/alloy'
ALLOY_TMP=''
ALLOY_SUDO=''
ALLOY_OS=''
ALLOY_PURGE=0
ALLOY_FORCE=0
ALLOY_YES=0
ALLOY_HAVE_MANIFEST=0
ALLOY_DISPLACED=''
ALLOY_M_VERSION=''
ALLOY_M_KIND=''
ALLOY_M_STATE=''
ALLOY_DD=absent
ALLOY_HOST_JOIN=unjoined
ALLOY_HOST_RELEASE=none
ALLOY_BOOT_ID=''

alloy_say() {
  printf '%s\n' "$*" >&2
}

# A step line: two spaces, a status word padded to six columns, the text.
alloy_step() {
  printf '  %-6s %s\n' "$1" "$2" >&2
}

alloy_cleanup() {
  if [ -n "$ALLOY_TMP" ] && [ -d "$ALLOY_TMP" ]; then
    rm -rf "$ALLOY_TMP"
  fi
  ALLOY_TMP=''
}

# alloy_fail <exit code> <message> [<command for the Try: line>]
alloy_fail() {
  printf '%s\n' "$2" >&2
  if [ -n "${3:-}" ]; then
    printf 'Try: %s\n' "$3" >&2
  fi
  alloy_cleanup
  exit "$1"
}

alloy_traps() {
  trap 'alloy_cleanup' EXIT
  trap 'alloy_cleanup; exit 130' INT
  trap 'alloy_cleanup; exit 143' TERM
}

# Runs a command as root: directly when the script runs as root, else
# through sudo (clause 3.15 step 2).
alloy_root() {
  if [ -n "$ALLOY_SUDO" ]; then
    sudo -- "$@"
  else
    "$@"
  fi
}

alloy_make_tmp() {
  ALLOY_TMP=$(mktemp -d "${TMPDIR:-/tmp}/alloy.XXXXXX") || ALLOY_TMP=''
  if [ -z "$ALLOY_TMP" ] || [ ! -d "$ALLOY_TMP" ]; then
    ALLOY_TMP=''
    alloy_fail 1 "Could not create a temporary directory under ${TMPDIR:-/tmp}."
  fi
}

alloy_detect_os() {
  if ! command -v uname >/dev/null 2>&1; then
    alloy_fail 1 "This script needs uname, which is not installed."
  fi
  case $(uname -s) in
    Linux) ALLOY_OS=linux ;;
    Darwin) ALLOY_OS=macos ;;
    *) alloy_fail 1 "Alloy runs on Linux, macOS, and Windows, and this machine runs $(uname -s)." ;;
  esac
}

alloy_digits() {
  case $1 in
    '' | *[!0-9]*) return 1 ;;
  esac
  return 0
}

# Prints "<mode> <owner uid>" of a path without following a final symbolic link.
# ls is the one POSIX tool that reports both; only its first and third fields
# are read, so the name in the line does not matter.
alloy_mode_owner() {
  # shellcheck disable=SC2012
  LC_ALL=C ls -ldn -- "$1" 2>/dev/null | awk 'NR == 1 { print $1, $3 }'
}

# The same, run as root, for paths only root may look into.
alloy_root_mode_owner() {
  alloy_root env LC_ALL=C ls -ldn -- "$1" 2>/dev/null | awk 'NR == 1 { print $1, $3 }'
}

# True when the path is owned by root and neither its group nor others may
# write it. Follows symbolic links.
alloy_root_only_writable() {
  # shellcheck disable=SC2012
  _row=$(LC_ALL=C ls -ldnL -- "$1" 2>/dev/null | awk 'NR == 1 { print $1, $3 }')
  case $_row in
    ?????w*' '* | ????????w*' '*) return 1 ;;
    *' 0') return 0 ;;
  esac
  return 1
}

alloy_exists() {
  [ -e "$1" ] || [ -L "$1" ]
}

# --- Manifest (section 4.1) ---------------------------------------------------

# Validates a manifest file. Prints nothing; returns 1 when it is not valid.
alloy_manifest_valid() {
  if [ -s "$1" ] && [ -n "$(tail -c 1 "$1")" ]; then
    return 1
  fi
  awk '
    function bad() { ok = 0; exit }
    NR == 1 { if ($0 != "alloy-manifest 1") bad(); next }
    NR == 2 { if ($0 !~ /^version [0-9A-Za-z.+-]+$/) bad(); next }
    NR == 3 { if ($0 !~ /^kind (node|client|cli)$/) bad(); next }
    NR == 4 { if ($0 !~ /^state (pending|complete)$/) bad(); next }
    /^dir \/./ { next }
    /^file [0-9a-f]+ \/./ { if (length($2) != 64) bad(); next }
    /^unit [A-Za-z0-9@._-]+$/ { next }
    /^service ./ { next }
    /^path ./ { next }
    { bad() }
    BEGIN { ok = 1 }
    END { if (NR < 4) ok = 0; exit !ok }
  ' "$1"
}

# An uninstall moves a directory of the manifest chain to its .alloy-old name
# only when it holds nothing but the chain and the manifest, and moves it back
# when something else appeared in it in the meantime. Interrupted between
# those two moves, it leaves the manifest in a directory under its .alloy-old
# name that is not a marker. Prints that directory of the chain, if any.
alloy_displaced() {
  for _dc in $ALLOY_MANIFEST_CHAIN; do
    _o="$_dc.alloy-old"
    ! alloy_exists "$_dc" || continue
    if [ ! -d "$_o" ] || [ -L "$_o" ] || alloy_is_marker "$_o"; then
      continue
    fi
    _dm="$_o${ALLOY_MANIFEST#"$_dc"}"
    if [ ! -f "$_dm" ] || [ -L "$_dm" ]; then
      continue
    fi
    printf '%s\n' "$_dc"
    return 0
  done
  return 0
}

# Moves the directory that alloy_displaced found back to its path, so that the
# manifest is where it belongs again.
alloy_restore_displaced() {
  [ -n "$ALLOY_DISPLACED" ] || return 0
  if alloy_exists "$ALLOY_DISPLACED" || ! alloy_root mv -- "$ALLOY_DISPLACED.alloy-old" "$ALLOY_DISPLACED"; then
    alloy_fail 1 "Could not move $ALLOY_DISPLACED.alloy-old, left by an interrupted uninstall, back to $ALLOY_DISPLACED." "run this again"
  fi
  alloy_step 'done' "moved $ALLOY_DISPLACED.alloy-old, left by an interrupted uninstall, back to $ALLOY_DISPLACED"
  ALLOY_DISPLACED=''
}

# Reads the manifest into $ALLOY_TMP/manifest.old, from where an interrupted
# uninstall left it when it is not at its path (alloy_displaced). Exits 1 when
# the manifest cannot be read, parsed, or trusted.
alloy_read_manifest() {
  ALLOY_HAVE_MANIFEST=0
  ALLOY_DISPLACED=''
  _rm=$ALLOY_MANIFEST
  _rd=$ALLOY_SHARE_DIR
  if ! alloy_exists "$ALLOY_MANIFEST"; then
    ALLOY_DISPLACED=$(alloy_displaced)
    [ -n "$ALLOY_DISPLACED" ] || return 0
    _rm="$ALLOY_DISPLACED.alloy-old${ALLOY_MANIFEST#"$ALLOY_DISPLACED"}"
    _rd="$ALLOY_DISPLACED.alloy-old${ALLOY_SHARE_DIR#"$ALLOY_DISPLACED"}"
  fi
  _mo=$(alloy_mode_owner "$_rm")
  case $_mo in
    -????-??-?' 0' | -????-??-?[.+]' 0') ;;
    *) alloy_fail 1 "The install manifest $_rm is not a regular file owned by root that only root can write, so it cannot be trusted." ;;
  esac
  if ! alloy_root_only_writable "$_rd"; then
    alloy_fail 1 "The directory $_rd is not owned by root or can be written by others, so the install manifest in it cannot be trusted."
  fi
  if ! cat "$_rm" >"$ALLOY_TMP/manifest.old" 2>/dev/null; then
    alloy_fail 1 "Could not read the install manifest $_rm."
  fi
  if ! alloy_manifest_valid "$ALLOY_TMP/manifest.old"; then
    alloy_fail 1 "The install manifest $_rm is damaged: its format is not one this script reads. Nothing was changed."
  fi
  if [ -n "$ALLOY_DISPLACED" ] && ! grep -qxF "dir $ALLOY_DISPLACED" "$ALLOY_TMP/manifest.old"; then
    alloy_fail 1 "The install manifest $_rm does not list $ALLOY_DISPLACED, so it was not left there by an uninstall and cannot be trusted. Nothing was changed."
  fi
  ALLOY_M_VERSION=$(sed -n '2s/^version //p' "$ALLOY_TMP/manifest.old")
  ALLOY_M_KIND=$(sed -n '3s/^kind //p' "$ALLOY_TMP/manifest.old")
  ALLOY_M_STATE=$(sed -n '4s/^state //p' "$ALLOY_TMP/manifest.old")
  if [ -n "$ALLOY_DISPLACED" ]; then
    # An uninstall was under way, so the install it records is not complete.
    ALLOY_M_STATE=pending
  fi
  ALLOY_HAVE_MANIFEST=1
}

# Prints the path of each entry of a manifest file (after its fourth line, or
# after the number of lines given as $2),
# one per line, in file order: "<keyword> <path or name>".
alloy_manifest_entries() {
  _skip=${2:-4}
  awk -v skip="$_skip" 'NR > skip {
    if ($1 == "file") { k = $1; sub(/^file [0-9a-f]+ /, ""); print k " " $0 }
    else print
  }' "$1"
}

# The manifest dir entries that hold the manifest, outermost first. These
# are removed with the manifest, after everything else.
alloy_manifest_holders() {
  for _h in $ALLOY_MANIFEST_CHAIN; do
    if grep -qxF "dir $_h" "$1"; then
      printf '%s\n' "$_h"
    fi
  done
}

# --- Interrupted first installs (T-0092, review finding F3) -----------------
#
# A first install creates the manifest's directory and the manifest in one
# rename: it builds them under a temporary name (<dir>.alloy-new) and renames
# that into place, and an uninstall removes them the same way through
# <dir>.alloy-old. Such a directory is a marker: it holds nothing but the
# directories that lead to the manifest and, at most, the manifest. Every
# install and uninstall recognizes and removes it. An uninstall moves a
# directory to its .alloy-old name only when it already holds nothing else, so
# that no interruption leaves another product's file away from its path.

# True when $1 (a temporary name of a directory of the manifest chain) is a
# marker. With $2, a directory of the chain, $1 is checked as if it were $2:
# alloy_is_marker <dir> <dir> is true when <dir> holds nothing but the
# directories that lead to the manifest and, at most, the manifest.
alloy_is_marker() {
  _mk=$1
  [ -d "$_mk" ] && [ ! -L "$_mk" ] || return 1
  case $(alloy_mode_owner "$_mk") in
    d*' 0') ;;
    *) return 1 ;;
  esac
  if [ -n "${2:-}" ]; then
    _base=$2
  else
    _base=${_mk%.alloy-new}
    _base=${_base%.alloy-old}
  fi
  # The allowed relative paths inside the marker.
  _allowed=''
  _seen=0
  for _mc in $ALLOY_MANIFEST_CHAIN; do
    if [ "$_mc" = "$_base" ]; then
      _seen=1
    fi
    if [ "$_seen" = 1 ]; then
      _allowed="$_allowed ${_mc#"$_base"}"
    fi
  done
  [ "$_seen" = 1 ] || return 1
  _mrel=${ALLOY_MANIFEST#"$_base"}
  _bad=$(cd "$_mk" && find . -print 2>/dev/null | while IFS= read -r _e; do
    _rel=${_e#.}
    if [ "$_rel" = "$_mrel" ]; then
      [ -f "$_e" ] && [ ! -L "$_e" ] && continue
      echo bad
      break
    fi
    _ok=0
    [ -n "$_rel" ] || _ok=1
    for _a in $_allowed; do
      if [ "$_rel" = "$_a" ]; then
        _ok=1
      fi
    done
    if [ "$_ok" = 1 ] && [ -d "$_e" ] && [ ! -L "$_e" ]; then
      continue
    fi
    echo bad
    break
  done)
  [ -z "$_bad" ]
}

# Prints each marker that exists.
alloy_markers() {
  for _c in $ALLOY_MANIFEST_CHAIN; do
    for _t in "$_c.alloy-new" "$_c.alloy-old"; do
      if alloy_exists "$_t" && alloy_is_marker "$_t"; then
        printf '%s\n' "$_t"
      fi
    done
  done
}

# --- Data directory and host file (ARFC 0002 section 3.12, section 4.2) -----

alloy_host_valid() {
  if [ -s "$1" ] && [ -n "$(tail -c 1 "$1")" ]; then
    return 1
  fi
  awk '
    function bad() { ok = 0; exit }
    function boot(s) { return s ~ /^[0-9a-f-]+$/ && length(s) == 36 }
    BEGIN { ok = 1 }
    NR == 1 { if ($0 != "alloy-host 1") bad(); next }
    NR == 2 { if ($0 !~ /^join (unjoined|joining|joined)$/) bad(); next }
    NR == 3 { if ($0 !~ /^release [0-9A-Za-z.+-]+$/) bad(); next }
    $1 == "link" || $1 == "netns" || $1 == "run" { if (NF < 3 || !boot($2)) bad(); next }
    $1 == "nft" { if (NF < 4 || !boot($2)) bad(); next }
    $1 == "adapter" { if (NF < 3 || $2 !~ /^\{[0-9a-f-]+\}$/) bad(); next }
    { bad() }
    END { if (NR < 3) ok = 0; exit !ok }
  ' "$1"
}

# Sets ALLOY_DD to absent, other, or alloy. For an Alloy data directory it
# also sets ALLOY_HOST_JOIN and ALLOY_HOST_RELEASE and copies the host file to
# $ALLOY_TMP/host. Exits 1 when the host file of a directory that meets the
# other conditions cannot be parsed (section 4.2).
alloy_check_data_dir() {
  ALLOY_DD=absent
  ALLOY_HOST_JOIN=unjoined
  ALLOY_HOST_RELEASE=none
  if [ "$ALLOY_OS" != linux ]; then
    return 0
  fi
  if ! alloy_exists "$ALLOY_DATA_DIR"; then
    return 0
  fi
  ALLOY_DD=other
  case $(alloy_mode_owner "$ALLOY_DATA_DIR") in
    drwx------*' 0') ;;
    *) return 0 ;;
  esac
  _p=$ALLOY_DATA_DIR
  while :; do
    _p=${_p%/*}
    [ -n "$_p" ] || _p=/
    alloy_root_only_writable "$_p" || return 0
    [ "$_p" != / ] || break
  done
  _hf="$ALLOY_DATA_DIR/alloy-host"
  case $(alloy_root_mode_owner "$_hf") in
    -rw-------*' 0') ;;
    *) return 0 ;;
  esac
  if ! alloy_root cat "$_hf" >"$ALLOY_TMP/host" 2>/dev/null; then
    alloy_fail 1 "Could not read the host file $_hf."
  fi
  if ! alloy_host_valid "$ALLOY_TMP/host"; then
    alloy_fail 1 "The host file $_hf cannot be read: its format is not one this script reads. Nothing was changed."
  fi
  ALLOY_DD=alloy
  ALLOY_HOST_JOIN=$(sed -n '2s/^join //p' "$ALLOY_TMP/host")
  ALLOY_HOST_RELEASE=$(sed -n '3s/^release //p' "$ALLOY_TMP/host")
}

alloy_boot_id() {
  if [ -z "$ALLOY_BOOT_ID" ] && [ -r /proc/sys/kernel/random/boot_id ]; then
    ALLOY_BOOT_ID=$(cat /proc/sys/kernel/random/boot_id)
  fi
  printf '%s\n' "$ALLOY_BOOT_ID"
}

# Prints, for each host file line that matches its object (section 4.2), the
# object's description as alloy_scan_reserved prints it, a "|", and the line.
# Lines that name an object outside the reserved names never match.
alloy_host_matches() {
  [ "$ALLOY_DD" = alloy ] || return 0
  _bid=$(alloy_boot_id)
  tail -n +4 "$ALLOY_TMP/host" | while IFS= read -r _l; do
    set -f
    # shellcheck disable=SC2086
    set -- $_l
    set +f
    case $1 in
      link)
        [ "$2" = "$_bid" ] || continue
        case $3 in al[0-9]* | alv[0-9]*) ;; *) continue ;; esac
        [ -e "/sys/class/net/$3" ] || continue
        printf 'network interface %s|%s\n' "$3" "$_l"
        ;;
      netns)
        [ "$2" = "$_bid" ] || continue
        case $3 in alloy-*) ;; *) continue ;; esac
        [ -e "/run/netns/$3" ] || continue
        printf 'network namespace %s|%s\n' "$3" "$_l"
        ;;
      nft)
        [ "$2" = "$_bid" ] || continue
        case $4 in alloy | alloy_*) ;; *) continue ;; esac
        command -v nft >/dev/null 2>&1 || continue
        alloy_root nft list table "$3" "$4" >/dev/null 2>&1 || continue
        printf 'nftables table %s %s|%s\n' "$3" "$4" "$_l"
        ;;
      run)
        [ "$2" = "$_bid" ] || continue
        _rp=${_l#run * }
        case $_rp in /run/alloy | /run/alloy/*) ;; *) continue ;; esac
        case "$_rp/" in */../* | */./*) continue ;; esac
        alloy_exists "$_rp" || continue
        printf 'directory %s|%s\n' "$_rp" "$_l"
        ;;
    esac
  done
}

# Prints one line for each object on the machine with a name that clause 3.17
# reserves, other than files and the data directory.
alloy_scan_reserved() {
  [ "$ALLOY_OS" = linux ] || return 0
  if [ -d /sys/class/net ]; then
    for _n in /sys/class/net/*; do
      _n=${_n##*/}
      case $_n in
        al[0-9]* | alv[0-9]*) printf 'network interface %s\n' "$_n" ;;
      esac
    done
  fi
  if [ -d /run/netns ]; then
    for _n in /run/netns/*; do
      _n=${_n##*/}
      case $_n in
        alloy-*) printf 'network namespace %s\n' "$_n" ;;
      esac
    done
  fi
  if command -v nft >/dev/null 2>&1; then
    alloy_root nft list tables 2>/dev/null | while read -r _t _fam _name; do
      case $_name in
        alloy | alloy_*) printf 'nftables table %s %s\n' "$_fam" "$_name" ;;
      esac
    done
  fi
  if command -v systemctl >/dev/null 2>&1; then
    {
      systemctl list-unit-files --no-legend --no-pager --full 'alloyd.service' 'alloyd-*' 2>/dev/null
      systemctl list-units --all --no-legend --no-pager --full --plain 'alloyd.service' 'alloyd-*' 2>/dev/null
    } | awk 'NF { print "systemd unit " $1 }' | sort -u
  fi
  if alloy_exists /run/alloy; then
    printf 'directory /run/alloy\n'
  fi
}

# --- Processes ------------------------------------------------------------------

# True when /proc/<pid>/status shows a user id in the guest id range.
alloy_is_vmm() {
  _vf=$1
  while IFS= read -r _vl; do
    case $_vl in
      Uid:*)
        set -f
        # shellcheck disable=SC2086
        set -- $_vl
        set +f
        shift
        for _u in "$@"; do
          if alloy_digits "$_u" && [ "$_u" -ge "$ALLOY_GUEST_ID_MIN" ] && [ "$_u" -le "$ALLOY_GUEST_ID_MAX" ]; then
            return 0
          fi
        done
        return 1
        ;;
    esac
  done 2>/dev/null <"$_vf"
  return 1
}

alloy_vmm_pids() {
  [ -d /proc ] || return 0
  for _s in /proc/[0-9]*/status; do
    if alloy_is_vmm "$_s"; then
      _pid=${_s#/proc/}
      printf '%s\n' "${_pid%/status}"
    fi
  done
}

# Prints the pid of every process run from one of the executables listed,
# one path per line, in the file $1.
alloy_exe_pids() {
  if [ -d /proc/self ]; then
    for _e in /proc/[0-9]*/exe; do
      _x=$(readlink "$_e" 2>/dev/null) || continue
      _x=${_x% (deleted)}
      if grep -qxF -- "$_x" "$1"; then
        _pid=${_e#/proc/}
        printf '%s\n' "${_pid%/exe}"
      fi
    done
  else
    ps -axo pid=,comm= 2>/dev/null | while read -r _pid _x; do
      if grep -qxF -- "$_x" "$1"; then
        printf '%s\n' "$_pid"
      fi
    done
  fi
}

# Sends each pid in the file $1 a termination request, waits up to 30 seconds
# for all of them to end, and kills those still running (clause 3.16 step 5).
alloy_stop_pids() {
  [ -s "$1" ] || return 0
  # shellcheck disable=SC2046
  kill -TERM $(cat "$1") 2>/dev/null
  _waited=0
  while [ "$_waited" -lt 30 ]; do
    _alive=0
    while read -r _pid; do
      if kill -0 "$_pid" 2>/dev/null; then
        _alive=1
      fi
    done <"$1"
    [ "$_alive" = 1 ] || break
    sleep 1
    _waited=$((_waited + 1))
  done
  while read -r _pid; do
    if kill -0 "$_pid" 2>/dev/null; then
      kill -KILL "$_pid" 2>/dev/null
      alloy_step 'done' "killed process $_pid after 30 s"
    fi
  done <"$1"
}

# --- Removal helpers -----------------------------------------------------------

# Removes the object of a host file line that alloy_host_matches matched.
alloy_remove_host_object() {
  _hl=$1
  set -f
  # shellcheck disable=SC2086
  set -- $_hl
  set +f
  case $1 in
    link) alloy_root ip link delete dev "$3" ;;
    netns) alloy_root ip netns delete "$3" ;;
    nft) alloy_root nft delete table "$3" "$4" ;;
    run) alloy_root rm -rf -- "${_hl#run * }" ;;
    *) return 1 ;;
  esac
}

# Removes a temporary name of a manifest path: a file, an empty directory, or
# a marker. Anything else under that name is left and reported.
alloy_remove_temp() {
  alloy_exists "$1" || return 0
  if [ -d "$1" ] && [ ! -L "$1" ]; then
    if alloy_is_marker "$1"; then
      alloy_root rm -rf -- "$1"
    else
      alloy_root rmdir -- "$1" 2>/dev/null
    fi
  else
    alloy_root rm -f -- "$1"
  fi
  if alloy_exists "$1"; then
    alloy_step warn "left $1 in place: it is not empty"
    return 1
  fi
  return 0
}

alloy_remove_temps() {
  alloy_remove_temp "$1.alloy-new"
  _rt=$?
  alloy_remove_temp "$1.alloy-old" && [ "$_rt" = 0 ]
}

alloy_daemon_reload() {
  if command -v systemctl >/dev/null 2>&1 && [ -d /run/systemd/system ]; then
    alloy_root systemctl daemon-reload >/dev/null 2>&1
  fi
}

# Removes one manifest entry with its temporary names. Returns 1 when the entry
# could not be removed, and 2 for a directory left in place because it holds
# files the install did not place (clause 3.16 step 7 removes directories that
# are empty).
alloy_remove_entry() {
  _k=$1
  _p=$2
  case $_k in
    file)
      if [ -d "$_p" ] && [ ! -L "$_p" ]; then
        alloy_step warn "left $_p in place: the manifest lists a file there, and it is a directory"
        return 1
      fi
      if alloy_exists "$_p"; then
        alloy_root rm -f -- "$_p" || return 1
        alloy_step 'done' "removed $_p"
      fi
      alloy_remove_temps "$_p"
      ;;
    dir)
      if [ -d "$_p" ] && [ ! -L "$_p" ]; then
        if alloy_root rmdir -- "$_p" 2>/dev/null; then
          alloy_step 'done' "removed $_p"
        elif [ -n "$(alloy_root ls -A -- "$_p" 2>/dev/null | head -n 1)" ]; then
          alloy_remove_temps "$_p"
          alloy_step warn "left $_p in place: it holds files the install did not place"
          return 2
        else
          return 1
        fi
      fi
      alloy_remove_temps "$_p"
      ;;
    unit)
      if command -v systemctl >/dev/null 2>&1; then
        alloy_root systemctl disable --now "$_p" >/dev/null 2>&1
      fi
      if alloy_exists "$ALLOY_UNIT_DIR/$_p"; then
        alloy_root rm -f -- "$ALLOY_UNIT_DIR/$_p" || return 1
        alloy_step 'done' "removed the unit $_p"
      fi
      alloy_remove_temps "$ALLOY_UNIT_DIR/$_p"
      alloy_daemon_reload
      ;;
    *)
      alloy_step warn "skipped the manifest entry '$_k $_p', which this system does not have"
      ;;
  esac
  return 0
}

# Removes the manifest and the directories that hold it, which the install
# created. The outermost of them that holds nothing but the others and the
# manifest goes in one rename, so that an interruption leaves a marker that the
# next run recognizes. A directory that holds anything else is never moved: the
# manifest is removed from it, and it is left in place.
alloy_remove_manifest() {
  _holders=$(alloy_manifest_holders "$ALLOY_TMP/manifest.old")
  alloy_remove_temps "$ALLOY_MANIFEST"
  _gone=''
  for _h in $_holders; do
    alloy_is_marker "$_h" "$_h" || continue
    alloy_remove_temp "$_h.alloy-old" || break
    alloy_root mv -- "$_h" "$_h.alloy-old" 2>/dev/null || break
    if alloy_is_marker "$_h.alloy-old"; then
      alloy_root rm -rf -- "$_h.alloy-old"
      alloy_step 'done' "removed $_h"
      _gone=$_h
    elif ! alloy_root mv -- "$_h.alloy-old" "$_h"; then
      # Another file appeared in it after the check. alloy_displaced finds
      # it on the next run.
      alloy_fail 1 "Could not move $_h.alloy-old back to $_h." "run this uninstall again"
    fi
    break
  done
  if [ -z "$_gone" ]; then
    alloy_root rm -f -- "$ALLOY_MANIFEST"
    alloy_step 'done' "removed $ALLOY_MANIFEST"
  fi
  for _h in $(printf '%s\n' "$_holders" | awk '{ a[NR] = $0 } END { for (i = NR; i >= 1; i--) print a[i] }'); do
    if [ -d "$_h" ] && ! alloy_root rmdir -- "$_h" 2>/dev/null; then
      alloy_step warn "left $_h in place: it holds files the install did not place"
    fi
  done
}

# Formats a size given in KiB with binary units.
alloy_size() {
  awk -v k="$1" 'BEGIN {
    split("KiB MiB GiB TiB PiB", u, " "); i = 1; s = k
    while (s >= 1024 && i < 5) { s /= 1024; i++ }
    if (s < 10) printf "%.1f %s\n", s, u[i]; else printf "%d %s\n", s, u[i]
  }'
}

# --- Uninstall (clause 3.16) ---------------------------------------------------

# alloy_uninstall <command for the Try: line of a root refusal>
alloy_uninstall() {
  # Step 1.
  if [ "$(id -u)" != 0 ]; then
    alloy_fail 4 "Uninstalling Alloy needs root. Nothing was changed." "$1"
  fi
  alloy_detect_os
  alloy_make_tmp
  alloy_say "Uninstalling Alloy"

  # Step 2.
  alloy_read_manifest
  alloy_check_data_dir
  _markers=$(alloy_markers)
  if [ "$ALLOY_HAVE_MANIFEST" = 0 ] && [ "$ALLOY_DD" != alloy ] && [ -z "$_markers" ]; then
    if [ "$ALLOY_DD" = other ]; then
      alloy_say "Left $ALLOY_DATA_DIR in place: it is not an Alloy data directory."
    fi
    alloy_say "Alloy is not installed on this machine."
    alloy_cleanup
    exit 0
  fi

  # Step 3.
  _vmms=$(alloy_vmm_pids | wc -l | tr -d ' ')
  if [ "$_vmms" -gt 0 ] && [ "$ALLOY_FORCE" != 1 ]; then
    alloy_fail 5 "$_vmms guest processes (VMMs) run on this machine, and uninstalling stops them. Nothing was changed." "stop or move the guests first, or add --force to stop them"
  fi

  # Step 4.
  if [ "$ALLOY_PURGE" = 1 ] && [ "$ALLOY_DD" = alloy ]; then
    if [ "$ALLOY_YES" != 1 ]; then
      _prompt="Delete the data directory $ALLOY_DATA_DIR, with this node's keys and state and every guest and disk in it? Type $ALLOY_DATA_DIR to confirm: "
      if [ -t 0 ]; then
        printf '%s' "$_prompt" >&2
        IFS= read -r _answer || _answer=''
      elif (: </dev/tty) 2>/dev/null; then
        printf '%s' "$_prompt" >&2
        IFS= read -r _answer </dev/tty || _answer=''
      else
        alloy_fail 2 "--purge deletes the data directory $ALLOY_DATA_DIR and asks first, and there is no terminal to ask on. Nothing was changed." "add --yes to confirm"
      fi
      if [ "$_answer" != "$ALLOY_DATA_DIR" ]; then
        alloy_fail 1 "Not confirmed. Nothing was changed."
      fi
    fi
    case $ALLOY_HOST_JOIN in
      joined | joining)
        _alloy="$ALLOY_BIN_DIR/alloy"
        if [ "$ALLOY_HAVE_MANIFEST" = 1 ] && grep -qx "file [0-9a-f]* $_alloy" "$ALLOY_TMP/manifest.old" && [ -x "$_alloy" ]; then
          alloy_say "Leaving the cluster"
          "$_alloy" node leave --yes </dev/null
          _rc=$?
          if [ "$_rc" != 0 ]; then
            if [ "$ALLOY_FORCE" = 1 ]; then
              alloy_step warn "alloy node leave failed with exit code $_rc; going on because of --force. The node record lasts until it expires or a cloud admin deletes it."
            else
              alloy_fail "$_rc" "alloy node leave failed with exit code $_rc, so this machine is still a node of its cluster. Nothing more was changed." "fix what alloy node leave reported, or add --force to remove the node anyway"
            fi
          fi
        elif [ "$ALLOY_FORCE" = 1 ]; then
          alloy_step warn "alloy is not installed, so the node cannot leave its cluster; going on because of --force. The node record lasts until it expires or a cloud admin deletes it."
        else
          alloy_fail 5 "This machine is a node of a cluster, and alloy is not installed, so it cannot leave the cluster. Nothing was changed." "add --force to remove the node anyway"
        fi
        ;;
    esac
  fi

  # Step 5.
  alloy_say "Stopping Alloy"
  : >"$ALLOY_TMP/exes"
  if [ "$ALLOY_HAVE_MANIFEST" = 1 ]; then
    if grep -qx "unit $ALLOY_UNIT_NAME" "$ALLOY_TMP/manifest.old" && command -v systemctl >/dev/null 2>&1; then
      alloy_root systemctl disable --now "$ALLOY_UNIT_NAME" >/dev/null 2>&1
      alloy_step 'done' "stopped and disabled $ALLOY_UNIT_NAME"
    fi
    alloy_manifest_entries "$ALLOY_TMP/manifest.old" | sed -n 's/^file //p' | while IFS= read -r _f; do
      printf '%s\n%s.alloy-old\n' "$_f" "$_f"
    done >"$ALLOY_TMP/exes"
  fi
  {
    alloy_exe_pids "$ALLOY_TMP/exes"
    alloy_vmm_pids
  } | sort -u >"$ALLOY_TMP/pids"
  if [ -s "$ALLOY_TMP/pids" ]; then
    alloy_step 'done' "asked $(wc -l <"$ALLOY_TMP/pids" | tr -d ' ') processes to stop"
    alloy_stop_pids "$ALLOY_TMP/pids"
  fi

  # Step 6.
  if [ "$ALLOY_DD" = alloy ]; then
    alloy_say "Removing host objects"
    alloy_host_matches >"$ALLOY_TMP/matches"
    while IFS= read -r _m <&3; do
      _what=${_m%%|*}
      _line=${_m#*|}
      if alloy_remove_host_object "$_line"; then
        alloy_step 'done' "removed $_what"
      else
        alloy_fail 1 "Could not remove $_what. Nothing more was changed." "run this uninstall again"
      fi
    done 3<"$ALLOY_TMP/matches"
    if [ "$(wc -l <"$ALLOY_TMP/host" | tr -d ' ')" -gt 3 ]; then
      head -n 3 "$ALLOY_TMP/host" >"$ALLOY_TMP/host.new"
      _hn="$ALLOY_DATA_DIR/alloy-host.new"
      if ! (umask 077 && alloy_root cp "$ALLOY_TMP/host.new" "$_hn" && alloy_root chmod 0600 "$_hn" && sync && alloy_root mv -f "$_hn" "$ALLOY_DATA_DIR/alloy-host"); then
        alloy_fail 1 "Could not rewrite the host file $ALLOY_DATA_DIR/alloy-host." "run this uninstall again"
      fi
      alloy_step 'done' "rewrote the host file"
    fi
  fi

  # Step 7.
  if [ "$ALLOY_HAVE_MANIFEST" = 1 ]; then
    alloy_say "Removing installed files"
    alloy_restore_displaced
    _holders=$(alloy_manifest_holders "$ALLOY_TMP/manifest.old")
    alloy_manifest_entries "$ALLOY_TMP/manifest.old" |
      awk '{ a[NR] = $0 } END { for (i = NR; i >= 1; i--) print a[i] }' >"$ALLOY_TMP/reverse"
    _left=0
    while IFS= read -r _e <&3; do
      _k=${_e%% *}
      _p=${_e#* }
      if [ "$_k" = dir ] && printf '%s\n' "$_holders" | grep -qxF -- "$_p"; then
        continue
      fi
      alloy_remove_entry "$_k" "$_p"
      if [ "$?" = 1 ]; then
        _left=1
      fi
    done 3<"$ALLOY_TMP/reverse"
    if [ "$_left" = 1 ]; then
      alloy_fail 1 "Some installed files could not be removed, so the install manifest $ALLOY_MANIFEST is kept." "run this uninstall again"
    fi
    alloy_remove_manifest
  fi
  for _mk in $(alloy_markers); do
    alloy_root rm -rf -- "$_mk"
    alloy_step 'done' "removed $_mk, left by an interrupted install"
  done

  # Step 8.
  if [ "$ALLOY_DD" = alloy ]; then
    if [ "$ALLOY_PURGE" = 1 ]; then
      alloy_root rm -rf -- "$ALLOY_DATA_DIR"
      alloy_step 'done' "removed the data directory $ALLOY_DATA_DIR"
    else
      _kib=$(alloy_root du -sk -- "$ALLOY_DATA_DIR" 2>/dev/null | awk 'NR == 1 { print $1 }')
      alloy_say "Kept the data directory $ALLOY_DATA_DIR ($(alloy_size "${_kib:-0}")). It holds this node's keys and state."
      alloy_say "To delete it: sudo rm -rf $ALLOY_DATA_DIR"
    fi
  elif [ "$ALLOY_DD" = other ]; then
    alloy_say "Left $ALLOY_DATA_DIR in place: it is not an Alloy data directory."
  fi
  alloy_scan_reserved | while IFS= read -r _o; do
    alloy_say "Left $_o in place: Alloy did not create it."
  done
  alloy_say "Kept each user's CLI configuration in ~/.config/alloy (or \$XDG_CONFIG_HOME/alloy) and the access keys the CLI stored in the keychain."
  alloy_say "Alloy is uninstalled."
  alloy_cleanup
  exit 0
}

# END SHARED

# --- Install (clause 3.15) -----------------------------------------------------

ALLOY_RELEASES_URL='https://github.com/tundrasgg/alloy/releases/download'
ALLOY_ARCH=''
ALLOY_KIND=''
ALLOY_DL=''
ALLOY_SHA=''
ALLOY_TOP=''

alloy_is_hex64() {
  case $1 in
    *[!0-9a-f]*) return 1 ;;
  esac
  [ "${#1}" -eq 64 ]
}

alloy_is_preview() {
  case $1 in
    *-preview.*) return 0 ;;
  esac
  return 1
}

alloy_valid_version() {
  _vv=$1
  _core=${_vv%%-*}
  if [ "$_core" != "$_vv" ]; then
    _pre=${_vv#*-}
    case $_pre in
      preview.*) alloy_digits "${_pre#preview.}" || return 1 ;;
      *) return 1 ;;
    esac
  fi
  _va=${_core%%.*}
  _vb=${_core#*.}
  _vc=${_vb#*.}
  _vb=${_vb%%.*}
  alloy_digits "$_va" && alloy_digits "$_vb" && alloy_digits "$_vc" && [ "$_core" = "$_va.$_vb.$_vc" ]
}

# A script whose placeholders the release workflow did not fill in refuses to
# run. The checks name no placeholder, so the workflow's replacement cannot
# change them.
alloy_check_template() {
  if ! alloy_valid_version "$ALLOY_VERSION"; then
    alloy_fail 1 "This install script is a template from the Alloy source tree, not the script of a release: its version and digests are not filled in." "use the install.sh of a release, from https://github.com/tundrasgg/alloy/releases"
  fi
  for _d in "$ALLOY_SHA256_LINUX_X86_64" "$ALLOY_SHA256_LINUX_AARCH64" \
    "$ALLOY_SHA256_MACOS_X86_64" "$ALLOY_SHA256_MACOS_AARCH64" \
    "$ALLOY_SHA256_WINDOWS_X86_64" "$ALLOY_SHA256_WINDOWS_AARCH64"; do
    if [ -n "$_d" ] && ! alloy_is_hex64 "$_d"; then
      alloy_fail 1 "This install script is a template from the Alloy source tree, not the script of a release: its digests are not filled in." "use the install.sh of a release, from https://github.com/tundrasgg/alloy/releases"
    fi
  done
}

alloy_usage() {
  alloy_fail 2 "$1
Usage: install.sh [--version <version>] [--uninstall [--purge] [--force] [--yes]]"
}

# Step 1.
alloy_platform() {
  alloy_detect_os
  _m=$(uname -m)
  if [ "$ALLOY_OS" = macos ] && [ "$(sysctl -n hw.optional.arm64 2>/dev/null)" = 1 ]; then
    _m=arm64
  fi
  case $_m in
    x86_64 | amd64) ALLOY_ARCH=x86_64 ;;
    aarch64 | arm64) ALLOY_ARCH=aarch64 ;;
    *) alloy_fail 1 "Alloy runs on x86_64 and aarch64, and this machine is $_m." ;;
  esac
  alloy_say "Alloy $ALLOY_VERSION for $ALLOY_OS-$ALLOY_ARCH"
  ALLOY_KIND=cli
  if [ "$ALLOY_OS" = linux ]; then
    _r=$(uname -r)
    _maj=${_r%%.*}
    _min=${_r#*.}
    _min=${_min%%[!0-9]*}
    _kernel_ok=0
    if alloy_digits "$_maj" && alloy_digits "$_min"; then
      if [ "$_maj" -gt 6 ] || { [ "$_maj" -eq 6 ] && [ "$_min" -ge 1 ]; }; then
        _kernel_ok=1
      fi
    fi
    if [ "$_kernel_ok" = 1 ] && [ -d /run/systemd/system ]; then
      ALLOY_KIND=node
    elif [ "$_kernel_ok" = 1 ]; then
      alloy_say "Installing the alloy command only: alloyd needs systemd as the service manager, and this machine does not run it."
    else
      alloy_say "Installing the alloy command only: alloyd needs Linux 6.1 or later, and this machine runs $_r."
    fi
  else
    alloy_say "Installing the alloy command only: alloyd does not run on macOS in this version."
  fi
}

# Step 2.
alloy_privileges() {
  if [ "$(id -u)" = 0 ]; then
    ALLOY_SUDO=''
    return 0
  fi
  if ! command -v sudo >/dev/null 2>&1; then
    alloy_fail 4 "Installing Alloy needs root, and sudo is not installed. Nothing was changed." "run the install as root"
  fi
  alloy_say "Installing Alloy needs root; asking sudo."
  if ! sudo true; then
    alloy_fail 4 "sudo refused, and installing Alloy needs root. Nothing was changed." "run the install as root"
  fi
  ALLOY_SUDO=1
}

# Step 3.
alloy_tools() {
  for _t in uname mktemp tar gzip; do
    command -v "$_t" >/dev/null 2>&1 || alloy_fail 1 "This script needs $_t, which is not installed."
  done
  if command -v curl >/dev/null 2>&1; then
    ALLOY_DL=curl
  elif command -v wget >/dev/null 2>&1; then
    ALLOY_DL=wget
  else
    alloy_fail 1 "This script needs curl or wget, and neither is installed."
  fi
  if command -v sha256sum >/dev/null 2>&1; then
    ALLOY_SHA=sha256sum
  elif command -v shasum >/dev/null 2>&1; then
    ALLOY_SHA=shasum
  else
    alloy_fail 1 "This script needs sha256sum or shasum, and neither is installed."
  fi
}

alloy_sha256() {
  if [ "$ALLOY_SHA" = sha256sum ]; then
    sha256sum "$1"
  else
    shasum -a 256 "$1"
  fi | awk 'NR == 1 { print $1 }'
}

# alloy_download <asset path under the releases URL> <destination>
# Returns 0 when downloaded, 2 when an attempt got an HTTP status of 400 or
# more, 3 when no attempt got an HTTP response, and 4 when an attempt got a
# response and the transfer failed.
alloy_download() {
  if [ -n "${ALLOY_INSTALL_TEST_RELEASES:-}" ]; then
    if [ -f "$ALLOY_INSTALL_TEST_RELEASES/$1" ]; then
      cp "$ALLOY_INSTALL_TEST_RELEASES/$1" "$2" && return 0
      return 4
    fi
    return 2
  fi
  _url="$ALLOY_RELEASES_URL/$1"
  _attempt=1
  _status=''
  _wget_opts='-T 30'
  if [ "$ALLOY_DL" = wget ] && wget --help 2>&1 | grep -q -- '--connect-timeout'; then
    _wget_opts='--connect-timeout=10 --read-timeout=30 --tries=1'
  fi
  while :; do
    if [ "$ALLOY_DL" = curl ]; then
      _code=$(curl --proto '=https' --proto-redir '=https' --tlsv1.2 -sSL \
        --connect-timeout 10 --speed-limit 1 --speed-time 30 \
        -o "$2" -w '%{http_code}' "$_url" 2>"$ALLOY_TMP/download.err")
      _rc=$?
      case $_code in
        2??) [ "$_rc" = 0 ] && return 0 ;;
      esac
      case $_code in
        '' | 000) ;;
        *) _status=$_code ;;
      esac
    else
      # shellcheck disable=SC2086
      wget -q $_wget_opts -O "$2" "$_url" 2>"$ALLOY_TMP/download.err"
      _rc=$?
      [ "$_rc" = 0 ] && return 0
      if [ "$_rc" = 8 ]; then
        _status=400
      elif grep -q 'server returned error: HTTP/[0-9.]* [45]' "$ALLOY_TMP/download.err"; then
        _status=400
      fi
    fi
    [ "$_attempt" -lt 3 ] || break
    _attempt=$((_attempt + 1))
    sleep 2
  done
  case $_status in
    '') return 3 ;;
    [45]??) return 2 ;;
  esac
  return 4
}

alloy_archive_sha() {
  case $ALLOY_OS-$ALLOY_ARCH in
    linux-x86_64) printf '%s\n' "$ALLOY_SHA256_LINUX_X86_64" ;;
    linux-aarch64) printf '%s\n' "$ALLOY_SHA256_LINUX_AARCH64" ;;
    macos-x86_64) printf '%s\n' "$ALLOY_SHA256_MACOS_X86_64" ;;
    macos-aarch64) printf '%s\n' "$ALLOY_SHA256_MACOS_AARCH64" ;;
  esac
}

# The paths of this install other than the manifest and the files of the
# archive, which are known only once it is extracted.
alloy_fixed_paths() {
  printf '%s\n' "$ALLOY_SHARE_DIR" "$ALLOY_BIN_DIR/alloy"
  if [ "$ALLOY_KIND" = node ]; then
    printf '%s\n' "$ALLOY_BIN_DIR/alloyd" "$ALLOY_UNIT_DIR/$ALLOY_UNIT_NAME"
  fi
}

# Prints, for a list of paths on standard input, every one that exists, or
# whose temporary name exists, and that the earlier manifest does not list.
alloy_path_conflicts() {
  while IFS= read -r _cp; do
    for _c in "$_cp" "$_cp.alloy-new" "$_cp.alloy-old"; do
      alloy_exists "$_c" || continue
      if [ "$ALLOY_HAVE_MANIFEST" = 1 ] && alloy_manifest_entries "$ALLOY_TMP/manifest.old" | sed 's/^[a-z]* //' | grep -qxF -- "$_cp"; then
        continue
      fi
      if [ "$_c" != "$_cp" ] && alloy_is_marker "$_c"; then
        continue
      fi
      printf '%s\n' "$_c"
    done
  done
}

# Step 4.
alloy_conflicts() {
  : >"$ALLOY_TMP/conflicts"
  if [ "$ALLOY_HAVE_MANIFEST" = 0 ]; then
    alloy_host_matches | sed 's/|.*//' >"$ALLOY_TMP/owned"
    alloy_scan_reserved | grep -vxF -f "$ALLOY_TMP/owned" >>"$ALLOY_TMP/conflicts"
    if [ "$ALLOY_DD" = other ]; then
      printf '%s, which is not an Alloy data directory\n' "$ALLOY_DATA_DIR" >>"$ALLOY_TMP/conflicts"
    fi
    if alloy_exists "$ALLOY_MANIFEST"; then
      printf '%s\n' "$ALLOY_MANIFEST" >>"$ALLOY_TMP/conflicts"
    fi
  fi
  alloy_fixed_paths | alloy_path_conflicts >>"$ALLOY_TMP/conflicts"
  if [ -s "$ALLOY_TMP/conflicts" ]; then
    alloy_say "These already exist, and this install did not create them:"
    while IFS= read -r _c; do
      alloy_say "  $_c"
    done <"$ALLOY_TMP/conflicts"
    alloy_fail 5 "Alloy installs only where nothing else is, so nothing was changed." "remove or rename what is listed above, then run the install again"
  fi
  case $ALLOY_HOST_JOIN in
    joined | joining)
      if [ "$ALLOY_HOST_RELEASE" != none ] && [ "$ALLOY_HOST_RELEASE" != "$ALLOY_VERSION" ]; then
        if alloy_is_preview "$ALLOY_HOST_RELEASE" || alloy_is_preview "$ALLOY_VERSION"; then
          alloy_fail 5 "This machine is a node on release $ALLOY_HOST_RELEASE, and this install is release $ALLOY_VERSION. A node on a preview release, or moving to one, changes release only by an uninstall with --purge and a new alloy init or alloy join. Nothing was changed." "sudo sh $ALLOY_SHARE_DIR/uninstall.sh --purge, then install $ALLOY_VERSION and run alloy init or alloy join"
        fi
      fi
      ;;
  esac
}

# True when the manifest says this release is completely installed.
alloy_already_installed() {
  [ "$ALLOY_HAVE_MANIFEST" = 1 ] || return 1
  [ "$ALLOY_M_STATE" = complete ] && [ "$ALLOY_M_VERSION" = "$ALLOY_VERSION" ] && [ "$ALLOY_M_KIND" = "$ALLOY_KIND" ] || return 1
  _bad=$(awk 'NR > 4 && $1 == "file" { h = $2; sub(/^file [0-9a-f]+ /, ""); print h " " $0 }' "$ALLOY_TMP/manifest.old" | while IFS= read -r _l; do
    _h=${_l%% *}
    _f=${_l#* }
    if [ ! -f "$_f" ] || [ "$(alloy_sha256 "$_f")" != "$_h" ]; then
      echo bad
      break
    fi
  done)
  [ -z "$_bad" ] || return 1
  if [ "$ALLOY_KIND" = node ]; then
    [ "$(systemctl show -p LoadState --value "$ALLOY_UNIT_NAME" 2>/dev/null)" = loaded ] || return 1
  fi
  return 0
}

# Checks every entry of the archive against clause 3.13, then extracts it.
alloy_extract() {
  _ar=$1
  _x="$ALLOY_TMP/x"
  if ! tar -tzf "$_ar" >"$ALLOY_TMP/names" 2>/dev/null || ! tar -tvzf "$_ar" >"$ALLOY_TMP/long" 2>/dev/null; then
    alloy_fail 1 "The archive ${_ar##*/} cannot be read."
  fi
  while IFS= read -r _n; do
    case $_n in
      /*) alloy_fail 1 "The archive ${_ar##*/} has the absolute entry $_n, so nothing was installed." ;;
    esac
    case "/$_n/" in
      */../*) alloy_fail 1 "The archive ${_ar##*/} has the entry $_n, which leaves its directory, so nothing was installed." ;;
    esac
    case $_n in
      *[!A-Za-z0-9._/+-]*) alloy_fail 1 "The archive ${_ar##*/} has an entry with an unexpected name, so nothing was installed." ;;
    esac
    case $_n in
      "$ALLOY_TOP" | "$ALLOY_TOP"/ | "$ALLOY_TOP"/*) ;;
      *) alloy_fail 1 "The archive ${_ar##*/} has the entry $_n outside $ALLOY_TOP/, so nothing was installed." ;;
    esac
  done <"$ALLOY_TMP/names"
  if ! awk '{ c = substr($1, 1, 1); if (c != "-" && c != "d") { exit 1 } }' "$ALLOY_TMP/long"; then
    alloy_fail 1 "The archive ${_ar##*/} has an entry that is a link or a device, so nothing was installed."
  fi
  if [ "$(wc -l <"$ALLOY_TMP/names")" != "$(wc -l <"$ALLOY_TMP/long")" ]; then
    alloy_fail 1 "The archive ${_ar##*/} has an entry with an unexpected name, so nothing was installed."
  fi
  if ! mkdir "$_x" || ! tar -xzf "$_ar" -C "$_x" 2>/dev/null; then
    alloy_fail 1 "The archive ${_ar##*/} could not be extracted."
  fi
  if [ -n "$(find "$_x" ! -type f ! -type d -print 2>/dev/null | head -n 1)" ]; then
    alloy_fail 1 "The archive ${_ar##*/} has an entry that is a link or a device, so nothing was installed."
  fi
  for _need in bin/alloy uninstall.sh; do
    [ -f "$_x/$ALLOY_TOP/$_need" ] || alloy_fail 1 "The archive ${_ar##*/} has no $_need."
  done
  if [ "$ALLOY_KIND" = node ] && [ ! -f "$_x/$ALLOY_TOP/bin/alloyd" ]; then
    alloy_fail 1 "The archive ${_ar##*/} has no bin/alloyd."
  fi
}

# Step 5.
alloy_fetch() {
  _sha=$(alloy_archive_sha)
  _asset="alloy-$ALLOY_VERSION-$ALLOY_OS-$ALLOY_ARCH.tar.gz"
  if [ -z "$_sha" ]; then
    alloy_fail 1 "Release $ALLOY_VERSION has no build for $ALLOY_OS-$ALLOY_ARCH."
  fi
  alloy_say "Downloading $_asset"
  alloy_download "v$ALLOY_VERSION/$_asset" "$ALLOY_TMP/$_asset"
  case $? in
    0) ;;
    2) alloy_fail 1 "Release $ALLOY_VERSION has no build for $ALLOY_OS-$ALLOY_ARCH: $ALLOY_RELEASES_URL/v$ALLOY_VERSION/$_asset was not found." ;;
    3) alloy_fail 6 "Could not reach github.com to download $_asset, after 3 attempts. Nothing was changed." "check this machine's connection to github.com, then run the install again" ;;
    *) alloy_fail 1 "The download of $_asset failed, after 3 attempts. Nothing was changed." "run the install again" ;;
  esac
  _got=$(alloy_sha256 "$ALLOY_TMP/$_asset")
  if [ "$_got" != "$_sha" ]; then
    alloy_fail 1 "The archive $_asset has the SHA-256 $_got, and this release's digest for it is $_sha, so nothing was installed."
  fi
  alloy_step ok "checked the SHA-256 of $_asset"
  ALLOY_TOP="alloy-$ALLOY_VERSION-$ALLOY_OS-$ALLOY_ARCH"
  alloy_extract "$ALLOY_TMP/$_asset"
  alloy_step ok "checked and extracted every entry"
}

# Writes $ALLOY_TMP/new: the manifest entries of this release, in creation
# order, and $ALLOY_TMP/sources: "<source>|<mode>|<path>" for each file.
alloy_plan() {
  _src="$ALLOY_TMP/x/$ALLOY_TOP"
  : >"$ALLOY_TMP/new"
  : >"$ALLOY_TMP/sources"
  _dirs="$ALLOY_MANIFEST_CHAIN $ALLOY_BIN_DIR"
  for _d in $_dirs; do
    if ! alloy_exists "$_d" || { [ "$ALLOY_HAVE_MANIFEST" = 1 ] && grep -qxF "dir $_d" "$ALLOY_TMP/manifest.old"; }; then
      printf 'dir %s\n' "$_d" >>"$ALLOY_TMP/new"
    fi
  done
  _bins=alloy
  if [ "$ALLOY_KIND" = node ]; then
    _bins=$(cd "$_src/bin" && find . -print | sed -n 's|^\./||p' | LC_ALL=C sort)
  fi
  for _b in $_bins; do
    case $_b in
      */*) alloy_fail 1 "The archive has bin/$_b, and bin/ holds only files." ;;
    esac
    [ -f "$_src/bin/$_b" ] || alloy_fail 1 "The archive has bin/$_b, and bin/ holds only files."
    printf 'file %s %s\n' "$(alloy_sha256 "$_src/bin/$_b")" "$ALLOY_BIN_DIR/$_b" >>"$ALLOY_TMP/new"
    printf '%s|0755|%s\n' "$_src/bin/$_b" "$ALLOY_BIN_DIR/$_b" >>"$ALLOY_TMP/sources"
  done
  (cd "$_src" && find licenses -print | LC_ALL=C sort) | while IFS= read -r _l; do
    if [ -d "$_src/$_l" ]; then
      printf 'dir %s\n' "$ALLOY_SHARE_DIR/$_l" >>"$ALLOY_TMP/new"
    else
      printf 'file %s %s\n' "$(alloy_sha256 "$_src/$_l")" "$ALLOY_SHARE_DIR/$_l" >>"$ALLOY_TMP/new"
      printf '%s|0644|%s\n' "$_src/$_l" "$ALLOY_SHARE_DIR/$_l" >>"$ALLOY_TMP/sources"
    fi
  done
  printf 'file %s %s\n' "$(alloy_sha256 "$_src/uninstall.sh")" "$ALLOY_SHARE_DIR/uninstall.sh" >>"$ALLOY_TMP/new"
  printf '%s|0644|%s\n' "$_src/uninstall.sh" "$ALLOY_SHARE_DIR/uninstall.sh" >>"$ALLOY_TMP/sources"
  if [ "$ALLOY_KIND" = node ]; then
    alloy_unit >"$ALLOY_TMP/$ALLOY_UNIT_NAME"
    printf 'file %s %s\n' "$(alloy_sha256 "$ALLOY_TMP/$ALLOY_UNIT_NAME")" "$ALLOY_UNIT_DIR/$ALLOY_UNIT_NAME" >>"$ALLOY_TMP/new"
    printf '%s|0644|%s\n' "$ALLOY_TMP/$ALLOY_UNIT_NAME" "$ALLOY_UNIT_DIR/$ALLOY_UNIT_NAME" >>"$ALLOY_TMP/sources"
    printf 'unit %s\n' "$ALLOY_UNIT_NAME" >>"$ALLOY_TMP/new"
  fi
  # Step 4, for the paths that only the archive names: each one that exists
  # must be listed in the earlier manifest.
  alloy_manifest_entries "$ALLOY_TMP/new" 0 | sed -n 's/^file //p' | alloy_path_conflicts >"$ALLOY_TMP/conflicts"
  if [ -s "$ALLOY_TMP/conflicts" ]; then
    alloy_say "These already exist, and this install did not create them:"
    while IFS= read -r _c; do
      alloy_say "  $_c"
    done <"$ALLOY_TMP/conflicts"
    alloy_fail 5 "Alloy installs only where nothing else is, so nothing was changed." "remove or rename what is listed above, then run the install again"
  fi
}

# The systemd unit (clause 3.15 step 7). packaging/check.sh checks that this
# text equals packaging/alloyd.service.
alloy_unit() {
  cat <<'EOF'
[Unit]
Description=Alloy node daemon
Documentation=https://github.com/tundrasgg/alloy
Wants=network-online.target
After=network-online.target

[Service]
Type=exec
ExecStart=/usr/local/bin/alloyd
Restart=on-failure
RestartSec=5
KillMode=process
TimeoutStopSec=30

[Install]
WantedBy=multi-user.target
EOF
}

alloy_manifest_text() {
  printf 'alloy-manifest 1\nversion %s\nkind %s\nstate %s\n' "$ALLOY_VERSION" "$ALLOY_KIND" "$1"
}

# Installs the file $1 as $2 with the mode $3: written as its .alloy-new name
# and renamed over the target.
alloy_put() {
  alloy_root rm -f -- "$2.alloy-new" &&
    alloy_root cp -- "$1" "$2.alloy-new" &&
    alloy_root chmod "$3" -- "$2.alloy-new" &&
    alloy_root mv -f -- "$2.alloy-new" "$2"
}

# Writes the manifest. With no manifest yet, the directories that hold it are
# created under a temporary name with the manifest inside, and renamed into
# place in one step.
alloy_write_manifest() {
  if alloy_exists "$ALLOY_MANIFEST"; then
    alloy_put "$1" "$ALLOY_MANIFEST" 0644 || alloy_fail 1 "Could not write the install manifest $ALLOY_MANIFEST." "run the install again"
    return 0
  fi
  _top=''
  for _c in $ALLOY_MANIFEST_CHAIN; do
    if [ -z "$_top" ] && ! alloy_exists "$_c"; then
      _top=$_c
    fi
  done
  if [ -z "$_top" ]; then
    alloy_fail 5 "$ALLOY_SHARE_DIR exists, and this install did not create it. Nothing was changed."
  fi
  for _c in $ALLOY_MANIFEST_CHAIN; do
    alloy_remove_temps "$_c" || alloy_fail 5 "$_c.alloy-new or $_c.alloy-old exists, and this install did not create it. Nothing was changed."
  done
  _stage="$_top.alloy-new"
  alloy_root mkdir -m 0755 -- "$_stage" || alloy_fail 1 "Could not create $_stage."
  for _c in $ALLOY_MANIFEST_CHAIN; do
    case $_c in
      "$_top"/*) alloy_root mkdir -m 0755 -- "$_stage${_c#"$_top"}" || alloy_fail 1 "Could not create $_stage${_c#"$_top"}." "run the install again" ;;
    esac
  done
  _m="$_stage${ALLOY_MANIFEST#"$_top"}"
  if ! alloy_root cp -- "$1" "$_m" || ! alloy_root chmod 0644 -- "$_m" || ! alloy_root mv -- "$_stage" "$_top"; then
    alloy_fail 1 "Could not create the install manifest $ALLOY_MANIFEST." "run the install again"
  fi
}

# Step 6.
alloy_place() {
  alloy_say "Installing"
  # The pending manifest lists every path of the earlier install and of this
  # one, so that an interruption from here on leaves a machine that a rerun
  # completes and an uninstall cleans.
  {
    alloy_manifest_text pending
    if [ "$ALLOY_HAVE_MANIFEST" = 1 ]; then
      tail -n +5 "$ALLOY_TMP/manifest.old"
    fi
    cat "$ALLOY_TMP/new"
  } | awk '
    { key = $1 " " $0; if ($1 == "file") { k = $0; sub(/^file [0-9a-f]+ /, "", k); key = "file " k } }
    NR <= 4 || !(key in seen) { seen[key] = 1; print }
  ' >"$ALLOY_TMP/manifest.pending"
  alloy_restore_displaced
  alloy_write_manifest "$ALLOY_TMP/manifest.pending"
  ALLOY_CHANGED=0
  while IFS= read -r _e <&3; do
    _k=${_e%% *}
    case $_k in
      dir)
        _d=${_e#dir }
        if ! alloy_exists "$_d"; then
          alloy_root mkdir -m 0755 -- "$_d" || alloy_fail 1 "Could not create $_d." "run the install again"
        fi
        ;;
    esac
  done 3<"$ALLOY_TMP/new"
  while IFS='|' read -r _s _mode _t <&3; do
    _h=$(alloy_sha256 "$_s")
    if [ -f "$_t" ] && [ "$(alloy_sha256 "$_t")" = "$_h" ]; then
      continue
    fi
    alloy_put "$_s" "$_t" "$_mode" || alloy_fail 1 "Could not write $_t." "run the install again"
    case $_t in
      "$ALLOY_BIN_DIR"/* | "$ALLOY_UNIT_DIR"/*) ALLOY_CHANGED=1 ;;
    esac
    alloy_step 'done' "$_t"
  done 3<"$ALLOY_TMP/sources"
  if [ "$ALLOY_KIND" = node ]; then
    alloy_daemon_reload
  fi
  # Remove what the earlier install has and this one does not.
  if [ "$ALLOY_HAVE_MANIFEST" = 1 ]; then
    alloy_manifest_entries "$ALLOY_TMP/new" 0 >"$ALLOY_TMP/new.paths"
    alloy_manifest_entries "$ALLOY_TMP/manifest.old" |
      awk '{ a[NR] = $0 } END { for (i = NR; i >= 1; i--) print a[i] }' >"$ALLOY_TMP/old.reverse"
    : >"$ALLOY_TMP/kept"
    while IFS= read -r _e <&3; do
      grep -qxF -- "$_e" "$ALLOY_TMP/new.paths" && continue
      if ! alloy_remove_entry "${_e%% *}" "${_e#* }"; then
        printf '%s\n' "$_e" >>"$ALLOY_TMP/kept"
      fi
    done 3<"$ALLOY_TMP/old.reverse"
  fi
  # Remove every temporary name of a manifest path that exists.
  alloy_manifest_entries "$ALLOY_TMP/manifest.pending" | while IFS= read -r _e; do
    _p=${_e#* }
    case ${_e%% *} in
      unit) _p="$ALLOY_UNIT_DIR/$_p" ;;
    esac
    alloy_remove_temps "$_p" >/dev/null
  done
  {
    alloy_manifest_text complete
    cat "$ALLOY_TMP/new"
    if [ "$ALLOY_HAVE_MANIFEST" = 1 ] && [ -s "$ALLOY_TMP/kept" ]; then
      # Earlier directories that still hold files this install did not
      # place stay listed, so that an uninstall removes them once empty.
      grep -xF -f "$ALLOY_TMP/kept" "$ALLOY_TMP/manifest.old" | grep '^dir '
    fi
  } >"$ALLOY_TMP/manifest.complete"
  alloy_put "$ALLOY_TMP/manifest.complete" "$ALLOY_MANIFEST" 0644 || alloy_fail 1 "Could not write the install manifest $ALLOY_MANIFEST." "run the install again"
  alloy_step 'done' "$ALLOY_MANIFEST"
}

# Step 7.
alloy_service() {
  [ "$ALLOY_KIND" = node ] || return 0
  if [ "$ALLOY_HAVE_MANIFEST" = 1 ] && [ "$ALLOY_M_STATE" = complete ]; then
    if [ "$ALLOY_WAS_ACTIVE" = 1 ] && [ "$ALLOY_CHANGED" = 1 ]; then
      alloy_root systemctl restart "$ALLOY_UNIT_NAME" || alloy_fail 1 "Could not restart $ALLOY_UNIT_NAME." "systemctl status $ALLOY_UNIT_NAME"
      alloy_step 'done' "restarted $ALLOY_UNIT_NAME"
    fi
    return 0
  fi
  case $ALLOY_HOST_JOIN in
    joined | joining)
      alloy_root systemctl enable "$ALLOY_UNIT_NAME" >/dev/null 2>&1 || alloy_fail 1 "Could not enable $ALLOY_UNIT_NAME." "systemctl status $ALLOY_UNIT_NAME"
      if [ "$ALLOY_WAS_ACTIVE" = 1 ] && [ "$ALLOY_CHANGED" = 1 ]; then
        alloy_root systemctl restart "$ALLOY_UNIT_NAME"
      else
        alloy_root systemctl start "$ALLOY_UNIT_NAME"
      fi || alloy_fail 1 "Could not start $ALLOY_UNIT_NAME." "systemctl status $ALLOY_UNIT_NAME"
      alloy_step 'done' "enabled and started $ALLOY_UNIT_NAME: this machine is a node, and it resumes"
      ;;
  esac
}

# Step 8.
alloy_verify() {
  _out=$("$ALLOY_BIN_DIR/alloy" version 2>/dev/null </dev/null)
  if [ "$_out" != "$ALLOY_VERSION" ]; then
    alloy_fail 1 "$ALLOY_BIN_DIR/alloy version printed '$_out', not $ALLOY_VERSION." "run the install again"
  fi
  alloy_step ok "alloy version prints $ALLOY_VERSION"
  alloy_say ""
  alloy_say "Installed Alloy $ALLOY_VERSION ($ALLOY_KIND):"
  alloy_manifest_entries "$ALLOY_TMP/new" 0 | sed -n 's/^file //p' | grep -v "^$ALLOY_SHARE_DIR/licenses/" | while IFS= read -r _f; do
    printf '  %s\n' "$_f"
  done
  printf '  %s\n' "$ALLOY_SHARE_DIR/licenses/" "$ALLOY_MANIFEST"
  if [ "$ALLOY_KIND" = node ]; then
    _en=$(systemctl is-enabled "$ALLOY_UNIT_NAME" 2>/dev/null)
    _ac=$(systemctl is-active "$ALLOY_UNIT_NAME" 2>/dev/null)
    alloy_say "Service $ALLOY_UNIT_NAME: ${_en:-unknown}, ${_ac:-unknown}"
  fi
  _first=$(command -v alloy 2>/dev/null)
  _IFS=$IFS
  IFS=:
  for _d in $PATH; do
    IFS=$_IFS
    if [ -z "$_d" ] || [ "$_d" = "$ALLOY_BIN_DIR" ]; then
      continue
    fi
    if [ -f "$_d/alloy" ] && [ -x "$_d/alloy" ]; then
      alloy_say "Warning: another alloy is installed at $_d/alloy. The search path finds ${_first:-neither} first."
    fi
  done
  IFS=$_IFS
  if [ "$ALLOY_KIND" = node ]; then
    alloy_say "Next: sudo alloy init, or sudo alloy join <join token>"
  else
    alloy_say "Next: alloy login <login code>"
  fi
}

alloy_install() {
  alloy_platform
  alloy_privileges
  alloy_tools
  alloy_make_tmp
  alloy_read_manifest
  alloy_check_data_dir
  alloy_conflicts
  if alloy_already_installed; then
    alloy_say "Alloy $ALLOY_VERSION is already installed. Nothing was changed."
    alloy_cleanup
    exit 0
  fi
  alloy_fetch
  alloy_plan
  ALLOY_WAS_ACTIVE=0
  if [ "$ALLOY_KIND" = node ] && systemctl is-active --quiet "$ALLOY_UNIT_NAME" 2>/dev/null; then
    ALLOY_WAS_ACTIVE=1
  fi
  alloy_place
  alloy_service
  alloy_verify
  alloy_cleanup
  exit 0
}

# --version <version>: runs the install script of that release instead.
alloy_other_release() {
  _ver=$1
  shift
  alloy_tools
  alloy_make_tmp
  alloy_say "Downloading the install script of Alloy $_ver"
  alloy_download "v$_ver/install.sh" "$ALLOY_TMP/install.sh"
  case $? in
    0) ;;
    2) alloy_fail 1 "There is no release $_ver: $ALLOY_RELEASES_URL/v$_ver/install.sh was not found." ;;
    3) alloy_fail 6 "Could not reach github.com to download the install script of $_ver, after 3 attempts." "check this machine's connection to github.com, then run the install again" ;;
    *) alloy_fail 1 "The download of the install script of $_ver failed, after 3 attempts." "run the install again" ;;
  esac
  sh "$ALLOY_TMP/install.sh" "$@" </dev/null
  _rc=$?
  alloy_cleanup
  exit "$_rc"
}

main() {
  set -u
  alloy_traps
  alloy_check_template
  _version=''
  _uninstall=0
  _rest=''
  _n=0
  while [ $# -gt 0 ]; do
    case $1 in
      --version)
        [ $# -ge 2 ] || alloy_usage "--version needs a version, such as 0.0.1-preview.1."
        alloy_valid_version "$2" || alloy_usage "$2 is not a version: a version is MAJOR.MINOR.PATCH, or 0.0.PATCH-preview.N."
        _version=$2
        shift 2
        continue
        ;;
      --uninstall) _uninstall=1 ;;
      --purge) ALLOY_PURGE=1 ;;
      --force) ALLOY_FORCE=1 ;;
      --yes) ALLOY_YES=1 ;;
      *) alloy_usage "Unknown argument $1." ;;
    esac
    _rest="$_rest $1"
    _n=$((_n + 1))
    shift
  done
  if [ "$_uninstall" = 0 ] && [ "$_n" -gt 0 ]; then
    alloy_usage "--purge, --force, and --yes go with --uninstall."
  fi
  if [ -n "$_version" ] && [ "$_version" != "$ALLOY_VERSION" ]; then
    # shellcheck disable=SC2086
    alloy_other_release "$_version" $_rest
  fi
  if [ "$_uninstall" = 1 ]; then
    alloy_uninstall "curl -fsSL $ALLOY_RELEASES_URL/v$ALLOY_VERSION/install.sh | sudo sh -s --$_rest"
  fi
  alloy_install
}

{ main "$@"; }

#!/bin/sh
# Alloy uninstall script for Linux and macOS (ARFC 0017 clause 3.16).
#
#   sudo sh /usr/local/share/alloy/uninstall.sh [--purge] [--force] [--yes]
#
# It reads only the install manifest, the host file of the data directory, and
# the reserved names of clause 3.17, whose formats every release shares, so it
# removes the install of any release. install.sh --uninstall does the same and
# needs only the manifest.
#
# Every top-level command before the last line only defines a function or sets
# a variable, and the last line starts the work inside a brace group.
#
# The shared block also sets variables that only install.sh reads.
# shellcheck disable=SC2034

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

main() {
  set -u
  alloy_traps
  while [ $# -gt 0 ]; do
    case $1 in
      --purge) ALLOY_PURGE=1 ;;
      --force) ALLOY_FORCE=1 ;;
      --yes) ALLOY_YES=1 ;;
      *) alloy_fail 2 "Unknown argument $1.
Usage: uninstall.sh [--purge] [--force] [--yes]" ;;
    esac
    shift
  done
  _try="sudo sh $ALLOY_SHARE_DIR/uninstall.sh"
  [ "$ALLOY_PURGE" = 0 ] || _try="$_try --purge"
  [ "$ALLOY_FORCE" = 0 ] || _try="$_try --force"
  [ "$ALLOY_YES" = 0 ] || _try="$_try --yes"
  alloy_uninstall "$_try"
}

{ main "$@"; }

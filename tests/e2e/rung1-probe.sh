#!/bin/sh
# Reports the install state of a Linux machine for the M11 rung 1 check
# (ARFC 0017 clauses 3.15 and 3.16). Run as root on the machine:
#
#   sh rung1-probe.sh <version> [--check-b64 <base64 of a list of paths>]
#
# It only reads: it writes no file and changes nothing. The one program it
# starts other than standard tools is the installed `alloy version`. Each output
# line is a record whose first word names it:
#
#   uid <id>                         the user id the probe runs as
#   path <yes|no> <path>             whether a path of the install footprint exists
#   tempname <path>                  a temporary name (clause 3.15 step 6) that exists
#   mhead <n> <line>                 line n (1 to 4) of the manifest (section 4.1)
#   mfile <ok|bad|missing> <path>    a file line of the manifest, checked against its digest
#   mdir <path>                      a dir line of the manifest
#   munit <name>                     a unit line of the manifest
#   fp <mode> <uid> <gid> <size> <mtime> <sha256> <path>
#                                    the fingerprint of the manifest and of each
#                                    file and unit it lists
#   unit_enabled <state>             `systemctl is-enabled alloyd.service`
#   unit_active <state>              `systemctl is-active alloyd.service`
#   version_exit <code>              the exit code of `alloy version`
#   version_out <text>               its output on one line
#   version_match <yes|no>           whether a word of that output is the version
#
# Every top-level command before the last line only defines a function or sets
# a variable, and the last line runs `main` with standard input from /dev/null,
# so the script can be piped to `sh -s`.

LC_ALL=C
export LC_ALL

PREFIX_BIN=/usr/local/bin
SHARE=/usr/local/share/alloy
MANIFEST=/usr/local/share/alloy/manifest
UNIT_DIR=/etc/systemd/system
UNIT=/etc/systemd/system/alloyd.service
# The default data directory, ARFC 0002 section 3.3.
DATA_DIR=/var/lib/alloyd
RUN_DIR=/run/alloy

exists() {
    if [ -e "$1" ] || [ -L "$1" ]; then
        printf 'yes'
    else
        printf 'no'
    fi
}

sha256() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" 2>/dev/null | cut -d ' ' -f 1
    else
        shasum -a 256 "$1" 2>/dev/null | cut -d ' ' -f 1
    fi
}

fingerprint() {
    if [ -e "$1" ]; then
        printf 'fp %s %s %s\n' "$(stat -c '%a %u %g %s %Y' "$1" 2>/dev/null)" "$(sha256 "$1")" "$1"
    else
        printf 'fp - - - - - - %s\n' "$1"
    fi
}

# Prints every path whose footprint is checked: the fixed paths of clause 3.15
# step 6, the paths the manifest lists, and the paths given by the driver.
footprint_paths() {
    printf '%s\n' "$PREFIX_BIN/alloy" "$PREFIX_BIN/alloyd" "$SHARE" "$MANIFEST" \
        "$SHARE/uninstall.sh" "$UNIT" "$DATA_DIR" "$RUN_DIR"
    if [ -f "$MANIFEST" ]; then
        tail -n +5 "$MANIFEST" | while IFS= read -r line; do
            case "$line" in
                'dir '*) printf '%s\n' "${line#dir }" ;;
                'file '*)
                    rest=${line#file }
                    printf '%s\n' "${rest#* }"
                    ;;
                'unit '*) printf '%s/%s\n' "$UNIT_DIR" "${line#unit }" ;;
            esac
        done
    fi
    if [ -n "$extra" ]; then
        printf '%s' "$extra" | base64 -d 2>/dev/null
        printf '\n'
    fi
}

report_paths() {
    footprint_paths | sort -u | while IFS= read -r p; do
        [ -n "$p" ] || continue
        printf 'path %s %s\n' "$(exists "$p")" "$p"
        for t in "$p.alloy-new" "$p.alloy-old"; do
            if [ "$(exists "$t")" = yes ]; then
                printf 'tempname %s\n' "$t"
            fi
        done
    done
    # Temporary names in the install's directories, whatever path they belong to.
    for f in "$PREFIX_BIN"/*.alloy-new "$PREFIX_BIN"/*.alloy-old \
        "$SHARE"/*.alloy-new "$SHARE"/*.alloy-old \
        "$UNIT_DIR"/*.alloy-new "$UNIT_DIR"/*.alloy-old; do
        if [ "$(exists "$f")" = yes ]; then
            printf 'tempname %s\n' "$f"
        fi
    done
}

report_manifest() {
    [ -f "$MANIFEST" ] || return 0
    n=0
    head -n 4 "$MANIFEST" | while IFS= read -r line; do
        n=$((n + 1))
        printf 'mhead %s %s\n' "$n" "$line"
    done
    fingerprint "$MANIFEST"
    tail -n +5 "$MANIFEST" | while IFS= read -r line; do
        case "$line" in
            'file '*)
                rest=${line#file }
                want=${rest%% *}
                path=${rest#* }
                if [ ! -f "$path" ]; then
                    printf 'mfile missing %s\n' "$path"
                elif [ "$(sha256 "$path")" = "$want" ]; then
                    printf 'mfile ok %s\n' "$path"
                else
                    printf 'mfile bad %s\n' "$path"
                fi
                fingerprint "$path"
                ;;
            'dir '*)
                printf 'mdir %s\n' "${line#dir }"
                ;;
            'unit '*)
                printf 'munit %s\n' "${line#unit }"
                fingerprint "$UNIT_DIR/${line#unit }"
                ;;
        esac
    done
}

report_service() {
    if command -v systemctl >/dev/null 2>&1; then
        enabled=$(systemctl is-enabled alloyd.service 2>/dev/null | head -n 1)
        active=$(systemctl is-active alloyd.service 2>/dev/null | head -n 1)
        printf 'unit_enabled %s\n' "${enabled:-(none)}"
        printf 'unit_active %s\n' "${active:-(none)}"
    else
        printf 'unit_enabled (no systemctl)\n'
        printf 'unit_active (no systemctl)\n'
    fi
}

report_version() {
    if [ ! -x "$PREFIX_BIN/alloy" ]; then
        printf 'version_exit (absent)\n'
        printf 'version_match no\n'
        return 0
    fi
    out=$("$PREFIX_BIN/alloy" version 2>&1)
    code=$?
    printf 'version_exit %s\n' "$code"
    printf 'version_out %s\n' "$(printf '%s' "$out" | tr '\n' ' ')"
    match=no
    set -f
    for word in $out; do
        if [ "$word" = "$version" ] || [ "$word" = "v$version" ]; then
            match=yes
        fi
    done
    set +f
    printf 'version_match %s\n' "$match"
}

main() {
    if [ $# -lt 1 ]; then
        printf 'usage: rung1-probe.sh <version> [--check-b64 <list>]\n' >&2
        return 2
    fi
    version=$1
    shift
    extra=
    while [ $# -gt 0 ]; do
        case "$1" in
            --check-b64)
                extra=${2-}
                shift 2 || return 2
                ;;
            *)
                printf 'usage: rung1-probe.sh <version> [--check-b64 <list>]\n' >&2
                return 2
                ;;
        esac
    done
    printf 'uid %s\n' "$(id -u)"
    report_paths
    report_manifest
    report_service
    report_version
    return 0
}

{ main "$@" </dev/null; }

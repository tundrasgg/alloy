#!/bin/sh
# Prints the host snapshot of a Linux machine, ARFC 0017 clause 3.17.6.
#
# Usage: sh host-snapshot.sh [--proxmox] [--self-check <seconds>]
#
# Run it as root on the machine. It only reads: it writes no file, starts no
# service, and changes nothing. The output is one section per command of the
# clause, each headed by a line `### <name>`, with counters, statistics,
# timestamps, and the units no install places (see norm_units) removed so that
# two snapshots of an unchanged machine are equal.
# With --proxmox it adds the Proxmox VE sections of the clause.
#
# With --self-check <seconds> it prints no snapshot. It takes two snapshots
# <seconds> apart and checks that they are equal, in the format of the other
# scripts under tests/e2e: a PASS or FAIL line, the differences indented under
# a FAIL (`-` only in the first snapshot, `+` only in the second), and a
# SUMMARY line. It exits 0 when they are equal, 1 when not, and 2 on a usage
# error. Run it on an idle machine before a rung run: a difference is a value
# that changes on its own and that the snapshot must remove.
#
# Every top-level command before the last line only defines a function or sets
# a variable, and the last line runs `main` with standard input from /dev/null,
# so the script can be piped to `sh -s` and no command it runs reads the rest
# of the script as input.

LC_ALL=C
export LC_ALL

section() {
    printf '### %s\n' "$1"
}

has() {
    command -v "$1" >/dev/null 2>&1
}

# Runs a command and prints its output, or a marker when the command is absent
# or fails, so that the absence of a tool is itself part of the snapshot.
run() {
    if ! has "$1"; then
        printf '(absent: %s)\n' "$1"
        return 0
    fi
    "$@" 2>/dev/null
    status=$?
    if [ "$status" -ne 0 ]; then
        printf '(exit %s)\n' "$status"
    fi
    return 0
}

# `ip -d link show`: the values the kernel changes during normal operation,
# each listed with its reason in tests/e2e/README.md. Each value is removed and
# its name kept; every other attribute is configuration and is kept.
#
# - promiscuity, allmulti: reference counts.
# - Every `*_timer` of a bridge or bridge port (hello_timer, tcn_timer,
#   topology_change_timer, gc_timer, message_age_timer, forward_delay_timer,
#   hold_timer, mcast_querier_ipv4_other_timer, ...): running timers.
# - fdb_n_learned (bridge): the number of MAC addresses the bridge has learned.
# - mcast_n_groups (bridge port): the number of multicast groups snooped.
# - mcast_querier_ipv4_addr, mcast_querier_ipv4_port and the ipv6 pair
#   (bridge): the multicast querier the bridge last heard on the LAN.
# - topology_change, topology_change_detected (bridge), topology_change_ack,
#   config_pending (bridge port): STP flags that the timers above clear.
# - link_failure_count (bond port): a counter.
# - berr-counter (CAN): bus error counters.
norm_link() {
    sed -e 's/ promiscuity [0-9][0-9]*/ promiscuity/g' \
        -e 's/ allmulti [0-9][0-9]*/ allmulti/g' \
        -e 's/ \([a-z0-9_]*_timer\) *[0-9.]*/ \1/g' \
        -e 's/ fdb_n_learned [0-9][0-9]*/ fdb_n_learned/g' \
        -e 's/ mcast_n_groups [0-9][0-9]*/ mcast_n_groups/g' \
        -e 's/ \(mcast_querier_ipv[46]_addr\) [^ ]*/ \1/g' \
        -e 's/ \(mcast_querier_ipv[46]_port\) [0-9][0-9]*/ \1/g' \
        -e 's/ \(topology_change\) [0-9][0-9]*/ \1/g' \
        -e 's/ \(topology_change_detected\) [0-9][0-9]*/ \1/g' \
        -e 's/ \(topology_change_ack\) [0-9][0-9]*/ \1/g' \
        -e 's/ \(config_pending\) [0-9][0-9]*/ \1/g' \
        -e 's/ \(link_failure_count\) [0-9][0-9]*/ \1/g' \
        -e 's/(berr-counter tx [0-9][0-9]* rx [0-9][0-9]*)/(berr-counter)/g'
}

# `ip address show`: remaining address lifetimes count down. `forever` is kept.
norm_addr() {
    sed -e 's/valid_lft [0-9][0-9]*sec/valid_lft N/g' \
        -e 's/preferred_lft [0-9][0-9]*sec/preferred_lft N/g'
}

# Routes: the remaining lifetime of a learned route counts down.
norm_route() {
    sed -e 's/ expires -*[0-9][0-9]*sec/ expires N/g'
}

# `nft list ruleset`: counters, both the `counter` statement of a rule or set
# element and the `packets N bytes N` line of a named counter object, quota
# usage, `last used` ages, and the remaining lifetime of set elements.
norm_nft() {
    sed -e 's/counter packets [0-9]* bytes [0-9]*/counter/g' \
        -e 's/^\([[:space:]]*\)packets [0-9]* bytes [0-9]*[[:space:]]*$/\1packets bytes/' \
        -e 's/ used [0-9]* [a-z]*bytes//g' \
        -e 's/last used [^ ;}]*/last/g' \
        -e 's/ expires [0-9a-z.]*//g'
}

# `iptables-save`: the comment lines carry the time of the dump, and chain
# headers carry packet and byte counters.
norm_iptables() {
    sed -e '/^#/d' -e 's/\[[0-9]*:[0-9]*\]/[0:0]/g'
}

# `sysctl -a` restricted to `net.`: the connection tracking table size is a
# counter.
norm_sysctl() {
    grep '^net\.' | grep -v '^net\.netfilter\.nf_conntrack_count ' | sort
}

# `systemctl list-unit-files`: units in the state `transient` or `generated`
# have no unit file that anything installed. systemd makes transient units at
# run time, such as the `session-<n>.scope` of each login, the SSH login of
# this check included, and generators make generated units from other
# configuration, such as a `.mount` unit for an fstab line. Neither is a unit
# an install places (a file in /etc/systemd/system), and both change with no
# action by Alloy.
norm_units() {
    awk '$2 != "transient" && $2 != "generated"'
}

linux_sections() {
    section 'ip -d link show'
    run ip -d link show | norm_link

    section 'ip address show'
    run ip address show | norm_addr

    section 'ip route show table all'
    run ip route show table all | norm_route

    section 'ip -6 route show table all'
    run ip -6 route show table all | norm_route

    section 'ip rule show'
    run ip rule show

    section 'ip -6 rule show'
    run ip -6 rule show

    section 'nft list ruleset'
    run nft list ruleset | norm_nft

    section 'iptables-save'
    if has iptables-save; then
        run iptables-save | norm_iptables
    else
        printf '(not present)\n'
    fi

    section 'ip6tables-save'
    if has ip6tables-save; then
        run ip6tables-save | norm_iptables
    else
        printf '(not present)\n'
    fi

    section 'sysctl -a (net.)'
    run sysctl -a | norm_sysctl

    section 'systemctl list-unit-files'
    run systemctl list-unit-files --no-legend --no-pager | norm_units

    section 'getent passwd'
    run getent passwd

    section 'getent group'
    run getent group

    section '/etc/resolv.conf'
    if [ -e /etc/resolv.conf ]; then
        cat /etc/resolv.conf 2>/dev/null || printf '(unreadable)\n'
    else
        printf '(absent)\n'
    fi
}

# Proxmox VE: the guests with their states and configurations.
pve_guests() {
    if has qm; then
        printf 'qm list\n'
        run qm list
        for id in $(qm list 2>/dev/null | awk 'NR > 1 { print $1 }'); do
            printf 'qm config %s\n' "$id"
            run qm config "$id"
        done
    else
        printf '(absent: qm)\n'
    fi
    if has pct; then
        printf 'pct list\n'
        run pct list
        for id in $(pct list 2>/dev/null | awk 'NR > 1 { print $1 }'); do
            printf 'pct config %s\n' "$id"
            run pct config "$id"
        done
    else
        printf '(absent: pct)\n'
    fi
}

# Proxmox VE: whether a file under /etc/pve is one of the HA status files that
# Proxmox VE rewrites on its own. The local resource manager rewrites
# `nodes/<node>/lrm_status` and the cluster resource manager rewrites
# `ha/manager_status`, each as JSON with a `timestamp` field that advances and
# with its keys in no fixed order.
pve_status_file() {
    case "$1" in
        /etc/pve/nodes/*/lrm_status | */manager_status) return 0 ;;
    esac
    return 1
}

# Proxmox VE: the tool that normalizes the HA status files, the first present
# of `perl` with JSON::PP and `python3`, or `none`.
pve_json_tool() {
    if has perl && perl -MJSON::PP -e 1 >/dev/null 2>&1; then
        printf 'perl\n'
    elif has python3 && python3 -c 'import json' >/dev/null 2>&1; then
        printf 'python3\n'
    else
        printf 'none\n'
    fi
}

# Proxmox VE: prints the JSON on standard input with every `timestamp` key
# removed, at any depth, and the keys sorted, with no whitespace, using the
# tool named by $1. Exits nonzero when the input is not JSON or the tool is
# `none`.
pve_normalize_json() {
    case "$1" in
        perl)
            perl -MJSON::PP -e '
                sub strip {
                    my ($v) = @_;
                    if (ref $v eq "HASH") {
                        delete $v->{timestamp};
                        strip($_) for values %$v;
                    } elsif (ref $v eq "ARRAY") {
                        strip($_) for @$v;
                    }
                }
                local $/;
                my $text = <STDIN>;
                my $json = JSON::PP->new->utf8->allow_nonref;
                my $data = $json->decode(defined $text ? $text : "");
                strip($data);
                print $json->canonical->encode($data);
            '
            ;;
        python3)
            python3 -c '
import json, sys
def strip(v):
    if isinstance(v, dict):
        v.pop("timestamp", None)
        for x in v.values():
            strip(x)
    elif isinstance(v, list):
        for x in v:
            strip(x)
data = json.load(sys.stdin)
strip(data)
sys.stdout.write(json.dumps(data, sort_keys=True, separators=(",", ":")))
'
            ;;
        *)
            return 1
            ;;
    esac
}

# Proxmox VE: the SHA-256 of every file under /etc/pve. The dot files at its
# top (`.version`, `.members`, `.vmlist`, `.clusterlog`, `.rrd`, `.debug`) are
# the cluster file system's status files, made of counters, statistics, and
# timestamps, so they are left out, as are the temporary names that Proxmox VE
# writes before renaming (`*.tmp.<pid>`). The HA status files (see
# pve_status_file) are hashed after normalizing: parsed as JSON, every
# `timestamp` removed, and the keys sorted. A status file that cannot be
# normalized is hashed raw and its line says so. Every other file is hashed
# raw.
pve_digests() {
    if [ ! -d /etc/pve ]; then
        printf '(absent: /etc/pve)\n'
        return 0
    fi
    tool=$(pve_json_tool)
    if [ "$tool" = none ]; then
        printf '(HA status files hashed raw: neither perl with JSON::PP nor python3 is present)\n'
    else
        printf '(HA status files normalized with %s)\n' "$tool"
    fi
    find /etc/pve -type f ! -path '/etc/pve/.*' ! -name '*.tmp.[0-9]*' 2>/dev/null |
        sort |
        while IFS= read -r file; do
            note=
            if pve_status_file "$file"; then
                if json=$(pve_normalize_json "$tool" 2>/dev/null <"$file"); then
                    digest=$(printf '%s' "$json" | sha256sum | cut -d ' ' -f 1)
                    printf '%s  %s\n' "$digest" "$file"
                    continue
                fi
                note=' (raw: not normalized)'
            fi
            digest=$(sha256sum 2>/dev/null <"$file" | cut -d ' ' -f 1)
            printf '%s  %s%s\n' "$digest" "$file" "$note"
        done
}

pve_sections() {
    section 'pve guests'
    pve_guests

    section 'pve /etc/network/interfaces'
    if [ -e /etc/network/interfaces ]; then
        cat /etc/network/interfaces 2>/dev/null || printf '(unreadable)\n'
    else
        printf '(absent)\n'
    fi

    section 'pve /etc/pve digests'
    pve_digests
}

snapshot() {
    linux_sections
    if [ "$proxmox" = yes ]; then
        pve_sections
    fi
    section 'end'
}

# Reads two snapshots, one after the other, from standard input, each ending
# with its `### end` line, and writes the check of the self-check mode. $1 is
# the machine name and $2 the seconds between the snapshots. Lines are compared
# as a multiset within each section, so a line is a difference only when it is
# in one snapshot more times than in the other.
compare_snapshots() {
    awk -v machine="$1" -v seconds="$2" '
        BEGIN { part = 1; sec = ""; nsec = 0; n1 = 0; n2 = 0; done1 = 0; done2 = 0 }
        part > 2 { next }
        /^### / {
            sec = substr($0, 5)
            if (sec == "end") {
                if (part == 1) { done1 = 1 } else { done2 = 1 }
                part += 1
                sec = ""
                next
            }
            if (!(sec in seen)) { seen[sec] = 1; order[++nsec] = sec }
        }
        {
            key = sec SUBSEP $0
            if (part == 1) {
                n1 += 1; sec1[n1] = sec; line1[n1] = $0; count[key] += 1
            } else {
                n2 += 1; sec2[n2] = sec; line2[n2] = $0
            }
        }
        END {
            name = "two snapshots " seconds " s apart are equal"
            if (!done1 || !done2) {
                printf "FAIL  %s  %s\n", machine, name
                printf "  the %s snapshot is incomplete\n", (done1 ? "second" : "first")
                printf "SUMMARY  0 passed, 1 failed\n"
                exit 1
            }
            nadd = 0
            for (i = 1; i <= n2; i++) {
                key = sec2[i] SUBSEP line2[i]
                if (count[key] > 0) { count[key] -= 1 } else { nadd += 1; addsec[nadd] = sec2[i]; addline[nadd] = line2[i] }
            }
            nrem = 0
            for (i = 1; i <= n1; i++) {
                key = sec1[i] SUBSEP line1[i]
                if (count[key] > 0) { count[key] -= 1; nrem += 1; remsec[nrem] = sec1[i]; remline[nrem] = line1[i] }
            }
            if (nadd == 0 && nrem == 0) {
                printf "PASS  %s  %s\n", machine, name
                printf "SUMMARY  1 passed, 0 failed\n"
                exit 0
            }
            printf "FAIL  %s  %s\n", machine, name
            for (s = 1; s <= nsec; s++) {
                header = 0
                for (i = 1; i <= nrem; i++) {
                    if (remsec[i] != order[s]) continue
                    if (!header) { printf "  section '\''%s'\'':\n", order[s]; header = 1 }
                    printf "    - %s\n", remline[i]
                }
                for (i = 1; i <= nadd; i++) {
                    if (addsec[i] != order[s]) continue
                    if (!header) { printf "  section '\''%s'\'':\n", order[s]; header = 1 }
                    printf "    + %s\n", addline[i]
                }
            }
            printf "SUMMARY  0 passed, 1 failed\n"
            exit 1
        }
    '
}

usage() {
    printf 'usage: host-snapshot.sh [--proxmox] [--self-check <seconds>]\n' >&2
}

main() {
    proxmox=no
    seconds=
    while [ "$#" -gt 0 ]; do
        case "$1" in
            --proxmox) proxmox=yes ;;
            --self-check)
                if [ "$#" -lt 2 ]; then
                    usage
                    return 2
                fi
                seconds=$2
                case "$seconds" in
                    '' | *[!0-9]*)
                        usage
                        return 2
                        ;;
                esac
                shift
                ;;
            *)
                usage
                return 2
                ;;
        esac
        shift
    done
    if [ -z "$seconds" ]; then
        snapshot
        return 0
    fi
    machine=$(uname -n 2>/dev/null || printf 'localhost')
    { snapshot; sleep "$seconds"; snapshot; } | compare_snapshots "$machine" "$seconds"
}

{ main "$@" </dev/null; }

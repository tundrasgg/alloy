#!/bin/sh
# Prints the host snapshot of a Linux machine, ARFC 0017 clause 3.17.6.
#
# Usage: sh host-snapshot.sh [--proxmox]
#
# Run it as root on the machine. It only reads: it writes no file, starts no
# service, and changes nothing. The output is one section per command of the
# clause, each headed by a line `### <name>`, with counters, statistics, and
# timestamps removed so that two snapshots of an unchanged machine are equal.
# With --proxmox it adds the Proxmox VE sections of the clause.
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

# `ip -d link show`: promiscuity and allmulti are reference counts, and the
# bridge and bridge port `*_timer` fields are running timers.
norm_link() {
    sed -e 's/ promiscuity [0-9]*//g' \
        -e 's/ allmulti [0-9]*//g' \
        -e 's/ \([a-z_]*_timer\) *[0-9.]*/ \1/g'
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

# `nft list ruleset`: counters, quota usage, `last used` ages, and the
# remaining lifetime of dynamic set elements.
norm_nft() {
    sed -e 's/counter packets [0-9]* bytes [0-9]*/counter/g' \
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
    run systemctl list-unit-files --no-legend --no-pager

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

# Proxmox VE: the SHA-256 of every file under /etc/pve. The dot files at its
# top (`.version`, `.members`, `.vmlist`, `.clusterlog`, `.rrd`, `.debug`) are
# the cluster file system's status files, made of counters, statistics, and
# timestamps, so they are left out. The HA status files carry a `timestamp`
# field that is removed before hashing, and the temporary names that Proxmox
# VE writes before renaming (`*.tmp.<pid>`) are left out.
pve_digests() {
    if [ ! -d /etc/pve ]; then
        printf '(absent: /etc/pve)\n'
        return 0
    fi
    find /etc/pve -type f ! -path '/etc/pve/.*' ! -name '*.tmp.[0-9]*' 2>/dev/null |
        sort |
        while IFS= read -r file; do
            digest=$(sed -e 's/"timestamp":[0-9]*//g' "$file" 2>/dev/null | sha256sum | cut -d ' ' -f 1)
            printf '%s  %s\n' "$digest" "$file"
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

main() {
    proxmox=no
    for arg in "$@"; do
        case "$arg" in
            --proxmox) proxmox=yes ;;
            *)
                printf 'usage: host-snapshot.sh [--proxmox]\n' >&2
                return 2
                ;;
        esac
    done
    linux_sections
    if [ "$proxmox" = yes ]; then
        pve_sections
    fi
    section 'end'
    return 0
}

{ main "$@" </dev/null; }

#!/bin/sh
# Reports what the M11 rung 2 check needs to know about a Linux node before it
# joins: the name and roles the node should get (ARFC 0002 sections 3.1, 3.3,
# and 3.8), and whether the user it runs as has an `alloy` CLI configuration
# (ARFC 0019 clause 3.1.9). Run as root on the machine:
#
#   sh rung2-probe.sh
#
# It only reads: it writes no file and changes nothing. Each output line is a
# record whose first word names it:
#
#   uid <id>             the user id the probe runs as
#   hostname <name>      the hostname, as `uname -n` prints it, from which the
#                        default node name comes (ARFC 0002 section 3.3)
#   kvm <ok|warn>        the hardware virtualization check of ARFC 0002
#                        section 3.8: `ok` when /dev/kvm is a character device
#                        that opens for reading and writing
#   cliconfig <yes|no> <path>
#                        whether the CLI configuration directory of the user
#                        the probe runs as exists: `$XDG_CONFIG_HOME/alloy`,
#                        with `$HOME/.config` when XDG_CONFIG_HOME is unset or
#                        empty (ARFC 0019 clause 3.1.9)
#
# Every top-level command before the last line only defines a function or sets
# a variable, and the last line runs `main` with standard input from /dev/null,
# so the script can be piped to `sh -s`.

LC_ALL=C
export LC_ALL

# The test for a character device comes first: `<>` creates a file that does
# not exist, and the probe must create nothing.
kvm_state() {
    if [ -c /dev/kvm ] && (exec 3<>/dev/kvm) 2>/dev/null; then
        printf 'ok'
    else
        printf 'warn'
    fi
}

# The CLI configuration directory of the user the script runs as. The same rule
# is in rung2-cli-remove.sh.
cli_config_dir() {
    printf '%s/alloy' "${XDG_CONFIG_HOME:-$HOME/.config}"
}

exists() {
    if [ -e "$1" ] || [ -L "$1" ]; then
        printf 'yes'
    else
        printf 'no'
    fi
}

main() {
    if [ $# -ne 0 ]; then
        printf 'usage: rung2-probe.sh\n' >&2
        return 2
    fi
    printf 'uid %s\n' "$(id -u)"
    printf 'hostname %s\n' "$(uname -n)"
    printf 'kvm %s\n' "$(kvm_state)"
    config=$(cli_config_dir)
    printf 'cliconfig %s %s\n' "$(exists "$config")" "$config"
    return 0
}

{ main "$@" </dev/null; }

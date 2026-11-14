#!/bin/sh
# Removes the `alloy` CLI configuration directory of the user it runs as, for
# the M11 rung 2 check. An uninstall leaves it in place (ARFC 0017 clause
# 3.16), and while it holds a current context, `alloy login --issue-code` run
# by root on a control node goes to the API instead of the local admin socket
# (ARFC 0019 clause 3.1.14), so a rerun of the check needs it gone. The driver
# runs this only on a machine whose directory did not exist before the run, so
# it removes only what the run made. Run as root on the machine:
#
#   sh rung2-cli-remove.sh
#
# It removes that one directory and nothing else, then prints one record:
#
#   cliconfig <yes|no> <path>   whether the directory still exists
#
# It exits 0 when the directory is gone, 1 when it remains, and 2 on a usage
# error. Every top-level command before the last line only defines a function
# or sets a variable, and the last line runs `main` with standard input from
# /dev/null, so the script can be piped to `sh -s`.

LC_ALL=C
export LC_ALL

# The CLI configuration directory of the user the script runs as (ARFC 0019
# clause 3.1.9). The same rule is in rung2-probe.sh.
cli_config_dir() {
    printf '%s/alloy' "${XDG_CONFIG_HOME:-$HOME/.config}"
}

present() {
    [ -e "$1" ] || [ -L "$1" ]
}

main() {
    if [ $# -ne 0 ]; then
        printf 'usage: rung2-cli-remove.sh\n' >&2
        return 2
    fi
    config=$(cli_config_dir)
    if present "$config"; then
        rm -rf -- "$config"
    fi
    if present "$config"; then
        printf 'cliconfig yes %s\n' "$config"
        return 1
    fi
    printf 'cliconfig no %s\n' "$config"
    return 0
}

{ main "$@" </dev/null; }

#!/bin/sh
# Sets the version of the binary crates to a release's version, so that
# `alloy version` and `alloyd version` print it (ARFC 0017 clauses 3.13 and
# 3.15 step 8). The release workflow runs it before the build, then runs
# `cargo update --workspace` so that Cargo.lock matches.
#
#   sh packaging/set-version.sh <version> [<crate dir>...]
#
# The crate directories default to crates/alloy and crates/alloyd. A crate
# whose manifest says `version.workspace = true` gets the version through
# [workspace.package] in the root Cargo.toml.

set -eu
if [ $# -lt 1 ]; then
  echo "usage: set-version.sh <version> [<crate dir>...]" >&2
  exit 2
fi
version=$1
shift
root=$(cd "$(dirname "$0")/.." && pwd)
[ $# -gt 0 ] || set -- crates/alloy crates/alloyd

die() {
  echo "set-version: $*" >&2
  exit 1
}

case $version in
  *[!0-9A-Za-z.-]* | '') die "$version is not a version" ;;
esac

# set_in <file> <section header>: replaces the first version line of the
# section.
set_in() {
  awk -v v="$version" -v sect="$2" '
    /^\[/ { insec = ($0 == sect) }
    insec && !done && /^version[ \t]*=/ { print "version = \"" v "\""; done = 1; next }
    { print }
    END { if (!done) exit 3 }
  ' "$1" >"$1.tmp" || {
    rm -f "$1.tmp"
    die "$1 has no version line in $2"
  }
  mv "$1.tmp" "$1"
  echo "set-version: $1 $2 version = \"$version\""
}

workspace=0
for crate in "$@"; do
  manifest="$root/$crate/Cargo.toml"
  [ -f "$manifest" ] || die "$manifest does not exist"
  if grep -Eq '^version(\.workspace[[:space:]]*=[[:space:]]*true|[[:space:]]*=[[:space:]]*\{[[:space:]]*workspace[[:space:]]*=[[:space:]]*true)' "$manifest"; then
    workspace=1
  else
    set_in "$manifest" '[package]'
  fi
done
if [ "$workspace" = 1 ]; then
  set_in "$root/Cargo.toml" '[workspace.package]'
fi

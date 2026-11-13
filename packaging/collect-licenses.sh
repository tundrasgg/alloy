#!/bin/sh
# Copies the license texts of everything compiled into the given packages for
# a target into a directory, for the licenses/ directory of a release archive
# (ARFC 0017 clause 3.13): the copyright notices of the Rust standard library
# from the toolchain, and the license files of every crate the packages link,
# named <crate>-<version>-<file>. It also writes crates.txt, one line per
# crate with its license expression, which make-archive.sh puts in the
# archive's license listing. Crates of this workspace are under Alloy's own
# license and are left out.
#
#   sh packaging/collect-licenses.sh <target triple> <out dir> <package>...
#
# Run it from the workspace after a build, so the crate sources are in the
# Cargo registry. A crate with no license file stops it, since the archive must
# not ship code without its license.

set -eu
if [ $# -lt 3 ]; then
  echo "usage: collect-licenses.sh <target triple> <out dir> <package>..." >&2
  exit 2
fi
target=$1
out=$2
shift 2
mkdir -p "$out"

packages=''
for p in "$@"; do
  packages="$packages -p $p"
done
# shellcheck disable=SC2086
cargo tree --locked --target "$target" -e normal,no-proc-macro --prefix none --format '{p}|{l}' $packages |
  sed 's/ (\*)$//' | LC_ALL=C sort -u >"$out/.tree"

cargo_home=${CARGO_HOME:-$HOME/.cargo}
if command -v cygpath >/dev/null 2>&1; then
  cargo_home=$(cygpath -u "$cargo_home")
fi
registry="$cargo_home/registry/src"
: >"$out/crates.txt"
missing=''
while IFS='|' read -r pkg license; do
  case $pkg in
    '') continue ;;     # the blank line between the trees of two packages
    *' ('*) continue ;; # a workspace crate, or a proc-macro, which is not linked
  esac
  name=${pkg% v*}
  version=${pkg##* v}
  dir=''
  for d in "$registry"/*/"$name-$version"; do
    [ -d "$d" ] && dir=$d
  done
  if [ -z "$dir" ]; then
    echo "collect-licenses: no source of $name $version under $registry" >&2
    exit 1
  fi
  found=0
  for f in "$dir"/*; do
    [ -f "$f" ] || continue
    base=${f##*/}
    case $(printf '%s' "$base" | tr '[:lower:]' '[:upper:]') in
      LICENSE* | LICENCE* | COPYING* | COPYRIGHT* | NOTICE* | UNLICENSE*)
        cp "$f" "$out/$name-$version-$base"
        found=1
        ;;
    esac
  done
  if [ "$found" = 0 ]; then
    missing="$missing $name-$version"
  fi
  printf '%s %s: %s\n' "$name" "$version" "${license:-unknown}" >>"$out/crates.txt"
done <"$out/.tree"
rm -f "$out/.tree"
if [ -n "$missing" ]; then
  echo "collect-licenses: these crates have no license file:$missing" >&2
  exit 1
fi

std="$(rustc --print sysroot)/share/doc/rust/COPYRIGHT-library.html"
if [ ! -f "$std" ]; then
  echo "collect-licenses: the toolchain has no $std" >&2
  exit 1
fi
cp "$std" "$out/rust-std-COPYRIGHT-library.html"
echo "collect-licenses: $(wc -l <"$out/crates.txt" | tr -d ' ') crates for $target in $out"

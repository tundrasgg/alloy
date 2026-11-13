#!/bin/sh
# Assembles one release archive in the layout of ARFC 0017 clause 3.13.
#
#   sh packaging/make-archive.sh <version> <os> <arch> <stage dir> <out dir>
#
# <os> is linux, macos, or windows, and <arch> x86_64 or aarch64. <stage dir>
# holds bin/, the executables (and, for Windows, wintun.dll), and licenses/,
# the output of collect-licenses.sh (and, for Windows, wintun.txt). The archive
# is written to <out dir> as alloy-<version>-<os>-<arch>.tar.gz, or .zip for
# Windows. It holds one directory, alloy-<version>-<os>-<arch>/, with bin/,
# the uninstall script, and licenses/: Alloy's license texts, the staged ones,
# and README.txt, which lists what is in the archive under which license.
#
# Entries are sorted, owned by user 0, and dated SOURCE_DATE_EPOCH, by default
# the time of the commit checked out, so the same inputs give the same bytes.
# Needs GNU tar, gzip, and, for Windows, zip.

set -eu
if [ $# -ne 5 ]; then
  echo "usage: make-archive.sh <version> <os> <arch> <stage dir> <out dir>" >&2
  exit 2
fi
version=$1
os=$2
arch=$3
stage=$4
out=$5
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/.." && pwd)

die() {
  echo "make-archive: $*" >&2
  exit 1
}

case $os in
  linux | macos)
    ext=tar.gz
    exe=''
    uninstall=uninstall.sh
    ;;
  windows)
    ext=zip
    exe=.exe
    uninstall=uninstall.ps1
    ;;
  *) die "unknown os $os" ;;
esac
case $arch in
  x86_64 | aarch64) ;;
  *) die "unknown arch $arch" ;;
esac

name="alloy-$version-$os-$arch"
[ -f "$stage/bin/alloy$exe" ] || die "$stage/bin has no alloy$exe"
case $os in
  linux) [ -f "$stage/bin/alloyd" ] || die "$stage/bin has no alloyd" ;;
  windows)
    [ -f "$stage/bin/alloyd.exe" ] || die "$stage/bin has no alloyd.exe"
    [ -f "$stage/bin/wintun.dll" ] || die "$stage/bin has no wintun.dll"
    [ -f "$stage/licenses/wintun.txt" ] || die "$stage/licenses has no wintun.txt, the license of wintun.dll (ADR 0062)"
    ;;
esac
if [ "$os" != windows ] && [ -e "$stage/bin/wintun.dll" ]; then
  die "only the Windows archives may hold wintun.dll (ADR 0062)"
fi
[ -f "$stage/licenses/crates.txt" ] || die "$stage/licenses has no crates.txt; run collect-licenses.sh"

epoch=${SOURCE_DATE_EPOCH:-$(git -C "$root" log -1 --format=%ct)}
work=$(mktemp -d "${TMPDIR:-/tmp}/alloy-archive.XXXXXX")
trap 'rm -rf "$work"' EXIT
dir="$work/$name"
mkdir -p "$dir/bin" "$dir/licenses"

for f in "$stage"/bin/*; do
  [ -f "$f" ] || die "$f is not a file"
  cp "$f" "$dir/bin/"
  case $f in
    *.dll) chmod 0644 "$dir/bin/${f##*/}" ;;
    *) chmod 0755 "$dir/bin/${f##*/}" ;;
  esac
done
cp "$here/$uninstall" "$dir/$uninstall"
cp "$root/LICENSE-MIT" "$root/LICENSE-APACHE" "$dir/licenses/"
for f in "$stage"/licenses/*; do
  [ -f "$f" ] || die "$f is not a file"
  [ "${f##*/}" = crates.txt ] || cp "$f" "$dir/licenses/"
done

{
  echo "Alloy $version for $os-$arch"
  echo
  echo "Alloy is licensed under the MIT license (LICENSE-MIT) or the Apache"
  echo "License, Version 2.0 (LICENSE-APACHE), at your option."
  echo
  echo "The executables in bin/ also contain the Rust standard library, whose"
  echo "copyright notices and license texts are in rust-std-COPYRIGHT-library.html,"
  echo "and the crates below, each under its own license. Their license files are"
  echo "in this directory, named <crate>-<version>-<file>."
  echo
  sed 's/^/  /' "$stage/licenses/crates.txt"
  if [ "$os" = windows ]; then
    echo
    echo "bin/wintun.dll is the signed Wintun DLL from https://www.wintun.net/,"
    echo "unmodified. It is not under a permissive license: its license, in"
    echo "wintun.txt, allows it to be redistributed only unmodified and together"
    echo "with software that uses it through its published API. Whoever"
    echo "redistributes this archive must keep wintun.txt with it."
  fi
} >"$dir/licenses/README.txt"
find "$dir" -type f -exec chmod u+rw,go+r {} +
find "$dir" -type d -exec chmod 0755 {} +
find "$dir" -exec touch -h -d "@$epoch" {} +

mkdir -p "$out"
out=$(cd "$out" && pwd)
(cd "$work" && find "$name" -print | LC_ALL=C sort >"$work/list")
case $ext in
  tar.gz)
    (cd "$work" && tar --format=ustar --owner=0 --group=0 --numeric-owner \
      --mtime="@$epoch" --no-recursion -cf - -T "$work/list") | gzip -n -9 >"$out/$name.$ext"
    ;;
  zip)
    rm -f "$out/$name.$ext"
    (cd "$work" && TZ=UTC zip -X -q "$out/$name.$ext" -@ <"$work/list")
    ;;
esac
echo "make-archive: $out/$name.$ext ($(wc -l <"$work/list" | tr -d ' ') entries)"

#!/bin/sh
# Writes a release's version and the SHA-256 of each of its archives into
# copies of install.sh and install.ps1, then writes SHA256SUMS (ARFC 0017
# clause 3.14). The release workflow runs it after the archives are built and
# before anything is published.
#
#   sh packaging/stamp.sh <version> <dir>
#
# <dir> holds the release's archives and nothing else, but for the files this
# script writes, from an earlier run. The script writes <dir>/install.sh,
# <dir>/install.ps1, and <dir>/SHA256SUMS. A placeholder for
# an archive the release does not have is replaced by nothing, which the
# scripts read as "no build for this platform".

set -eu
if [ $# -ne 2 ]; then
  echo "usage: stamp.sh <version> <dir>" >&2
  exit 2
fi
version=$1
dir=$2
here=$(cd "$(dirname "$0")" && pwd)

die() {
  echo "stamp: $*" >&2
  exit 1
}

case $version in
  *[!0-9A-Za-z.-]* | '') die "$version is not a version" ;;
esac

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1"
  else
    shasum -a 256 "$1"
  fi | awk '{ print $1 }'
}

# Only archives of this release, by their exact names.
for f in "$dir"/*; do
  [ -e "$f" ] || continue
  case ${f##*/} in
    "alloy-$version-linux-x86_64.tar.gz" | "alloy-$version-linux-aarch64.tar.gz" | \
      "alloy-$version-macos-x86_64.tar.gz" | "alloy-$version-macos-aarch64.tar.gz" | \
      "alloy-$version-windows-x86_64.zip" | "alloy-$version-windows-aarch64.zip" | \
      install.sh | install.ps1 | SHA256SUMS) ;;
    *) die "$f is not an archive of release $version" ;;
  esac
done

expr=''
count=0
for platform in linux-x86_64 linux-aarch64 macos-x86_64 macos-aarch64 windows-x86_64 windows-aarch64; do
  case $platform in
    windows-*) asset="alloy-$version-$platform.zip" ;;
    *) asset="alloy-$version-$platform.tar.gz" ;;
  esac
  digest=''
  if [ -f "$dir/$asset" ]; then
    digest=$(sha256 "$dir/$asset")
    count=$((count + 1))
  fi
  key=$(printf '%s' "$platform" | tr 'a-z-' 'A-Z_')
  expr="$expr s/@ALLOY_SHA256_${key}@/$digest/;"
done
[ "$count" -gt 0 ] || die "$dir holds no archive of release $version"

for script in install.sh install.ps1; do
  sed "s/@ALLOY_VERSION@/$version/; $expr" "$here/$script" >"$dir/$script"
  if grep -q '@ALLOY_' "$dir/$script"; then
    die "$dir/$script still holds a placeholder"
  fi
done
sh -n "$dir/install.sh" || die "$dir/install.sh does not parse"

# One line per other asset, in byte order of the asset name.
(cd "$dir" && for f in $(find . -maxdepth 1 -type f ! -name 'SHA256SUMS*' -print | sed 's|^\./||' | LC_ALL=C sort); do
  printf '%s  %s\n' "$(sha256 "$f")" "$f"
done) >"$dir/SHA256SUMS.tmp"
mv "$dir/SHA256SUMS.tmp" "$dir/SHA256SUMS"
echo "stamp: wrote the digests of $count archives into install.sh and install.ps1, and SHA256SUMS:"
cat "$dir/SHA256SUMS"

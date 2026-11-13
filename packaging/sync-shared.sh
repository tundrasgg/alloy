#!/bin/sh
# Copies the shared block of each install script, the lines from "# BEGIN
# SHARED" to "# END SHARED", over the shared block of its uninstall script.
# Edit the block in install.sh or install.ps1, then run this. packaging/check.sh
# fails while the blocks differ.
#
#   sh packaging/sync-shared.sh

set -eu
here=$(cd "$(dirname "$0")" && pwd)

for pair in "install.sh uninstall.sh" "install.ps1 uninstall.ps1"; do
  src="$here/${pair% *}"
  dst="$here/${pair#* }"
  awk -v src="$src" '
    BEGIN {
      while ((getline line < src) > 0) {
        if (line ~ /^# BEGIN SHARED/) on = 1
        if (on) block = block line "\n"
        if (line ~ /^# END SHARED/) on = 0
      }
      if (block == "") { print "sync-shared: no shared block in " src > "/dev/stderr"; exit 1 }
    }
    /^# BEGIN SHARED/ { printf "%s", block; skip = 1; next }
    skip && /^# END SHARED/ { skip = 0; next }
    !skip { print }
  ' "$dst" >"$dst.tmp"
  mv "$dst.tmp" "$dst"
  echo "sync-shared: $dst"
done

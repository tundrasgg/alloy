#!/bin/sh
# Test only. A dry run of install.sh and uninstall.sh against archives built
# on this machine, read through ALLOY_INSTALL_TEST_RELEASES instead of GitHub.
# It installs and uninstalls Alloy for real, under /usr/local, /etc/systemd,
# and /var/lib/alloyd, so it refuses to run unless it is told the machine is
# disposable, such as a container:
#
#   docker run --rm -v "$PWD:/src:ro" debian:13 sh -c \
#     'apt-get update -q && apt-get install -yq curl >/dev/null && sh /src/packaging/dry-run.sh --disposable'
#
# Run it as root on Linux. It builds archives for two preview releases with
# make-archive.sh and stamp.sh, from stand-in executables that print the
# version (alloy also answers `node leave`), until the binaries of T-0091
# exist, and checks the install, the second install, the uninstall, the second
# uninstall, interrupted installs and uninstalls, conflicts, digest and entry
# checks, upgrades, --version, the preview rule, and --purge.
#
# When systemd is not the service manager, as in a container, it puts a stub
# systemctl that records its calls at /usr/local/sbin/systemctl and creates
# /run/systemd/system, so the node install runs; it removes both at the end.
# Every check prints PASS or FAIL, and the exit code is 1 when any fails.

set -u
if [ "${1:-}" != --disposable ]; then
  echo "dry-run: this installs and removes Alloy on this machine. Run it only on a disposable machine, with --disposable." >&2
  exit 2
fi
if [ "$(id -u)" != 0 ] || [ "$(uname -s)" != Linux ]; then
  echo "dry-run: run it as root on Linux." >&2
  exit 2
fi

here=$(cd "$(dirname "$0")" && pwd)
work=$(mktemp -d /tmp/alloy-dry-run.XXXXXX)
export SOURCE_DATE_EPOCH=1700000000
failed=0
stubbed=0

pass() { printf 'PASS  %s\n' "$*"; }
fail() {
  printf 'FAIL  %s\n' "$*"
  failed=1
}

case $(uname -m) in
  x86_64 | amd64) arch=x86_64 ;;
  aarch64 | arm64) arch=aarch64 ;;
  *)
    echo "dry-run: unsupported machine $(uname -m)" >&2
    exit 2
    ;;
esac

if [ ! -d /run/systemd/system ]; then
  stubbed=1
  mkdir -p /run/systemd/system /usr/local/sbin
  cat >/usr/local/sbin/systemctl <<'EOF'
#!/bin/sh
# Stub systemctl of packaging/dry-run.sh: records each call.
echo "systemctl $*" >>/tmp/alloy-dry-run-systemctl.log
state=/tmp/alloy-dry-run-systemctl.enabled
case $1 in
  enable) touch "$state" ;;
  disable) rm -f "$state" ;;
  is-enabled) if [ -e "$state" ]; then echo enabled; else echo disabled; exit 1; fi ;;
  is-active) [ "$2" = --quiet ] || echo inactive; exit 3 ;;
  show) if [ -e "/etc/systemd/system/$5" ]; then echo loaded; else echo not-found; fi ;;
esac
exit 0
EOF
  chmod 0755 /usr/local/sbin/systemctl
  PATH="/usr/local/sbin:$PATH"
fi

# build_release <version>: a release with a Linux archive for this machine.
build_release() {
  _v=$1
  _stage="$work/stage-$_v"
  mkdir -p "$_stage/bin" "$_stage/licenses"
  cat >"$_stage/bin/alloy" <<EOF
#!/bin/sh
case "\$1 \${2:-}" in
  'version '*) echo $_v ;;
  'node leave') echo "stand-in alloy: node leave \$*"; exit 0 ;;
  *) exit 1 ;;
esac
EOF
  cat >"$_stage/bin/alloyd" <<EOF
#!/bin/sh
[ "\$1" = version ] && echo $_v
EOF
  echo 'stand-in: no crates' >"$_stage/licenses/crates.txt"
  echo 'stand-in for the Rust standard library notices' >"$_stage/licenses/rust-std-COPYRIGHT-library.html"
  sh "$here/make-archive.sh" "$_v" linux "$arch" "$_stage" "$work/releases/v$_v" >/dev/null &&
    sh "$here/stamp.sh" "$_v" "$work/releases/v$_v" >/dev/null
}

V1=0.0.0-preview.1
V2=0.0.0-preview.2
build_release "$V1" || exit 1
build_release "$V2" || exit 1
export ALLOY_INSTALL_TEST_RELEASES="$work/releases"
# Readable by the unprivileged user of the checks that need root.
chmod -R go+rX "$work"
I1="$work/releases/v$V1/install.sh"
I2="$work/releases/v$V2/install.sh"
U=/usr/local/share/alloy/uninstall.sh

# run <expected exit code> <name> <command>...: the output goes to $work/out.
run() {
  _want=$1
  _name=$2
  shift 2
  "$@" </dev/null >"$work/out" 2>&1
  _rc=$?
  if [ "$_rc" = "$_want" ]; then
    pass "$_name (exit $_rc)"
  else
    fail "$_name (exit $_rc, expected $_want)"
    sed 's/^/      /' "$work/out"
  fi
}
says() {
  if grep -q -- "$1" "$work/out"; then
    pass "  ...and says \"$1\""
  else
    fail "  ...and does not say \"$1\""
    sed 's/^/      /' "$work/out"
  fi
}
gone() {
  for _p in "$@"; do
    if [ -e "$_p" ] || [ -L "$_p" ]; then
      fail "  $_p is still there"
      return
    fi
  done
  pass "  ...and $* are gone"
}
present() {
  for _p in "$@"; do
    if [ ! -e "$_p" ]; then
      fail "  $_p is missing"
      return
    fi
  done
  pass "  ...and $* are there"
}
footprint() {
  gone /usr/local/bin/alloy /usr/local/bin/alloyd /usr/local/share/alloy \
    /usr/local/share/alloy.alloy-new /usr/local/share/alloy.alloy-old /etc/systemd/system/alloyd.service
}

echo "== First install, second install, uninstall, second uninstall"
run 0 "install $V1" sh "$I1"
cat "$work/out"
present /usr/local/bin/alloy /usr/local/bin/alloyd /usr/local/share/alloy/manifest /usr/local/share/alloy/uninstall.sh /etc/systemd/system/alloyd.service
echo "-- manifest:"
cat /usr/local/share/alloy/manifest
cp /usr/local/share/alloy/manifest "$work/manifest.first"
if [ -f /tmp/alloy-dry-run-systemctl.enabled ]; then fail "  the unit was enabled"; else pass "  ...and the unit is disabled"; fi
run 0 "install $V1 again" sh "$I1"
says "already installed"
if cmp -s /usr/local/share/alloy/manifest "$work/manifest.first"; then pass "  ...and the manifest is unchanged"; else fail "  the manifest changed"; fi
run 0 "uninstall" sh "$U"
cat "$work/out"
footprint
run 0 "uninstall again, through install.sh --uninstall" sh "$I1" --uninstall
says "not installed"

echo "== Not root"
run 4 "install as an unprivileged user without sudo" setpriv --reuid=65534 --regid=65534 --clear-groups sh "$I1"
says "needs root"
gone /usr/local/share/alloy
run 4 "install.sh --uninstall as an unprivileged user" setpriv --reuid=65534 --regid=65534 --clear-groups sh "$I1" --uninstall

echo "== An install interrupted before the manifest exists (T-0088 review F3)"
mkdir /usr/local/share/alloy.alloy-new
printf 'alloy-manifest 1\nversi' >/usr/local/share/alloy.alloy-new/manifest
run 0 "install over the marker of an interrupted install" sh "$I1"
gone /usr/local/share/alloy.alloy-new
present /usr/local/share/alloy/manifest
run 0 "uninstall" sh "$U"
footprint
mkdir /usr/local/share/alloy.alloy-new
run 0 "uninstall with only the marker of an interrupted install" sh "$I1" --uninstall
says "left by an interrupted install"
gone /usr/local/share/alloy.alloy-new
mkdir /usr/local/share/alloy.alloy-old
: >/usr/local/share/alloy.alloy-old/manifest
run 0 "uninstall with only the marker of an interrupted uninstall" sh "$I1" --uninstall
gone /usr/local/share/alloy.alloy-old

echo "== An install interrupted after the manifest exists"
run 0 "install $V1" sh "$I1"
sed -i 's/^state complete$/state pending/' /usr/local/share/alloy/manifest
rm /usr/local/bin/alloyd
cp /usr/local/bin/alloy /usr/local/bin/alloy.alloy-new
run 0 "install again completes it" sh "$I1"
present /usr/local/bin/alloyd
gone /usr/local/bin/alloy.alloy-new
if grep -qx 'state complete' /usr/local/share/alloy/manifest; then pass "  ...and the manifest says complete"; else fail "  the manifest is not complete"; fi
sed -i 's/^state complete$/state pending/' /usr/local/share/alloy/manifest
cp /usr/local/bin/alloy /usr/local/bin/alloyd.alloy-new
run 0 "uninstall of an interrupted install" sh "$U"
footprint
gone /usr/local/bin/alloyd.alloy-new

echo "== Conflicts"
printf '#!/bin/sh\necho someone else\n' >/usr/local/bin/alloy
chmod 0755 /usr/local/bin/alloy
run 5 "install where another alloy is" sh "$I1"
says "/usr/local/bin/alloy"
gone /usr/local/share/alloy
rm /usr/local/bin/alloy
mkdir /run/alloy
run 5 "install where /run/alloy exists" sh "$I1"
rmdir /run/alloy
mkdir -m 0755 /var/lib/alloyd
run 5 "install where /var/lib/alloyd is not an Alloy data directory" sh "$I1"
says "not an Alloy data directory"
rmdir /var/lib/alloyd

echo "== Digests and archive entries"
mkdir -p "$work/tampered/v$V1"
cp "$work/releases/v$V1"/* "$work/tampered/v$V1/"
printf 'x' >>"$work/tampered/v$V1/alloy-$V1-linux-$arch.tar.gz"
run 1 "install from an archive whose digest does not match" env ALLOY_INSTALL_TEST_RELEASES="$work/tampered" sh "$I1"
says "SHA-256"
gone /usr/local/share/alloy
V3=0.0.0-preview.3
top="alloy-$V3-linux-$arch"
mkdir -p "$work/bad/$top/bin" "$work/releases/v$V3"
ln -s /bin/sh "$work/bad/$top/bin/alloy"
(cd "$work/bad" && tar -czf "$work/releases/v$V3/$top.tar.gz" "$top")
sh "$here/stamp.sh" "$V3" "$work/releases/v$V3" >/dev/null
run 1 "install from an archive with a symbolic link" sh "$work/releases/v$V3/install.sh"
says "link or a device"
rm -rf "$work/releases/v$V3" "$work/bad"
mkdir -p "$work/bad/x" "$work/releases/v$V3"
echo evil >"$work/bad/x/evil"
(cd "$work/bad" && tar --transform "s,^x,$top/..," -czf "$work/releases/v$V3/$top.tar.gz" x)
sh "$here/stamp.sh" "$V3" "$work/releases/v$V3" >/dev/null
run 1 "install from an archive with a .. entry" sh "$work/releases/v$V3/install.sh"
says "leaves its directory"
gone /usr/local/share/alloy
run 1 "install --version of a release that does not exist" sh "$work/releases/v$V3/install.sh" --version 0.0.0-preview.9
says "There is no release 0.0.0-preview.9"
rm -rf "$work/releases/v$V3"
mkdir -p "$work/releases/v$V3"
other=macos-aarch64
[ "$arch" = x86_64 ] || other=macos-x86_64
echo 'not this platform' >"$work/releases/v$V3/alloy-$V3-$other.tar.gz"
sh "$here/stamp.sh" "$V3" "$work/releases/v$V3" >/dev/null
run 1 "install of a release with no build for this platform" sh "$work/releases/v$V3/install.sh"
says "has no build for linux-$arch"
rm -rf "$work/releases/v$V3"

echo "== Upgrade, --version, and the uninstall script missing"
run 0 "install $V1" sh "$I1"
run 0 "install $V2 over $V1" sh "$I2"
if grep -qx "version $V2" /usr/local/share/alloy/manifest; then pass "  ...and the manifest names $V2"; else fail "  the manifest does not name $V2"; fi
run 0 "install.sh of $V2 with --version $V1" sh "$I2" --version "$V1"
says "Alloy $V1 for linux"
if [ "$(/usr/local/bin/alloy version)" = "$V1" ]; then pass "  ...and alloy version prints $V1"; else fail "  alloy version does not print $V1"; fi
rm "$U"
run 0 "install.sh --uninstall with the uninstall script missing" sh "$I1" --uninstall
footprint

echo "== Data directory: the preview rule, resuming, and --purge"
mkdir -m 0700 /var/lib/alloyd
printf 'alloy-host 1\njoin joined\nrelease %s\n' "$V1" >/var/lib/alloyd/alloy-host
chmod 0600 /var/lib/alloyd/alloy-host
rm -f /tmp/alloy-dry-run-systemctl.enabled
run 5 "install $V2 on a node of $V1" sh "$I2"
says "changes release only by an uninstall with --purge"
run 0 "install $V1 on a node of $V1 after an uninstall without --purge" sh "$I1"
says "resumes"
if [ -f /tmp/alloy-dry-run-systemctl.enabled ]; then pass "  ...and the unit is enabled"; else fail "  the unit is not enabled"; fi
run 0 "uninstall without --purge keeps the data directory" sh "$U"
says "Kept the data directory /var/lib/alloyd"
present /var/lib/alloyd/alloy-host
run 5 "uninstall --purge --yes of a node with alloy no longer installed" sh "$I1" --uninstall --purge --yes
present /var/lib/alloyd/alloy-host
run 2 "uninstall --purge with no terminal and no --yes" sh "$I1" --uninstall --purge
run 0 "uninstall --purge --yes --force" sh "$I1" --uninstall --purge --yes --force
gone /var/lib/alloyd
mkdir -m 0700 /var/lib/alloyd
printf 'alloy-host 1\njoin joined\nrelease %s\n' "$V1" >/var/lib/alloyd/alloy-host
chmod 0600 /var/lib/alloyd/alloy-host
run 0 "install $V1 on a node of $V1" sh "$I1"
run 0 "uninstall --purge --yes runs alloy node leave" sh "$U" --purge --yes
says "stand-in alloy: node leave"
gone /var/lib/alloyd
footprint
mkdir -m 0700 /var/lib/alloyd
printf 'alloy-host 1\njoin joined\nrelease none\nlink 0\n' >/var/lib/alloyd/alloy-host
chmod 0600 /var/lib/alloyd/alloy-host
run 1 "install with a host file that cannot be parsed" sh "$I1"
says "cannot be read"
rm -rf /var/lib/alloyd

if [ "$stubbed" = 1 ]; then
  echo "== Without systemd: the cli kind"
  rm -f /usr/local/sbin/systemctl
  rmdir /run/systemd/system 2>/dev/null
  run 0 "install $V1 without systemd" sh "$I1"
  says "alloyd needs systemd"
  present /usr/local/bin/alloy
  gone /usr/local/bin/alloyd /etc/systemd/system/alloyd.service
  run 0 "uninstall" sh "$U"
  footprint
  echo "-- the stub systemctl was called with:"
  sort /tmp/alloy-dry-run-systemctl.log | uniq -c
  rm -f /tmp/alloy-dry-run-systemctl.log /tmp/alloy-dry-run-systemctl.enabled
  rmdir /usr/local/sbin 2>/dev/null
fi

rm -rf "$work"
if [ "$failed" = 0 ]; then
  echo "dry-run: every check passed"
else
  echo "dry-run: some checks failed"
fi
exit "$failed"

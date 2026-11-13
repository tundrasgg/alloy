# Packaging

The release assets of ARFC 0017 clauses 3.13 to 3.16, and the tools that build
and check them. `.github/workflows/release.yml` runs these when a tag `v*` is
pushed.

| File | What |
|---|---|
| `install.sh` | Install script for Linux and macOS, run as `curl -fsSL <url> \| sh`. A template: the release workflow fills in its version and archive digests |
| `install.ps1` | Install script for Windows, run as `irm <url> \| iex`. A template like `install.sh` |
| `uninstall.sh`, `uninstall.ps1` | The uninstall scripts that go into the archives |
| `alloyd.service` | The systemd unit that `install.sh` writes for a node. `check.sh` checks that the copy inside `install.sh` is the same |
| `set-version.sh` | Sets the version of the `alloy` and `alloyd` crates to the release's version before the build |
| `collect-licenses.sh` | Gathers the license texts of everything linked into the binaries |
| `make-archive.sh` | Assembles one archive in the layout of clause 3.13 |
| `stamp.sh` | Writes the digests into the install scripts and writes `SHA256SUMS` |
| `check.sh`, `check.ps1` | Checks the scripts: syntax, shellcheck, the PowerShell parser, that a cut script runs nothing, usage errors |
| `check-removal.ps1` | Checks the uninstall's removal of directories that hold other files, and its recovery of an interrupted move, against a temporary directory. `check.sh` runs it |
| `sync-shared.sh` | Copies the shared block of each install script into its uninstall script |
| `dry-run.sh` | Test only: installs and uninstalls for real against locally built archives, on a disposable Linux machine |

## Editing the scripts

Each install script holds the whole uninstall between `# BEGIN SHARED` and
`# END SHARED`, so that `install.sh --uninstall` works when the installed
uninstall script is missing. Edit that block in `install.sh` or `install.ps1`,
then run `sh packaging/sync-shared.sh`, then `sh packaging/check.sh`.

The scripts are ASCII with LF line ends. Every top-level command before the
last line only defines a function or sets a variable, so a script cut short in
transfer runs nothing (clause 3.15). `check.sh` checks every prefix.

## Test only: ALLOY_INSTALL_TEST_RELEASES

When the environment variable `ALLOY_INSTALL_TEST_RELEASES` names a directory,
the install scripts read each release asset from
`<directory>/v<version>/<asset>` instead of downloading it from
`https://github.com/tundrasgg/alloy/releases/download/v<version>/<asset>`. The
digests written into the script are still checked, so only archives built with
the same `stamp.sh` run install. It exists for `dry-run.sh` and other dry runs
on disposable machines. User documentation never mentions it, and it is never
set on a machine that matters.

## Dry run

On a disposable Linux machine, as root, from the repository root:

```
docker run --rm -v "$PWD:/src:ro" debian:13 sh -c \
  'apt-get update -q && apt-get install -yq curl >/dev/null && sh /src/packaging/dry-run.sh --disposable'
```

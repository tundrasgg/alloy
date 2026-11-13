# ARFC 0017: Upgrades and versioning

- Status: accepted
- Depends on: 0000, 0002, 0003, 0006, 0008, 0019
- Crates: alloyd, alloy-state, alloy (binary)
- Decisions: 0029, 0044, 0056, 0057, 0059, 0062

## 1. Purpose

A cluster is upgraded one node at a time, while running, without stopping guests.
This ARFC defines the version rules that make that possible, and how a release
is published, checked, installed on a machine, and removed from it again, leaving
everything else the machine runs as it was.

## 2. Model

Releases are `MAJOR.MINOR.PATCH`. Four things are versioned independently and
each carries its version explicitly:

| Thing | Where the version is | Compatibility rule |
|---|---|---|
| AL wire protocol | `version` byte in every message | A node speaks its own version and the one before |
| Internal RPC between nodes | Capability set in the node record | Features are used only when both ends have them |
| Control plane state | Schema version per resource kind | Readable and writable by the previous minor release |
| On-disk formats | Header of each format | Readable by the previous minor release until finalized |

A **capability** is a named feature a binary supports. A **gate** is a cluster
setting that turns a capability's use on.

## 3. Behavior

3.1 Nodes running minor release N and N+1 must interoperate fully (I-18). Skipping
a minor release is refused by `alloyd` at startup.

3.2 A new capability stays unused until every node in the cluster reports it. Its
gate then opens automatically, or on a cloud admin's command when the gate is
marked manual.

3.3 State and disk format changes are two-step. Release N+1 reads and writes the
old format and can read the new one. The new format is written only after the
upgrade is **finalized**: the switch of ARFC 0001 clause 3.13 records N+1 as the
finalized release, and finalization starts there. Until that switch, every node
can be rolled back to N.

3.4 `alloy cluster upgrade` drives a rolling upgrade: control nodes first, one at
a time, waiting for group health between each, then other nodes in batches no
larger than a configured share of each zone, default 10%. It pauses on any node
that fails its health check after upgrade.

3.5 Upgrading a node replaces `alloyd` and restarts it. `alloy-net`, `alloy-blk`,
and `alloy-store` are replaced by handover: the new process starts, takes over
sockets and device connections from the old one by passing file descriptors and
state, and the old one exits. VMM processes are not restarted by an upgrade, and a
guest picks up a new VMM version at its next restart or migration.

3.6 During handover a guest must see no network gap over 100 ms and no I/O error
(I-17).

3.7 `alloy cluster upgrade --finalize` finalizes the upgrade to release N+1 and
writes the new formats, including the check, the switch, and the rewrite of
stored seam bindings in ARFC 0001 clause 3.13. It refuses while any node's
running release (clause 3.11) is not N+1, and it reports success only when that
finalization is complete. Running it again resumes a finalization that is
started and not complete. `alloy cluster upgrade` to release N+2 refuses to start,
and `alloyd` of release N+2 refuses to start, while the finalization of N+1 is not
complete.

3.8 Release artifacts are signed. `alloyd` verifies the signature against the
release key compiled into the running binary before replacing itself. This
binds every release that is not a preview (clause 3.13). A preview release has
no release key and no release signature: its archives are checked by digest
(clause 3.14), its `alloyd` never replaces itself, and `alloy cluster upgrade`
refuses, naming the preview release, while any node runs one. A machine on a
preview release changes release only by an install (clause 3.15).

3.9 Patch releases change no format, capability, or API.

3.10 CI runs every simulation scenario in a mixed-version mode once a previous
minor release exists: half the nodes on N, half on N+1.

3.11 Running release. The global group (ARFC 0006 section 2.2) stores with each
node record the release its `alloyd` runs, the node's **running release**. It is
control plane state, not a field of the signed record of ARFC 0002 section 2.
Each node also keeps two releases in node-local state that survives an `alloyd`
restart and is lost when the node is reinstalled: its **started release**, the
release of the last `alloyd` that started on it, and its **applied release**, the
release of the last running release write applied for it. The started release
is the `release` line of the node's host file (section 4.2).

At each start, `alloyd` reads both, applies the preview rule of clause 3.13,
then sets the started release to its own release before it does anything else. If its own release equals both values it
read, it proceeds without writing. Otherwise it writes its running release, and
until that write is applied it serves no API call, runs no controller, applies
no view change, and decodes no stored binding. Once the write is applied it sets
the applied release to its own. The write is refused when the release is older
than the finalized release. `alloyd` then does none of those things and reports
the refusal, naming both releases. Guests keep running (I-17). So an `alloyd`
that proceeds without writing runs the release last written for its node by an
`alloyd` that started on it, and every `alloyd` whose release differs from the
node's running release has written its running release before it serves.

`alloyd` watches its own node record, and while it serves, if the running release
recorded there is not its own, it writes its running release again, with the
same refusal. `alloy cluster recover` (ARFC 0006 clause 3.11) clears every
node's running release, and clause 3.7 counts a cleared running release as not
N+1.

A restart at the same release, and a resume of a view subscription after a
disconnect (ARFC 0006 clause 3.7), therefore write nothing to the global group,
and ARFC 0006 clause 3.9 holds for them while the global group has no quorum. An
`alloyd` that has to write, which is an upgrade, a fallback, or a start after the
node-local state was lost, serves nothing, not even the node's last view, while
the global group has no quorum (ARFC 0006 section 6), until quorum returns. This
is the one exception to ARFC 0006 clause 3.9. Clause 3.4 already waits for group
health between nodes, so a rolling upgrade is not slowed by it.

3.12 Finalization suite. CI runs `tests/conformance/upgrade/` on the simulator
with three builds of the current tree that report releases R-1, R, and R+1. Each
build registers a test implementation `t`, defined in the suite, whose
configuration has `VERSION` 1, 2, and 3 and `MIN_VERSION` 1, 1, and 2, whose
format 2 encoding of a configuration is longer than its format 1 encoding, and
whose release R decoder rejects one field value that the release R-1 decoder
accepts. The R-1 build also registers a second test implementation `u`, with a
configuration type, that the R and R+1 builds do not register. Starting from a
cluster created at R-1 and upgraded to R, its scenarios check each rule of ARFC
0001 clause 3.13:

- A stored binding of `t` that release R cannot decode, one whose format 2
  encoding exceeds 65,536 bytes, and a stored binding of `u` each stop
  finalization before the switch with the binding's resource and error, and
  rollback to R-1 then succeeds.
- Before the switch, a writer of release R refuses with `TooLarge`, and stores
  nothing, a configuration of `t` whose format 1 encoding fits in 65,536 bytes
  and whose format 2 encoding does not.
- A binding write of `t` that release R cannot decode, proposed by an `alloyd`
  of R-1 before the check begins and delivered to its group after the check's
  write to that group, is refused, and finalization then completes.
- A binding write delayed across the check's write by a writer of release R
  succeeds after one retry. A binding write that races the switch succeeds after
  one re-encode.
- A node that falls back to R-1 after the check begins makes the switch refuse.
  After the switch, starting `alloyd` of R-1 and falling back to R-1 are refused,
  and an `alloyd` of R-1 that started before the switch and never wrote its
  running release serves nothing.
- With the global group without quorum, an `alloyd` restarted at its node's
  running release serves and applies view changes from a healthy regional group,
  and an `alloyd` of another release serves nothing until quorum returns.
- A region with no recorded regional group does not delay completion. Regional
  groups recorded before the upgrade, during it, between the check and the
  switch, and after the switch hold the write versions of R-1 before the switch
  and those of R after their own switch. A group whose record precedes the
  switch and whose first entry commits after finalization's switch write to it
  holds the write versions of R. A recorded regional group without quorum keeps
  finalization incomplete and is reported with its region, and finalization
  completes when run again after its quorum returns.
- A writer that changes a binding during the rewrite keeps its change.
- The rewrite causes no swap, drain, or restart, and leaves every slot's
  `current`, `binding`, and `generation` unchanged.
- The upgrade to R+1, and `alloyd` of R+1, are refused until completion and
  accepted after it.
- A finalization interrupted by a crash, and one interrupted by a leader change,
  complete when run again, and rewrite no binding twice.

3.13 Releases. A release's git tag and the name of its GitHub release are `v`
followed by its version. A **preview release** has the version
`0.0.PATCH-preview.N`, with `N` a decimal number from 1, and is marked
pre-release on GitHub. A preview makes no compatibility promise: clauses 3.1,
3.3, 3.9, and 3.10 do not bind it. Every node of a cluster that has a preview
node runs the same preview release, and such a cluster never changes release:

- `join` admits a machine to a cluster where it or any node runs a preview only
  when every node runs exactly its release (ARFC 0002 section 3.3 step 5).
- An install refuses to change the release of a node that is `joined` or
  `joining` when either release is a preview (clause 3.15 step 4).
- An `alloyd` that starts on a node whose host file (section 4.2) says `join
  joining` or `join joined` and names a started release other than its own,
  when either release is a preview, does not set the started release. It
  serves no API call, runs no controller, opens no AL session, applies no view
  change, starts no process, performs no handover, and writes nothing to the
  cluster or the data directory. It logs the refusal every 60 seconds, naming
  both releases. Guests keep running. Installing the node's started release
  again ends the refusal.
- A cluster moves to another preview release, or from a preview to a release
  that is not a preview, only by being created again: each machine is
  uninstalled with `--purge` (clause 3.16), which deletes its node and the
  guests and data on it, and the new release is installed and runs `init` and
  `join`.

Each release carries these assets on its GitHub release (ADR 0044), and no
others:

| Asset | Content |
|---|---|
| `alloy-<version>-linux-<arch>.tar.gz` | Archive for Linux |
| `alloy-<version>-macos-<arch>.tar.gz` | Archive for macOS |
| `alloy-<version>-windows-<arch>.zip` | Archive for Windows |
| `install.sh`, `install.ps1` | The install scripts of clause 3.15 |
| `SHA256SUMS` | The digests of clause 3.14 |

`<arch>` is `x86_64` or `aarch64`. A release that is not a preview carries all
six archives. A preview may leave any of them out.

An archive holds one directory, `alloy-<version>-<os>-<arch>/`, and in it:

- `bin/`: the executables. Every archive has `alloy` (`alloy.exe` on Windows).
  The Linux archives also hold `alloyd` and every other executable of the
  processes ARFC 0000 section 2.3 lists. The Windows archives also hold
  `alloyd.exe`, `alloy-net.exe`, and `wintun.dll` under ADR 0062. The macOS
  archives hold only `alloy` in this version.
- `uninstall.sh` (Linux and macOS) or `uninstall.ps1` (Windows): clause 3.16.
- `licenses/`: the license texts of Alloy and of everything else in the
  archive, `licenses/wintun.txt` among them in the Windows archives.

No entry is a symbolic link, a hard link, or a device, and no entry name is
absolute or has a `..` component.

3.14 Release integrity. The release workflow builds the archives, computes the
SHA-256 of each, writes those digests into both install scripts of the same
release, and only then publishes. `SHA256SUMS` has one line per other asset,
ordered by asset name in byte order: 64 lower case hexadecimal digits, two
spaces, the asset name, and a line feed. Every asset, `SHA256SUMS` included,
carries a GitHub artifact attestation of build provenance made by the release
workflow, so that `gh attestation verify <asset> --repo tundrasgg/alloy`
succeeds for it. User documentation gives that command. The install scripts
need neither `gh` nor `SHA256SUMS`: they check each archive against the digest
written into them, before reading any entry of it.

A script fetched from a URL of ADR 0044 is trusted as far as TLS to github.com
and the right to publish the repository's releases are. The written digests
guarantee that a script installs exactly the archives released with it, so the
script and the files it installs always come from one release.

The first release that is not a preview must add a release key: an Ed25519 key
whose public key is compiled into `alloyd`, and an asset `SHA256SUMS.sig` holding
the 64 byte Ed25519 signature (RFC 8032) by that key over the exact bytes of
`SHA256SUMS`, with an attestation like every other asset. `alloyd` then verifies
`SHA256SUMS.sig`, and the digest it lists for the archive, before replacing
itself (clause 3.8). The install scripts keep checking digests only, since a key
fetched together with the script adds nothing for a first install.

3.15 Install. An install runs `install.sh` on Linux and macOS, as
`curl -fsSL <url> | sh`, and `install.ps1` on Windows, as `irm <url> | iex`,
with a URL of ADR 0044. GitHub's latest release is never a pre-release, so
while every release is a preview, the `latest` URLs find no release and a
preview is installed from its pinned URL,
`https://github.com/tundrasgg/alloy/releases/download/v<version>/install.sh` or
`.../install.ps1`. A script installs the release it belongs to.

`install.sh` takes `--version <version>`, which makes it download the
`install.sh` of that release from its pinned URL and run it with the other
arguments instead, and `--uninstall`, `--purge`, `--force`, and `--yes` (clause
3.16). `install.ps1` takes `-Version <version>`, `-Uninstall`, `-Purge`,
`-Force`, and `-Yes` with the same meanings, when it is run with parameters. Any
other argument, or an uninstall flag without `--uninstall`, is a usage error.

A script runs nothing until it has been read to its end. In `install.sh` every
top-level command before the last line only defines a function or sets a shell
option or variable, and the last line is the one command that starts the work,
written so that a shell that meets the end of its input before the last byte of
that command runs none of it, as a brace group such as `{ main "$@"; }` is.
`install.ps1` follows the same rule, so that `Invoke-Expression` given any
prefix of it runs nothing. Each script, cut at any byte and run with any
arguments, changes nothing on the machine.

The script runs these steps in order. A failing step stops the script, which
removes its temporary directory and exits with the code given. Steps 1 to 5
change nothing on the machine.

1. Platform. The operating system is `linux`, `macos`, or `windows`, and the
   architecture is the native one of the operating system, not of the shell:
   `x86_64` (also reported as `amd64`) or `aarch64` (also `arm64`). Anything
   else exits 1. The script prints `Alloy <version> for <os>-<arch>` and picks
   the install kind:

   | Kind | When | Installs |
   |---|---|---|
   | `node` | Linux 6.1 or later, with systemd as the service manager (`/run/systemd/system` is a directory) | `alloy`, `alloyd` and the other executables, the unit `alloyd.service` |
   | `client` | Windows build 17763 or later | `alloy`, the mesh client (`alloyd`, `alloy-net`, `wintun.dll`), the service `alloyd` |
   | `cli` | macOS, and Linux that does not qualify as `node` | `alloy` only. The script prints why `alloyd` was left out |

2. Privileges. Every kind needs root, or on Windows an elevated administrator.
   `install.sh` run by another user runs each step that changes the machine
   through `sudo`, and exits 4 when `sudo` is missing or refuses. `install.ps1`
   not elevated exits 4.
3. Tools. `install.sh` needs `uname`, `mktemp`, `tar`, `gzip`, `curl` or `wget`,
   and `sha256sum` or `shasum`. `install.ps1` needs PowerShell 5.1 or later. A
   missing tool exits 1, naming it.
4. Conflicts. The script exits 5, naming what it found, in each of these
   cases:
   - There is no manifest (section 4.1) on the machine, and an object that
     clause 3.17 reserves by name exists, other than an Alloy data directory
     (ARFC 0002 section 3.12), or a path that step 6 would write exists, its
     temporary names included.
   - There is a manifest, and a path that step 6 would write exists and is
     neither in the manifest nor a temporary name of a path in it.
   - The data directory is an Alloy data directory whose host file (section
     4.2) says `join joining` or `join joined` and names a started release
     other than this one, and either release is a preview (clause 3.13). The
     message names both releases and says that the node changes release only
     by an uninstall with `--purge` and a new `init` or `join`.
5. Download. When the manifest says `state complete`, names this version and
   kind, every file it lists has its recorded digest, and, for `node` and
   `client`, the service is present, the script prints that this release is
   already installed and exits 0 without downloading. Otherwise it downloads
   the archive for the platform from the pinned URL into a new temporary
   directory, checks its SHA-256 against the written digest, and exits 1 on a
   mismatch, naming the archive and both digests. It then checks every entry
   against clause 3.13 and exits 1 on the first that breaks it, then extracts.
6. Place. The script writes the manifest with `state pending`, listing every
   path of the installed release, if any, and of this one. Each path has two
   **temporary names**, the path followed by `.alloy-new` and by `.alloy-old`.
   The script writes each file as its `.alloy-new` name and renames it over the
   target. When the target cannot be replaced because it is running, as on
   Windows, it first renames the target to its `.alloy-old` name. It then
   removes every path of the earlier manifest that this release does not have,
   renaming one it cannot remove to its `.alloy-old` name, and removes every
   temporary name of a manifest path that exists and is not in use. It rewrites
   the manifest with `state complete`, listing this release's entries and every
   earlier path whose `.alloy-old` name is still in use. The temporary names of
   manifest paths are the only paths outside its temporary directory that the
   script writes and the manifest does not list, and every later install and
   every uninstall removes those that exist. The paths are:

   | What | Linux and macOS | Windows |
   |---|---|---|
   | The files of `bin/` | `/usr/local/bin/` | `%ProgramFiles%\Alloy\` |
   | `licenses/` and the uninstall script | `/usr/local/share/alloy/` | `%ProgramFiles%\Alloy\` |
   | Manifest | `/usr/local/share/alloy/manifest` | `%ProgramFiles%\Alloy\manifest` |
   | Service (`node`, `client`) | `/etc/systemd/system/alloyd.service` | Windows service `alloyd` |
   | Search path | Unchanged | `%ProgramFiles%\Alloy` appended to the machine `PATH` when absent |

   A directory the install creates, such as `/usr/local/bin` where it was
   missing, is listed in the manifest. `%ProgramFiles%` and `%ProgramData%` are
   the folders Windows reports for them.
7. Service, for `node` and `client`. The unit or service runs `alloyd` from the
   path above, as root or as the Windows `LocalSystem` account, starts it again
   5 seconds after it exits abnormally, and, on Linux, orders it after
   `network-online.target`. Stopping
   or restarting it stops `alloyd` alone, never a process that ARFC 0000
   section 2.3 says survives an `alloyd` restart. On a machine with no earlier
   install, the unit is disabled and not started, or the service's start type
   is `Disabled`: `init` and `join` enable and start it (ARFC 0002). When the
   data directory is an Alloy data directory whose host file says `join joined`
   or `join joining`, as after an uninstall without `--purge`, the script
   enables and starts it instead, and says that the node resumes. On a machine
   with an earlier install the enablement is kept, and when `alloyd` was
   running and a file changed, `alloyd` is restarted, and replaces the
   surviving processes by handover (clause 3.5).
8. Check. The script runs the installed `alloy version` and exits 1 unless it
   prints this version. It then prints what it installed and where, and the
   state of the service. When a directory of the search path other than the
   install's holds another executable named `alloy`, it prints that
   executable's path and which of the two the search path finds first. It
   changes neither. Its last line names the next command: `sudo alloy
   init` or `sudo alloy join <join token>` for `node`, `alloy join <join token>`
   from an elevated PowerShell, and `alloy login <login code>`, for `client`,
   and `alloy login <login code>` for `cli`.

An install creates no user, group, interface, address, route, or firewall
state, and creates no data directory: `init` and `join` create it. It starts no
process except `alloy version` and the service starts of step 7.

Running a script again is safe. A second run after a successful install changes
nothing and exits 0. An install interrupted at any point leaves a machine on
which running the same script again completes it, and on which an uninstall
removes everything.

Each download must connect within 10 seconds and fails when no byte arrives for
30 seconds. A failed download is tried again 2 seconds later, at most 3 attempts
in all. The script exits 6 when no attempt got an HTTP response, and 1 on an HTTP
status of 400 or more, which, for an archive, it reports as this release having
no build for the platform. Exit codes follow ARFC 0019 clause 3.1.7: 0 success,
1 failure, 2 usage error, 4 not permitted, 5 conflict, 6 unavailable.
`install.ps1` run through `Invoke-Expression` never calls `exit`: it ends a
failure with a terminating error whose message is the one it prints, and leaves
the PowerShell session open. Run as a file, it exits with those codes.

3.16 Uninstall. These commands uninstall, as root or from an elevated
PowerShell:

| Where | Command |
|---|---|
| Linux, macOS | `sh /usr/local/share/alloy/uninstall.sh [--purge] [--force] [--yes]` |
| Windows | `powershell -NoProfile -ExecutionPolicy Bypass -File "$env:ProgramFiles\Alloy\uninstall.ps1" [-Purge] [-Force] [-Yes]` |

`install.sh --uninstall` and `install.ps1 -Uninstall` do the same. An uninstall
reads only the manifest, the host file (section 4.2), and clause 3.17, whose
formats every release shares, so the uninstall of any release removes the
install of any release. The data directory in this clause and in clause 3.15
is the one `init` or `join` used, as the installed `alloyd` reports it, else the
default of ARFC 0002 section 3.3. An uninstall removes only what it can show
Alloy created: the manifest's entries and their temporary names, the objects
the host file lists that still match it (section 4.2), and an Alloy data
directory (ARFC 0002 section 3.12). It removes nothing else, whatever its
name, and prints each object it finds with a reserved name that it left in
place. It runs these steps in order:

1. Not root, or not elevated: exits 4, changing nothing.
2. With no manifest and no Alloy data directory, it prints that Alloy is not
   installed and exits 0. A directory at the data directory's path that is not
   an Alloy data directory is left as it is, and the uninstall says so. With an
   Alloy data directory and no manifest, it skips step 7.
3. When a VMM runs on the machine, it exits 5, naming how many, changing
   nothing, unless `--force` is given. A VMM is a process whose user id is in
   the guest id range of clause 3.17.7.
4. With `--purge`, it asks for confirmation as ARFC 0019 clause 3.1.6 says,
   reading the controlling terminal when standard input is not one, and with
   no terminal it exits 2 unless `--yes` is given. When the host file says
   `join joined` or `join joining`, it then runs `alloy node leave --yes`
   (ARFC 0002 section 3.9). When that fails, the uninstall exits with its exit
   code, changing nothing more, and when `alloy` is no longer installed, so that
   `leave` cannot run, it exits 5. With `--force` it goes on in both cases, and
   the node record lasts until it expires or a cloud admin deletes it.
5. It stops and disables the service, then stops every process run from an
   executable the manifest lists, and every VMM: `alloyd`, the processes of
   ARFC 0000 section 2.3, and VMMs. Each gets a termination request and is
   killed after 30 seconds.
6. It removes each object that a line of the host file lists and that matches
   the line (section 4.2), interfaces with their addresses and routes, and
   drops each line, rewriting the host file as section 4.2 says. It never
   removes the Wintun driver (clause 3.17).
7. It removes the manifest's entries in reverse order, each path with its
   temporary names: files, then directories that are empty, the unit, after
   which systemd reloads its units, the Windows service, and the search path
   entry. It removes the manifest last.
8. Without `--purge` it keeps the data directory, and prints its path, its size,
   that it holds the node's keys and state, and the command that removes it.
   With `--purge` it removes the data directory.

An uninstall never removes a user's CLI configuration or the access keys the
CLI stored in the keychain (ARFC 0019 clause 3.1.9), since the CLI made them for
that user and not the install, and it says where they are. It does not remove
system log entries. It prints what it removed and exits 0. Running it again is
safe: a second run removes nothing and exits 0, at step 2 after a run with
`--purge`, and an interrupted uninstall is completed by running it again.

The `--uninstall` form of an install script also works when the installed
uninstall script is damaged or missing, since it needs only the manifest.

3.17 Host footprint and coexistence. Alloy shares each machine with whatever
the machine already runs. It owns these objects and no others, the **reserved**
objects:

| Kind | Linux | Windows |
|---|---|---|
| Network interfaces | Names of `al` then a digit, and of `alv` then a digit (ARFC 0009 section 2) | Adapters named `al` then a digit, all of them Wintun adapters |
| Addresses and routes | Only on its own interfaces. Routes only in the `main` table. No policy routing rules | Only on its own adapters |
| Firewall | `nftables` tables, in any family, named `alloy` or starting with `alloy_`. No `iptables` or `ip6tables` rule or chain, on either backend | None. No Windows Firewall rule |
| Kernel parameters | Only those of its own interfaces, under `net.ipv4.conf.<interface>` and `net.ipv6.conf.<interface>` | None |
| Named network namespaces | Names starting with `alloy-` | None |
| Service manager | The unit `alloyd.service`, and units whose names start with `alloyd-` | The service `alloyd` |
| Users and groups | No user or group entry. The guest id range of clause 3.17.7 | None |
| Drivers | None | None. When Alloy creates a Wintun adapter, the Wintun driver is installed if it is not. Alloy never removes the Wintun driver, since other products load it (ADR 0062) |
| Files | The manifest's paths and their temporary names (clause 3.15 step 6), the data directory, `/run/alloy/` | The manifest's paths and their temporary names, the search path entry, the data directory |

The names that Grafana Alloy, a common host agent, uses on Linux are not
reserved: the unit `alloy.service`, the user and group `alloy`, and the
directories `/etc/alloy` and `/var/lib/alloy`. Alloy never creates, changes, or
removes them.

3.17.1 Alloy creates, changes, and deletes only reserved objects, and of those
only the ones it can show it created (clause 3.17.2). It never changes an
interface, bridge, address, route, routing rule, firewall table or chain,
kernel parameter, name resolution setting (`/etc/resolv.conf`,
`systemd-resolved`, NetworkManager, or Windows DNS client settings), service,
driver, user, group, or file that is not reserved, other than installing the
Wintun driver as the table says, and never adds a rule or chain to a table that
is not reserved.

3.17.2 Alloy records every host object it creates outside the manifest in the
host file of its data directory (section 4.2) before creating it. It treats a
reserved object as its own only while a line of the host file matches it
(section 4.2), or a line of the manifest lists it. A reserved name held by any
other object is a conflict. The install (clause 3.15 step 4), `init`, and
`join` (ARFC 0002 section 3.8) refuse it with exit 5, naming the object, and
change nothing. An `alloyd` or `alloy-net` that meets one later leaves the
object as it is and reports it in the node's status, naming it. No command
changes, reuses, or removes such an object.

3.17.3 Alloy assigns an address to its interface, or adds a route through it,
only for a prefix that overlaps no route in any routing table, other than a
route of prefix length 0, and no address on an interface that is not reserved.
`init` and `join` check this for the infrastructure prefix (ARFC 0002 section
3.8). In this version the infrastructure address (ARFC 0002 section 3.4) is the
only address Alloy puts on a host. Routes to VPC prefixes on a client node
follow this clause and ARFC 0009 clause 3.13, which the spec task of M11 rung 6
adds.

3.17.4 This version creates no firewall state at all. A later clause that needs
some uses only reserved tables, gives every base chain the policy `accept`, and
accepts, drops, rejects, or rewrites only packets that enter or leave through a
reserved interface or are addressed to an address or port of Alloy's.

3.17.5 On a Proxmox VE host, Alloy changes none of the host's bridges, its
network configuration files, its firewall rules or configuration, its storage
configuration or the devices and volumes it uses, and none of its guests: it
never starts, stops, pauses, migrates, or reconfigures a guest that Alloy did
not create. It writes nothing under `/etc/pve`. Alloy adds to a pool no block
device but one a cloud admin names, and by default uses only its `file` disk
under its data directory (ARFC 0002 section 3.1).

3.17.6 The **host snapshot** of a machine is the output of the commands below,
with counters, statistics, and timestamps removed:

- Linux: `ip -d link show`, `ip address show`, `ip route show table all`,
  `ip -6 route show table all`, `ip rule show`, `ip -6 rule show`,
  `nft list ruleset`, `iptables-save` and `ip6tables-save` where present,
  `sysctl -a` restricted to `net.`, `systemctl list-unit-files`, `getent passwd`,
  `getent group`, and `/etc/resolv.conf`. On a Proxmox VE host also the list of
  guests with their states and configurations, `/etc/network/interfaces`, and
  the SHA-256 of every file under `/etc/pve`.
- Windows: `Get-NetAdapter`, `Get-NetIPAddress`, `Get-NetRoute`,
  `Get-NetFirewallRule`, `Get-Service`, the machine `PATH`,
  `Get-DnsClientServerAddress`, and `Get-DnsClientNrptRule`.

These must hold, and the M11 scripts under `tests/e2e/` check them:

- After an install on a machine with no data directory, the snapshot equals the
  one taken before it, except for the unit or service `alloyd` and, on Windows,
  the search path entry.
- While the machine is a joined node, its snapshot differs from the one taken
  before the install only in reserved objects.
- After an uninstall, with or without `--purge`, the snapshot equals the one
  taken before the install, except, on Windows, for the Wintun driver when
  Alloy installed it.
- With the unit `alloy.service` enabled, the user and group `alloy`, and the
  directories `/etc/alloy` and `/var/lib/alloy` holding files, as Grafana Alloy
  leaves them, the install, `join`, and an uninstall with `--purge` succeed,
  the three rules above hold, and those four objects and every file in them
  are unchanged.
- With Docker running a container that publishes a port, and with Tailscale up,
  holding an address in 100.64.0.0/10, the three rules above hold, and through
  install, join, mesh traffic, and uninstall the container keeps reaching an
  outside address, its published port keeps answering, and the host keeps
  reaching a Tailscale peer.
- On a Proxmox VE host with its firewall enabled and a running guest on one of
  its bridges, the three rules above hold, and the guest keeps its state and
  its network reachability throughout.

3.17.7 Guest id range. The **guest id range** is the user and group ids
1,900,000,000 to 1,900,065,535. Each VMM (ARFC 0011 section 2.1) runs with a
user id and a group id from this range, the two equal, that no other running
process uses. Alloy creates no `passwd` or `group` entry for them. An id is
**free** when no entry that `getent passwd` or `getent group` lists has it, no
line of `/etc/subuid` or `/etc/subgid` names a range that holds it, and no
process runs with it as its real, effective, or saved user or group id. The
`names` check of `init` and `join` on Linux fails unless every id in the range
is free (ARFC 0002 section 3.8). Before it starts a VMM, `alloyd` checks that the
id it picked is free, and when it is not, it starts no VMM and fails the
guest's start with `Conflict`, naming the entry, the line, or the process that
holds the id.

## 4. Formats

### 4.1 Install manifest

The manifest records what an install placed (clause 3.15). It is UTF-8 text.
Every line ends with a line feed and has a keyword, one space, and its fields
separated by one space. The last field runs to the end of the line, so a path
may hold spaces.

| Line | Fields |
|---|---|
| `alloy-manifest 1` | First line. The format version |
| `version <version>` | Second line. The installed release |
| `kind <kind>` | Third line. `node`, `client`, or `cli` (clause 3.15 step 1) |
| `state <state>` | Fourth line. `pending` while step 6 runs, then `complete` |
| `dir <path>` | A directory the install created |
| `file <sha256> <path>` | A file the install placed, with its SHA-256 in 64 lower case hexadecimal digits |
| `unit <name>` | A systemd unit the install placed |
| `service <name>` | A Windows service the install created |
| `path <entry>` | An entry the install appended to the machine `PATH` |

Paths are absolute. Entries after the fourth line are in the order the install
created them, a directory before what it holds. A reader that finds a first
line other than `alloy-manifest 1`, or a line it cannot parse, exits 1 and
changes nothing.

### 4.2 Host file

The host file is the file `alloy-host` in the data directory. It holds the
node's local join state, its started release (clause 3.11), and every host
object that Alloy created outside the manifest (clause 3.17.2). Every release
reads and writes this version of the format, so that any release's install and
uninstall read any release's host file. It uses the line syntax of section 4.1.

| Line | Fields |
|---|---|
| `alloy-host 1` | First line. The format version |
| `join <state>` | Second line. `unjoined`, `joining`, or `joined` (ARFC 0002 section 2) |
| `release <version>` | Third line. The started release, or `none` before any `alloyd` has started |
| `link <boot id> <name>` | A Linux network interface |
| `netns <boot id> <name>` | A named network namespace |
| `nft <boot id> <family> <name>` | An `nftables` table |
| `run <boot id> <path>` | A directory under `/run` |
| `adapter <guid> <name>` | A Windows network adapter, with its interface GUID as Windows reports it, in lower case with braces |

`<boot id>` is the 36 character content of `/proc/sys/kernel/random/boot_id`
when the object was created, without its line feed.

- `init` and `join` write the host file before any other file in the data
  directory (ARFC 0002 section 3.12).
- A writer replaces the file whole: it writes `alloy-host.new` in the data
  directory, flushes it to disk, and renames it over `alloy-host`. Readers
  ignore `alloy-host.new`.
- Alloy adds an object's line before it creates the object, and removes the
  line once the object is gone. When the creation fails because the name is
  taken, it removes the line and treats the object as clause 3.17.2 says.
- `alloyd` rewrites the `join` line before it acts on a change of the local
  join state, and the `release` line as clause 3.11 says.
- A `link`, `netns`, `nft`, or `run` line **matches** its object when its boot
  id is the machine's current one and the object exists. An `adapter` line
  matches when an adapter of that name has that interface GUID. A line that
  matches nothing is dropped, and nothing is removed for it.

When the data directory meets the other conditions of ARFC 0002 section 3.12
and its host file has a first line other than `alloy-host 1`, or a line that
cannot be parsed, an install or an uninstall exits 1, naming the file, and
changes nothing, and `alloyd` treats the directory as not an Alloy data
directory.

## 6. Failure behavior

A node that fails to start after upgrade falls back to the previous binary, which
is kept until finalization, and reports the failure. A handover that fails before
the old process exits leaves the old process serving, once the successor has
ended. The old `alloy-net` does not resume an AL session from its own copy when
the successor may have used it. Before it serves again, it discards every AL
session its successor marked consumed, raises its handshake replay table to the
initiation counters its successor recorded, and reserves a new handshake counter
block, as ARFC 0003 section 3.9 requires, so no nonce is used twice (I-07) and no
handshake message or session message is accepted twice (ARFC 0020 section 3.2).
Within 1 second of serving again it starts a new session with every peer whose
newest session it discarded. Traffic from such a peer is dropped until that
session exists and the peer has seen it used, at most 1 second plus that
handshake and one keepalive (ARFC 0003 section 3.9). A failed handover can
therefore exceed, for those peers, the 100 ms gap that section 3.6 allows a
handover that succeeds.

An install or uninstall that fails stops at the failing step with the exit code
of clause 3.15 or 3.16. Before its step 6 an install has changed nothing. From
step 6 on, the manifest lists every path written, or a path whose temporary
name it is, so running the same script again completes the install, and an
uninstall removes it. A script cut short in transfer runs nothing (clause 3.15).
An uninstall that stops between removing a host object and rewriting the host
file leaves a line that matches nothing, which the next run drops. A digest mismatch or
an archive entry that breaks clause 3.13 ends the install before any entry is
written outside the temporary directory. An address or route that would
overlap existing use (clause 3.17.3) is not created: `init` and `join` fail
their `addresses` check with exit 5, and an `alloyd` that meets such an overlap
later reports it in the node's status. Both name the route or interface it
overlaps.

## 7. Limits

One upgrade in progress per cluster. Two release versions present at most.
One preview release per cluster: a machine on another release is refused at
`join`, an install that would change it on a node is refused, and an `alloyd`
that finds it serves nothing (clause 3.13). Install downloads: 10 seconds to connect, 30
seconds without a byte, 3 attempts 2 seconds apart (clause 3.15). Uninstall
waits 30 seconds for each process to end before killing it (clause 3.16).

## 8. Security

Signed artifacts only, from the first release that is not a preview. A preview
release's archives are checked against the digests written into its install
scripts, and every asset carries a provenance attestation (clause 3.14). Whoever
can publish the repository's releases can therefore put code on every machine
that installs from them, and for previews no release key narrows that. The
install scripts verify TLS for every download, extract nothing before the digest
matches, and refuse archive entries that could write outside their directory.
Downgrade below the finalized version is refused.

Coexistence (clause 3.17) keeps Alloy from weakening the host's own defenses:
it adds no firewall state and changes none the host has, so a host firewall
that blocks the mesh port stays in force until its owner opens it. An
uninstall removes only objects it can show Alloy created (clause 3.17.2), so it
cannot be used to remove another product's interface, unit, user, or data.
Secrets in the data directory are protected as ARFC 0020 clause 3.7 says, by
the owner and access rules of ARFC 0002 section 3.12, which `init`, `join`, and
`alloyd` check before they write or read a secret there, so a local user who
creates the directory first gets a refusal, not the node's keys. An uninstall
without `--purge` leaves those secrets on the machine and says so. Upholds
I-17, I-18, I-22.

## 9. Open questions

- How client nodes (ARFC 0002 section 3.11), which may be offline for long
  periods, count toward gates (clause 3.2) and finalization (clause 3.7).
  Today they count like any node. Spec task to be cut before the first release
  that is not a preview.
- Custody of the release key of clause 3.14. ADR to be written before the first
  release that is not a preview.
- macOS client nodes: a service and a virtual interface for `alloyd` on macOS.
  Spec task to be cut in M11 when a rung needs one.

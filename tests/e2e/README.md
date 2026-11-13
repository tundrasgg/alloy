# End-to-end checks of M11

Each rung of M11 (`plan/milestones.md`) leaves a script here that reruns its
check from a clean state. The machines are given only as parameters. Replace the
placeholders below with your own:

| Placeholder | Meaning |
|---|---|
| `<version>` | The release under test, such as `0.0.1-preview.1` |
| `root@<node-a>` | SSH destination of the first Linux node, logging in as root |
| `root@<node-b>` | SSH destination of the second Linux node, the Proxmox VE host |

Every script prints one `PASS` or `FAIL` line per check, with the differences
indented under a `FAIL`, then a `SUMMARY` line. It exits 0 when every check
passed, 1 when any failed, and 2 on a usage error. In a snapshot difference, a
line starting with `-` is expected and missing, and a line starting with `+` is
found and not expected.

## Files

| File | What |
|---|---|
| `rung1.ps1` | Rung 1 driver: install and uninstall on every machine |
| `host-snapshot.sh` | The host snapshot of a Linux machine (ARFC 0017 clause 3.17.6) |
| `host-snapshot.ps1` | The host snapshot of a Windows machine (same clause) |
| `rung1-probe.sh` | Reads the install state of a Linux node for rung 1 |

The snapshot scripts only read. Later rungs reuse them.

## Rung 1: install and uninstall

Checks ARFC 0017 clauses 3.15 (install), 3.16 (uninstall), and 3.17.6 (host
snapshot) on two Linux nodes and on the Windows client.

Before you start:

- Every machine is clean: Alloy was never installed, or was uninstalled with
  `--purge`, so that there is no manifest and no Alloy data directory.
- The Windows client has the OpenSSH client, and `ssh root@<node-a>` and
  `ssh root@<node-b>` log in with a key and no prompt.
- Each Linux node has `curl`, `base64`, and `sha256sum`.

From an elevated 64-bit PowerShell on the Windows client, at the repository
root:

```
powershell -NoProfile -ExecutionPolicy Bypass -File tests\e2e\rung1.ps1 -Version <version> -LinuxNode root@<node-a>, root@<node-b> -ProxmoxNode root@<node-b>
```

For each Linux node over SSH, then for the Windows client itself, it:

1. Checks that the machine is clean, and takes a host snapshot.
2. Runs the one-line install from the release's pinned URL:
   `curl -fsSL https://github.com/tundrasgg/alloy/releases/download/v<version>/install.sh | sh`
   on Linux, and `irm https://github.com/tundrasgg/alloy/releases/download/v<version>/install.ps1 | iex`
   on Windows.
3. Checks that `alloy version` prints the version, that the manifest is complete
   and every file it lists has its digest, that the unit or service `alloyd`
   exists, is disabled, and is not started, that no data directory and no
   temporary name exists, and that the snapshot differs from the first only by
   the unit or service `alloyd` and, on Windows, the `PATH` entry.
4. Runs the install again, and checks that it exits 0, changes no installed file,
   and leaves the snapshot as it was.
5. Runs the uninstall, `sh /usr/local/share/alloy/uninstall.sh` on Linux and the
   installed `uninstall.ps1` on Windows, and checks that every installed path is
   gone and that the snapshot equals the first.
6. Runs the uninstall again, through `install.sh --uninstall` or
   `install.ps1 -Uninstall` since the installed script is gone, and checks that
   it exits 0 and changes nothing.

On a node given with `-ProxmoxNode`, every snapshot comparison also checks the
guest list with states and configurations, `/etc/network/interfaces`, and the
SHA-256 of every file under `/etc/pve`, each as its own check.

On a remote machine the driver runs only the snapshot script, the probe script,
the install, and the uninstall. It writes the snapshot and probe scripts to the
standard input of `ssh.exe`, which runs `sh -s -- <arguments>` on the node, so
nothing is copied to the node and no script passes through a command line.

To look at a snapshot by hand, from a POSIX shell for a Linux node and from an
elevated PowerShell for the Windows client:

```
ssh root@<node-b> sh -s -- --proxmox < tests/e2e/host-snapshot.sh
powershell -NoProfile -ExecutionPolicy Bypass -File tests\e2e\host-snapshot.ps1
```

## What a comparison ignores

Clause 3.17.6 removes counters, statistics, and timestamps. The snapshots also
leave out the state below, which changes with no action by Alloy. Everything
else the commands of the clause print is compared.

| State left out | Where | Why |
|---|---|---|
| Link reference counts (`promiscuity`, `allmulti`) and bridge `*_timer` values | `ip -d link show` | Counters and running timers |
| Address `valid_lft` and `preferred_lft` in seconds (`forever` is kept) | `ip address show` | Remaining lifetimes count down |
| Route `expires` | `ip route`, `ip -6 route` | Remaining lifetimes count down |
| Rule counters, quota usage, `last used` ages, set element `expires` | `nft list ruleset` | Counters and countdowns |
| Comment lines and chain packet and byte counters | `iptables-save`, `ip6tables-save` | The comments carry the time of the dump; counters |
| `net.netfilter.nf_conntrack_count` | `sysctl -a` | A counter |
| Units in the state `transient` or `generated` | `systemctl list-unit-files` | No install places them. systemd makes transient units at run time, such as the `session-<n>.scope` of each login, this check's SSH logins included. Generators make generated units from other configuration, such as `.mount` units |
| The status dot files at the top of `/etc/pve` (`.version`, `.members`, `.vmlist`, `.clusterlog`, `.rrd`, `.debug`) and `*.tmp.<pid>` names | `/etc/pve` digests | Status written by the cluster file system; temporary names before a rename |
| The `"timestamp":<n>` field | HA status files under `/etc/pve` | A timestamp |
| Every property not named in `host-snapshot.ps1`, among them adapter link speeds and address and route lifetimes | Every Windows section | Measured rates and countdowns. The named properties are the ones an install or a configuration change sets |
| Service `Status` (the start type is kept) | `Get-Service` | Windows starts and stops demand-start and trigger-start services on its own |

After an install, the comparison also drops the unit or service `alloyd` and,
on Windows, takes the `PATH` entry the install appends as expected, as clause
3.17.6 allows. On a Proxmox VE node the three Proxmox VE sections are compared
as their own checks, not left out.

Things that do change on their own and are kept can still fail a comparison:
an IPv6 temporary address that rotates, a DHCP lease that moves to another
address, a Proxmox VE key rotation, or a Windows firewall rule that another
program adds while the check runs. Rerun the check when one of those shows in
the differences.

## Checking the scripts

Every shell script parses, each on its own (`sh -n a b` would parse only `a`):

```
for f in tests/e2e/*.sh; do sh -n "$f" || exit 1; done
```

Every PowerShell script parses, in Windows PowerShell 5.1 and in PowerShell 7.
The variables passed by `[ref]` must exist first, or the parse never runs and
the command still exits 0:

```
powershell -NoProfile -Command '$t = $null; $e = $null; foreach ($f in Get-ChildItem tests/e2e/*.ps1) { $null = [System.Management.Automation.Language.Parser]::ParseFile($f.FullName, [ref]$t, [ref]$e); if ($e.Count -gt 0) { $e; exit 1 } }'
pwsh -NoProfile -Command '<the same>'
```

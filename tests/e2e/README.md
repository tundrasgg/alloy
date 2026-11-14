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
| `rung2.ps1` | Rung 2 driver: init, join, login, node list, and uninstall with `--purge --force` |
| `e2e-lib.ps1` | The functions the drivers share: checks, SSH, snapshots and their comparison, probes, the reserved objects of ARFC 0017 clause 3.17, and node lists |
| `host-snapshot.sh` | The host snapshot of a Linux machine (ARFC 0017 clause 3.17.6), and its stability self-check |
| `host-snapshot.ps1` | The host snapshot of a Windows machine (same clause) |
| `rung1-probe.sh` | Reads the install state of a Linux node. Rung 2 uses it too |
| `rung2-probe.sh` | Reads the hostname of a Linux node and its hardware virtualization check, from which rung 2 expects the node's name and roles, and whether root has an `alloy` CLI configuration |
| `rung2-cli-remove.sh` | Removes root's `alloy` CLI configuration directory on a Linux node, which an uninstall keeps. Rung 2 runs it only where the run made the directory |

The snapshot and probe scripts only read. Later rungs reuse them.

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

## Rung 2: join, login, and node list

Checks the rung 2 profile of ARFC 0002 section 3.15: sections 3.1 (init), 3.2
(join token), 3.3 (join), 3.7 (login code), and 3.11 (client nodes), ARFC 0006
clause 3.15 (node state), ARFC 0019 clauses 3.1.11 to 3.1.15 (login, local
admin, node list), and ARFC 0017 clauses 3.16 (uninstall) and 3.17.6 (host
snapshot), on the same machines as rung 1. Before you start, every machine is
clean, as for rung 1, and root on each Linux node has no `alloy` CLI
configuration (`$XDG_CONFIG_HOME/alloy`, by default `~/.config/alloy`). With
a current context there, `alloy login --issue-code` would go to the API
instead of the local admin socket (ARFC 0019 clause 3.1.14). Then, from an
elevated 64-bit PowerShell on the Windows client, at the repository root:

```
powershell -NoProfile -ExecutionPolicy Bypass -File tests\e2e\rung2.ps1 -Version <version> -LinuxNode root@<node-a>, root@<node-b> -ProxmoxNode root@<node-b>
```

`init` runs on the first `-LinuxNode`. `-TimeoutSeconds` (default 30) bounds
each wait for a node list to reach the expected state. The driver:

1. Reads each machine's hostname and, on Linux, whether `/dev/kvm` opens for
   reading and writing and whether root has a CLI configuration. From these it
   expects each node's name (the default name of ARFC 0002 section 3.3) and
   roles: every role on `<node-a>`, `compute,storage` on each other Linux
   node, each without `compute` when `/dev/kvm` does not open, and none on the
   Windows client. It checks that each machine is clean, with no CLI
   configuration for root on Linux, and takes a host snapshot.
2. Installs the release on every machine, as rung 1 does.
3. Runs `alloy init` on the Windows client, which must exit 1 at its host
   check and create no data directory.
4. Runs `alloy init` on `<node-a>`. It must exit 0 within 30 seconds, print
   one join token and one login code, once each, on standard output only, and
   print no standing warning. The data directory has no `disks` directory,
   since rung 2 makes no pool.
5. Root on `<node-a>`, with no context yet, issues the login codes through
   the local admin socket. `alloy login --issue-code --output json` must exit
   2, and `alloy login --issue-code` run as user `nobody` must exit 4, each
   printing no login code. Then `alloy login --issue-code` prints one login
   code for `<node-b>`, and `alloy login --issue-code --user admin --org
   default` one for the Windows client. All three codes differ.
6. On `<node-a>`, `alloy login <login code from init>` must exit 0 and print
   the user `admin` and the organization `default`. The node list then shows
   `<node-a>` alone and `active`. The same code again must be refused, since a
   code works once. With the context now current, `alloy login --issue-code`
   must exit 4, since the API port denies `IssueLoginCode` in rung 2, and
   `alloy login --issue-code --user admin` must exit 2. A second `init` must
   exit 5.
7. On each other Linux node, `alloy join` with a token changed in one
   character must exit 2 and create no data directory. Then, on each other
   Linux node and then on the Windows client, `alloy join <join token>` must
   exit 0, and the node must show `active` in the node list of `<node-a>` less
   than 10 seconds after the join started. This time includes SSH and the
   polling. A second `join` must exit 5. On a Linux node the data directory
   has no `disks/0`, and `alloy login --issue-code` must exit 4 and print no
   code, since a node without the `control` role has no local admin socket.
   Then `alloy login <login code>` with that machine's code must exit 0 and
   print `admin` and `default`.
8. On every machine, `alloy node list --output json` must show exactly the
   three nodes, each with its roles and `active`, which is how the node list
   of rung 2 shows a node healthy (ARFC 0006 clause 3.15). The table of
   `alloy node list` must have the columns, widths, order, and values of ARFC
   0019 clause 3.1.15. Neither prints a standing warning.
9. Takes a snapshot of each machine. It must differ from the one taken before
   the install only in the reserved objects of clause 3.17, those named below.
10. Uninstalls each machine with `--purge --yes --force`, the Windows client
    first, then the other Linux nodes, then `<node-a>`. Rung 2 builds no
    `leave`, so the uninstall's `alloy node leave` fails and `--force` goes on
    (ARFC 0017 clause 3.16 step 4). After each uninstall no installed path,
    data directory, or service remains, and the snapshot equals the first one.
    On a Linux node the CLI configuration of root is still there, since an
    uninstall never removes it, and the driver then removes it with
    `rung2-cli-remove.sh`, so that a rerun starts with no context. It leaves
    the Windows client's CLI configuration and stored keys alone: a rerun
    replaces that context when it logs in again.

When a step that later steps need fails, the driver skips them, and then
uninstalls with `--purge --yes --force` from the install script every machine
it installed on, and removes the CLI configuration the run made, so that a
rerun starts clean.

Every join token and login code in the output is shown as `alyt_(hidden)` or
`alyc_(hidden)`.

### Reserved objects in a joined node's snapshot

To compare a joined node's snapshot with the one taken before the install, the
driver removes these objects from the joined node's snapshot only. A clean
machine holds none of them, so one found before the install shows as a
difference:

| Section | Removed |
|---|---|
| `ip -d link show`, `ip address show` | The block of each interface named `al` or `alv` then a digit |
| `ip route show table all`, `ip -6 route show table all` | Each route whose every device is such an interface, in the table `main`, or in the table `local` with `proto kernel`, which the kernel adds for an address on the interface |
| `nft list ruleset` | Each table named `alloy` or starting with `alloy_` |
| `sysctl -a (net.)` | The parameters under `net.ipv4.conf.<interface>`, `net.ipv6.conf.<interface>`, `net.ipv4.neigh.<interface>`, and `net.ipv6.neigh.<interface>` of such an interface. The kernel makes the `neigh` ones with the interface |
| `systemctl list-unit-files` | `alloyd.service` and units whose names start with `alloyd-` |
| `Get-NetAdapter`, `Get-NetIPAddress`, `Get-NetRoute` | Each adapter named `al` then a digit, and the addresses and routes on it |
| `Get-Service` | The service `alloyd` |
| `machine PATH` | The entry `%ProgramFiles%\Alloy`, when appended as clause 3.15 step 6 appends it |

Every other section, the Proxmox VE sections included, must be unchanged.

## What a comparison ignores

Clause 3.17.6 removes counters, statistics, and timestamps. The snapshots also
leave out the state below, which changes with no action by Alloy. Everything
else the commands of the clause print is compared.

| State left out | Where | Why |
|---|---|---|
| Link reference counts `promiscuity` and `allmulti` | `ip -d link show` | Counters |
| Every `*_timer` value of a bridge or bridge port: `hello_timer`, `tcn_timer`, `topology_change_timer`, `gc_timer`, `message_age_timer`, `forward_delay_timer`, `hold_timer`, `mcast_querier_ipv4_other_timer`, `mcast_querier_ipv6_other_timer` | `ip -d link show` | Running timers |
| Bridge `fdb_n_learned` | `ip -d link show` | The number of MAC addresses the bridge has learned. It changes whenever a guest or a LAN host sends traffic (`fdb_max_learned`, the limit, is kept) |
| Bridge port `mcast_n_groups` | `ip -d link show` | The number of multicast groups snooped on the port (`mcast_max_groups`, the limit, is kept) |
| Bridge `mcast_querier_ipv4_addr`, `mcast_querier_ipv4_port`, and the `ipv6` pair | `ip -d link show` | The multicast querier the bridge last heard on the LAN, learned from traffic (`mcast_querier`, the setting, is kept) |
| Bridge `topology_change` and `topology_change_detected`, bridge port `topology_change_ack` and `config_pending` | `ip -d link show` | STP flags that the kernel sets on a port change and that the timers above clear |
| Bond port `link_failure_count` | `ip -d link show` | A counter |
| CAN `berr-counter` | `ip -d link show` | Bus error counters |
| Address `valid_lft` and `preferred_lft` in seconds (`forever` is kept) | `ip address show` | Remaining lifetimes count down |
| Route `expires` | `ip route`, `ip -6 route` | Remaining lifetimes count down |
| Rule and set element counters, the `packets` and `bytes` of a named counter object, quota usage, `last used` ages, set element `expires` | `nft list ruleset` | Counters and countdowns |
| Comment lines and chain packet and byte counters | `iptables-save`, `ip6tables-save` | The comments carry the time of the dump; counters |
| `net.netfilter.nf_conntrack_count` | `sysctl -a` | A counter |
| Units in the state `transient` or `generated` | `systemctl list-unit-files` | No install places them. systemd makes transient units at run time, such as the `session-<n>.scope` of each login, this check's SSH logins included. Generators make generated units from other configuration, such as `.mount` units |
| The status dot files at the top of `/etc/pve` (`.version`, `.members`, `.vmlist`, `.clusterlog`, `.rrd`, `.debug`) and `*.tmp.<pid>` names | `/etc/pve` digests | Status written by the cluster file system; temporary names before a rename |
| Every `timestamp` field, and the order of keys | The HA status files under `/etc/pve` (below) | A timestamp; Proxmox VE writes the keys in a different order each time |
| Every property not named in `host-snapshot.ps1`, among them adapter link speeds and address and route lifetimes | Every Windows section | Measured rates and countdowns. The named properties are the ones an install or a configuration change sets |
| Service `Status` (the start type is kept) | `Get-Service` | Windows starts and stops demand-start and trigger-start services on its own |

For `ip -d link show` the value is removed and the attribute's name is kept, so
an attribute that appears or disappears still shows. Every other attribute
there is configuration or state that holds still on an idle machine, and is
compared: among them `state`, the STP results `designated_root`, `root_port`,
and `root_path_cost`, a bond's LACP `ad_info`, and a tap's `numqueues`. The
other commands of the clause print no counter, statistic, or timestamp beyond
the ones in the table: `ip rule`, `getent`, `/etc/resolv.conf`, and the
Proxmox VE guest list and configurations print only configuration and state,
and the `net.` sysctls other than `nf_conntrack_count` are settings.

After an install, the comparison also drops the unit or service `alloyd` and,
on Windows, takes the `PATH` entry the install appends as expected, as clause
3.17.6 allows. On a Proxmox VE node the three Proxmox VE sections are compared
as their own checks, not left out.

Things that do change on their own and are kept can still fail a comparison:
an IPv6 temporary address that rotates, a DHCP lease that moves to another
address, a Proxmox VE key rotation, an element that a rule adds to an nftables
set from traffic, or a Windows firewall rule that another program adds while
the check runs. Rerun the check when one of those shows in the differences.

### The HA status files under /etc/pve

Proxmox VE's HA resource managers rewrite their status files on their own,
with no action by Alloy: `/etc/pve/nodes/<node>/lrm_status` (the local resource
manager) and `/etc/pve/ha/manager_status` (the cluster resource manager, when
present; any `manager_status` under `/etc/pve` is treated the same). Each is JSON
with a `timestamp` that advances and with its keys in a different order on each
write, such as
`{"mode":"active","state":"wait_for_agent_lock","results":{},"timestamp":1794559030}`.
A raw digest of these files differs between any two snapshots.

So `host-snapshot.sh` hashes these files after normalizing them: it parses the
JSON, removes every `timestamp` key at any depth, and prints it again with the
keys sorted and no whitespace. It uses `perl` with JSON::PP, else `python3`.
Proxmox VE is written in Perl and JSON::PP is a Perl core module, so a stock
node has the first. The section's first line names the tool used. When
neither is present, or a file is not valid JSON, the file is hashed raw and
`(raw: ...)` says so, and its digest is then expected to differ between
snapshots. Every other file under `/etc/pve` is hashed raw, so any change to
its bytes shows.

## Checking that snapshots are stable

Before a rung run, check that two snapshots of each idle machine, taken a few
seconds apart, are equal. A difference is a value that changes on its own: it
would fail every comparison of the rung, and the snapshot script must remove
it. On a Linux node, the self-check mode of `host-snapshot.sh` takes the two
snapshots on the node and prints one `PASS` or `FAIL` line, the differences,
and a `SUMMARY` line, and exits 0 when they are equal and 1 when not (add
`--proxmox` on a Proxmox VE host):

```
ssh root@<node-a> sh -s -- --self-check 20 < tests/e2e/host-snapshot.sh
ssh root@<node-b> sh -s -- --proxmox --self-check 20 < tests/e2e/host-snapshot.sh
```

On the Windows client, from an elevated PowerShell at the repository root. It
prints `equal` and exits 0, or prints the differences and exits 1:

```
powershell -NoProfile -ExecutionPolicy Bypass -Command '$a = & tests\e2e\host-snapshot.ps1; Start-Sleep -Seconds 20; $b = & tests\e2e\host-snapshot.ps1; $d = @(Compare-Object $a $b -CaseSensitive); if ($d.Count -eq 0) { ''equal'' } else { $d; exit 1 }'
```

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

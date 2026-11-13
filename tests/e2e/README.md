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
the install, and the uninstall. It passes each script in the SSH command line,
so nothing is copied to the node.

To look at a snapshot by hand, from a POSIX shell for a Linux node and from an
elevated PowerShell for the Windows client:

```
ssh root@<node-b> sh -s -- --proxmox < tests/e2e/host-snapshot.sh
powershell -NoProfile -ExecutionPolicy Bypass -File tests\e2e\host-snapshot.ps1
```

What a snapshot leaves out, so that it does not change on its own: link
reference counts and bridge timers, address and route lifetimes, firewall
counters and quota usage, `iptables-save` comment lines and chain counters,
`net.netfilter.nf_conntrack_count`, the cluster file system's status files at
the top of `/etc/pve` and the `timestamp` field of its HA status files, the link
speed of Windows adapters, and the running status of Windows services (their
start type is kept). Things that do change on their own and are kept can still
fail a comparison: an IPv6 temporary address that rotates, a DHCP lease that
moves to another address, a Proxmox VE key rotation, or a Windows firewall rule
that another program adds while the check runs. Rerun the check when one of
those shows in the differences.

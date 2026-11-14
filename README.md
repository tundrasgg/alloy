# Alloy

The cloud is your computer.

Alloy is an open source private cloud. One command starts a cluster, one token
joins a machine to it, and the result behaves like a public cloud that you own:
instances, containers, private networks, public addresses, block volumes, object
storage, and a container registry.

- **Runs anywhere.** One board, one rack, or many sites. A cluster needs a single
  address that its machines can reach.
- **Brings its own network.** Machines form an encrypted overlay with direct
  connections where the network allows and relayed connections where it does not.
- **Multi-tenant.** Organizations and projects are isolated from each other in
  compute, network, and storage.
- **Familiar APIs.** A native API, plus EC2-compatible and S3-compatible surfaces
  and an OCI registry.

```
alloy init --advertise 203.0.113.7     # first machine, prints a join token
alloy join alyt_...                    # every other machine
```

## Status

Preview. Alloy publishes preview releases, tagged `v0.0.PATCH-preview.N`, as
GitHub pre-releases at <https://github.com/tundrasgg/alloy/releases>. A preview
is not for production use: it makes no compatibility promise, and a cluster on
one moves to another release only by being created again. A preview carries
only part of what this file describes; the specifications in `spec/` describe
the whole design.

### What a preview installs

Install a preview by its tag, for example `v0.0.1-preview.2`:

```
# Linux, as root or a user that can sudo
curl -fsSL https://github.com/tundrasgg/alloy/releases/download/<tag>/install.sh | sh

# Windows 10 version 1809 or later, from an elevated PowerShell
irm https://github.com/tundrasgg/alloy/releases/download/<tag>/install.ps1 | iex
```

Previews are built for x86_64 Linux and x86_64 Windows.

- On Linux 6.1 or later with systemd, the install places `alloy` (the CLI) and
  `alloyd` (the node daemon) in `/usr/local/bin` and writes the systemd unit
  `alloyd`. The unit is enabled once the machine is part of a cluster. On any
  other Linux machine, it installs the CLI only.
- On Windows, the install places `alloy` and `alloyd` in
  `%ProgramFiles%\Alloy` and registers `alloyd` as a service, disabled until the
  machine is part of a cluster. A Windows machine is a client and hosts no
  compute.

Uninstall, as root or from an elevated PowerShell:

```
sh /usr/local/share/alloy/uninstall.sh
powershell -NoProfile -ExecutionPolicy Bypass -File "$env:ProgramFiles\Alloy\uninstall.ps1"
```

An uninstall keeps the data directory unless given `--purge` (`-Purge` on
Windows).

### Milestone M11: first deployment

The current milestone is M11, a slice through the mesh, the control plane, and
compute, run on real machines: two Linux nodes, one of them a Proxmox VE host,
and one Windows client. It proceeds in seven rungs, in order: install, join,
mesh, compute, VPC, client, and survival. A rung is done when its check, a
script under `tests/e2e/`, passes on the real machines. Rung 1, install, has
passed.

`plan/milestones.md` defines M11, its rungs, and the other milestones.
`plan/STATE.md` names the rung in progress.

## Repository layout

| Path | Contents |
|---|---|
| `spec/` | ARFCs (Alloy RFCs), the normative design, one per subsystem |
| `adr/` | Architecture decision records, one decision per file |
| `plan/` | Milestones, the task board (`STATE.md`), task files, reports |
| `agent/` | How work is done here: behavior, protocol, roles, engineering rules |
| `product/` | Product principles and briefs for user-facing surfaces |
| `research/` | Research notes with sources |
| `crates/` | Rust workspace members |
| `packaging/` | Install and uninstall scripts, and the tools that build release assets |
| `tests/` | Conformance suites and the M11 end-to-end checks |

## How work happens

Alloy is built by coordinated agents and by humans following the same rules.
Everything a contributor needs is in this repository: `agent/ROOT.md` explains the
process, `plan/STATE.md` says what is happening now. See `CONTRIBUTING.md`.

## License

Licensed under either of Apache License, Version 2.0 (`LICENSE-APACHE`) or MIT
license (`LICENSE-MIT`) at your option. Contributions are accepted under the same
dual license.

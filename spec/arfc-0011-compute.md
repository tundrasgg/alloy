# ARFC 0011: Compute

- Status: accepted
- Depends on: 0006, 0007, 0009, 0013
- Crates: alloy-compute, alloy-init, alloy-imds
- Decisions: 0022, 0023, 0024, 0038

## 1. Purpose

Runs tenant workloads, as virtual machines and as containers, with the isolation
of a virtual machine for both.

## 2. Model

| Resource | Is |
|---|---|
| instance | A virtual machine: a shape, a boot volume, ENIs, volumes, user data |
| container | One or more OCI containers that share a network namespace, run inside one microVM |
| shape | A named size: vCPUs, memory, and optional limits. Custom sizes are allowed |
| image | A bootable disk image, public to the cluster or private to a project |
| key pair | An SSH public key injected at first boot |
| placement group | A set of guests with a spread or pack rule |

Guest states: `pending`, `starting`, `running`, `stopping`, `stopped`,
`migrating`, `failed`, `deleted`.

Each guest is one VMM process, started by `alloyd`, in the sandbox below (ADR
0022, ADR 0038). The seccomp filter is the same for every guest of one
`hypervisor` implementation and version. The user, namespaces, guest directory,
and Landlock rules are per guest.

2.1 The VMM process runs as a Unix user that is not root in the initial user
namespace and that no other guest's VMM and no other Alloy process runs as. It
has its own cgroup, user namespace, mount namespace, and network namespace.

2.2 Each guest has one guest directory on the node. Every file the VMM writes and
every Unix socket it binds or connects to is inside it: the control API socket,
each vhost-user socket for network and block, and the serial console. No other
guest's user can read, write, or search the guest directory or anything in it.

2.3 The VMM's mount namespace contains no other guest's directory and no Unix
socket file outside its own guest directory. Its network namespace contains no
network interface other than loopback; migration traffic (section 3.5) leaves the
sandbox only through a Unix socket in the guest directory.

2.4 Every thread of the VMM process runs under a seccomp filter (`Seccomp: 2` in
`/proc/<pid>/task/<tid>/status`) from before the guest's first instruction until
the process exits, and a system call the filter does not allow ends the process.
The filter is fixed per `hypervisor` implementation and version and is not derived
from the guest's devices. Alloy never runs a VMM with its filter disabled, in a
logging mode, or in a mode that returns an error instead of ending the process.
For `cloud-hypervisor` this is its built-in filter in its default mode.

2.5 The VMM runs under a Landlock rule set fixed when the VMM starts. The rules
Alloy passes grant read-write access beneath the guest directory and read-only
access to each file outside it that the guest's configuration names at start
(firmware, kernel, initramfs, read-only images), and nothing else. No rule is
added after start.

2.6 Hot attach (3.1.4) of a volume or ENI reaches the VMM only as a vhost-user
socket that `alloy-blk` or `alloy-net` creates inside the guest directory, so it
is covered by the rules of 2.5 and needs no new rule.

2.7 Landlock does not confine Unix sockets, so 2.2, 2.3, and this clause do: each
socket file in a guest directory grants write permission, which connecting
requires, to that guest's user and to no other guest's user, and abstract sockets
of the host and of other guests are unreachable because the network namespace
isolates them.

## 3. Behavior

### 3.1 Instances

3.1.1 An instance boots from a volume cloned from an image. The clone is
copy-on-write and must take under 1 second regardless of image size.

3.1.2 Devices are virtio only: network (vhost-user to `alloy-net`), block
(vhost-user to `alloy-blk`), console, entropy, and memory balloon. Firmware boot
(UEFI) and direct kernel boot are both supported.

3.1.3 From `Create` returning to the guest's first instruction must take under 3
seconds on a node that has the image's chunks, excluding guest boot.

3.1.4 `Stop` sends an ACPI shutdown and forces power off after 90 seconds.
`Restart`, `Resize` (while stopped), and hot attach and detach of volumes and ENIs
are supported.

3.1.5 Serial console is available through the API as a stream, with the last 1 MiB
of output kept.

### 3.2 Containers

3.2.1 A container resource names OCI images, commands, environment, mounts, and
resources. Alloy boots a microVM with an Alloy-provided kernel and `alloy-init` as
PID 1, which mounts each image and starts each container under the OCI runtime
specification.

3.2.2 Image layers are pulled once per node by the host, converted to a read-only
block image, cached, and attached read-only. The writable layer is a volume or
memory.

3.2.3 A container has ENIs, security groups, volumes, and an instance role exactly
as an instance does. From `Create` to the container's entry point running must
take under 2 seconds with a cached image.

3.2.4 Restart policy: `always`, `on-failure`, `never`. Logs are captured from
standard output and error and are available through the API.

### 3.3 Metadata service

`alloy-imds` answers on `169.254.169.254` and `fd00:ec2::254` from each guest's
port, served by the host. It follows the token-based protocol of the EC2 instance
metadata service version 2: a `PUT` for a token, then `GET` with the token. It
serves instance identity, user data, network configuration, SSH keys, and
temporary credentials for the guest's instance role. Requests must be attributed
by port, never by anything in the packet a guest can forge. Standard cloud images
using cloud-init must configure themselves without changes.

### 3.4 Scheduling

3.4.1 The scheduler filters nodes (role, capacity, architecture, shape features,
zone, taints, placement rules, volume locality) and scores the rest (balance,
image and volume locality, relay-only last).

3.4.2 A placement is a claim committed in the control plane. Two guests must never
be committed to capacity that only fits one.

3.4.3 Overcommit is per node pool: CPU default 4.0, memory default 1.0.

3.4.4 Scheduling a guest must take under 100 ms at the 99th percentile in a region
of 10,000 nodes.

### 3.5 Migration

Live migration moves a running instance between nodes of the same architecture
over the mesh. With a replicated boot volume only memory moves. Downtime must be
under 500 ms for a guest dirtying memory at up to 100 MiB per second. `alloy node
drain` migrates every guest off a node.

### 3.6 Nodes without hardware virtualization

Such a node must not run guests for more than one org. It may hold `compute` only
when dedicated to a single org by a cloud admin, and then runs containers through
the `sandbox` seam with namespaces, seccomp, and cgroups. Instances are refused
there with `Unsupported`.

## 5. Seams

| Kind | Trait | Implementations | Suite |
|---|---|---|---|
| `hypervisor` | `Hypervisor` | `cloud-hypervisor`, `mem` | `tests/conformance/hypervisor` |
| `sandbox` | `Sandbox` | `namespace`, `mem` | `tests/conformance/sandbox` |
| `scheduler-policy` | `SchedulerPolicy` | `default`, `mem` | `tests/conformance/scheduler` |

`cloud-hypervisor` runs as a separate program. Task T-0004 confirms its
capabilities against this ARFC.

## 6. Failure behavior

VMM crash: the guest is `failed`, and restarts if its policy says so. Node loss:
after 60 seconds without a heartbeat and a fencing confirmation (ARFC 0013), guests
with replicated volumes are restarted elsewhere. Guests with node-local volumes
stay `failed` until the node returns. `alloyd` restart: guests are untouched
(I-17).

## 7. Limits

256 vCPUs and 1 TiB of memory per instance. 16 ENIs and 32 volumes per guest. 16
containers per container resource. 64 KiB of user data.

## 8. Security

The guest is hostile. Everything it can reach, the VMM, vhost-user backends, the
metadata service, is bound by I-04. Upholds I-01, I-04, I-05.

## 9. Open questions

- Device passthrough for GPUs. Product brief to be cut in M10.
- Windows guests: firmware and driver requirements. Research task to be cut in M3.

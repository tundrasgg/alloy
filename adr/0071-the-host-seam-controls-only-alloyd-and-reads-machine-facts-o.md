# ADR 0071: The host seam controls only alloyd and reads machine facts, outside Runtime

- Status: accepted
- Subject: engineering
- Specs: arfc-0002, arfc-0005, arfc-0017

## Context

`alloy init` and `alloy join` must enable and start `alloyd` and read the facts that ARFC 0002 section 3.8 checks: privilege, platform, architecture, cores, memory, virtualization, disks, and free space. Every operating system call goes through `alloy-rt` (ADR 0067), and no seam did this. The install already defines the unit or service `alloyd`, leaves it disabled and stopped, and enables it only to resume a joined node (ARFC 0017 clause 3.15). Alloy may change only the service it owns (ARFC 0017 clause 3.17).

## Decision

- `alloy-rt` gains a sixth seam, `host` (trait `Host`, marker `HostSeam`, implementations `os` and `mem`, suite `tests/conformance/rt/host/`), specified in ARFC 0005 section 2.8 and clause 3.1.18.
- It is not part of `Runtime`. `init` and `join` build it from the registry beside their runtime. `register_os` registers it, `from_registry` does not build it.
- Its service methods take no name and act only on `alloyd.service` or the Windows service `alloyd`: status, enable, disable, start, and stop. Each action finishes or fails within 60 seconds. `Start` of a disabled service is refused on every platform.
- Linux runs `systemctl` from `/usr/bin` or `/bin`, never through `PATH`, with only `LC_ALL=C` in its environment. Windows calls the service control manager.
- The install owns the definition of the unit or service. The seam changes only enablement and run state, never writes a unit file or runs `daemon-reload`, and never changes a Windows service's configuration other than its start type.
- `init` and `join` stop a running `alloyd` before starting it, so it starts on the data directory they wrote, and on failure leave it disabled and stopped, as a first install does.
- The `host` check of ARFC 0002 section 3.8 also requires the unit or service to be installed, so `init` on a machine without it fails before writing anything.

## Consequences

- No component other than `init` and `join` can stop or disable `alloyd`: the seam is out of `Runtime`.
- The simulator models service state per host, which outlives a crash, and reports facts from `MachineConfig`. It runs no program on `Start`.
- The `os` service tests need privilege and a unit or service `alloyd` that is no real install's. `cargo xtask ci` on the CI hosts installs one running the workspace's `alloyd`, and the tests act only where `ALLOY_TEST_SERVICE` is `1`.
- Windows reports no disks, since it joins only as a client node.

## Alternatives rejected

- A sixth member of `Runtime`: every component would hold the power to stop `alloyd`, and every `Runtime::new` caller and runtime conformance test would change.
- Library functions in `alloyd` that call `systemctl` and the service control manager: they break the rule that only `alloy-rt` calls the operating system, and the simulator could not run `init`.
- Talking to systemd over D-Bus: it needs a new dependency for five calls that `systemctl` already makes, as the install does.
- Leaving enablement to the install script: ARFC 0002 section 3.1 step 4 would lose its meaning, and a failed `init` could not clean up.

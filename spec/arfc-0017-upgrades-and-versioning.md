# ARFC 0017: Upgrades and versioning

- Status: accepted
- Depends on: 0000, 0003, 0006, 0008
- Crates: alloyd, alloy-state
- Decisions: 0029

## 1. Purpose

A cluster is upgraded one node at a time, while running, without stopping guests.
This ARFC defines the version rules that make that possible.

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
upgrade is **finalized**. Until finalization, every node can be rolled back to N.

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

3.7 `alloy cluster upgrade --finalize` writes the new formats. It refuses while
any node is on the old release.

3.8 Release artifacts are signed. `alloyd` verifies the signature against the
release key compiled into the running binary before replacing itself.

3.9 Patch releases change no format, capability, or API.

3.10 CI runs every simulation scenario in a mixed-version mode once a previous
minor release exists: half the nodes on N, half on N+1.

## 6. Failure behavior

A node that fails to start after upgrade falls back to the previous binary, which
is kept until finalization, and reports the failure. A handover that fails before
the old process exits leaves the old process serving.

## 7. Limits

One upgrade in progress per cluster. Two release versions present at most.

## 8. Security

Signed artifacts only. Downgrade below the finalized version is refused. Upholds
I-17, I-18.

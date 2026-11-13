# ARFC 0017: Upgrades and versioning

- Status: accepted
- Depends on: 0000, 0003, 0006, 0008
- Crates: alloyd, alloy-state
- Decisions: 0029, 0056, 0057, 0059

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
release key compiled into the running binary before replacing itself.

3.9 Patch releases change no format, capability, or API.

3.10 CI runs every simulation scenario in a mixed-version mode once a previous
minor release exists: half the nodes on N, half on N+1.

3.11 Running release. The global group (ARFC 0006 section 2.2) stores with each
node record the release its `alloyd` runs, the node's **running release**. It is
control plane state, not a field of the signed record of ARFC 0002 section 2.
Each node also keeps two releases in node-local state that survives an `alloyd`
restart and is lost when the node is reinstalled: its **started release**, the
release of the last `alloyd` that started on it, and its **applied release**, the
release of the last running release write applied for it.

At each start, `alloyd` reads both, then sets the started release to its own
release before it does anything else. If its own release equals both values it
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

## 7. Limits

One upgrade in progress per cluster. Two release versions present at most.

## 8. Security

Signed artifacts only. Downgrade below the finalized version is refused. Upholds
I-17, I-18.

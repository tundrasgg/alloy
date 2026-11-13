# ADR 0059: Finalization covers new groups, running releases, and re-encoding

- Status: accepted
- Subject: operations
- Specs: arfc-0001, arfc-0017

## Context

ADR 0056 left gaps. A regional group created after cluster creation held no
write versions, so a newer release stored bindings an older one could not read. A
node that fell back to the old release before the switch kept running after it.
The check only decoded, so a binding too long at `V(R)` failed after the switch,
and a binding write delayed past the check was never checked.

## Decision

- The global group records each regional group before the group's first entry,
  and that entry copies the write versions the record write returns. Write
  versions only move to a newer release, whatever order writes commit in. A group
  without write versions refuses binding writes. Finalization switches every
  recorded regional group and completes only when all have switched. A region
  with no recorded group is not waited for.
- Each group holds a binding epoch that every binding write names. The check
  increments it in each group before reading that group, so a write whose epoch
  was read earlier is refused, whatever release sent it.
- The global group stores each node's running release. `alloyd` writes it before
  it serves, unless node-local state shows that the same release was the last
  one started and written for that node. The switch applies only if every
  running release is `R` and none other was written since the check began. A
  running release older than the finalized one is refused, and that `alloyd`
  serves nothing.
- The check computes `reencode(value, V(R))`. A writer also encodes at its own
  `VERSION` and refuses what it cannot write there.
- A write refused only because the switch changed the write version, or a check
  changed the epoch, is encoded again and written once more. `SeamConfig`
  requires `Eq`.

## Consequences

- What the rewrite would fail on stops finalization while rollback is possible.
- A node that falls back to the old release blocks the switch until it is
  upgraded or removed.
- An `alloyd` that starts at a release other than the last one written for its
  node, as in an upgrade or a fallback, serves nothing while the global group
  has no quorum. This is the one exception to ARFC 0006 clause 3.9. A restart at
  the same release and a view resume do not depend on the global group.
- A recorded regional group without quorum holds finalization incomplete and is
  reported until its quorum returns.
- ARFC 0017 gains a finalization suite that runs three builds of one tree.

## Alternatives rejected

- Report the race as a retryable error: the writer can encode and write again
  at once.
- Guarantee that `to_value(VERSION)` never fails: only the data shows whether a
  value grows past 65,536 bytes at a new version.
- Fence delayed writes by the writer's release: after a failed check, a rollback
  to the old release would have its own writes refused.
- Write the running release at every start and view resume: a global group
  outage would stop every restarted node in every region from serving.

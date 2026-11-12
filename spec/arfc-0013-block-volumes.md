# ARFC 0013: Block volumes

- Status: accepted
- Depends on: 0012
- Crates: alloy-blk
- Decisions: 0025

## 1. Purpose

Virtual block devices for guests: thin, snapshot-capable, replicated according to
their pool, and safe against two writers.

## 2. Model

| Resource | Is |
|---|---|
| volume | A sparse array of 4 MiB chunks in one pool. Has a size, a class, and optional limits |
| volume class | A name bound to a pool and default limits, for example `standard`, `fast`, `local` |
| snapshot | A read-only point-in-time view of a volume |
| attachment | A volume bound to one guest, with a fencing epoch |

A `local` class is a pool pinned to one node with `replica(1)`. It gives the
lowest latency and no protection against losing that node, and volumes in it pin
their guest to that node.

`alloy-blk` runs on each compute node and serves attached volumes to guests over
vhost-user-blk, and to the host over a block device for container use.

## 3. Behavior

3.1 A new volume consumes no space. Space is allocated on first write to a chunk.
Reads of unwritten ranges return zeros.

3.2 A snapshot is taken in under 1 second regardless of size and consumes space
only as the volume diverges. Snapshots are crash consistent.

3.3 A clone of a snapshot or image is a new writable volume sharing unchanged
chunks. Deleting a snapshot or volume frees only chunks nothing else references.

3.4 **Single writer.** An attachment holds a lease in the control plane with an
epoch that increases on every attach. `alloy-blk` sends the epoch with every
write. The chunk store must reject a write whose epoch is lower than the highest
it has seen for that volume (I-13). A node that loses contact with the control
plane for 20 seconds must stop writing. The control plane must wait 30 seconds
after a node's last heartbeat before granting its volumes to another node.

3.5 A flush from the guest returns only when every write before it is durable
under the pool's policy.

3.6 Volumes grow online. Shrinking is not supported.

3.7 Limits on IOPS and throughput are enforced per volume by `alloy-blk` with a
token bucket and a burst allowance. A volume without limits must not be able to
starve others on the same disk beyond its fair share (I-04).

3.8 Discard from the guest frees whole chunks that become empty.

3.9 Snapshots can be exported to and imported from a bucket as a stream of
changed chunks, full or incremental against an earlier snapshot.

3.10 Volumes in an encrypted pool are encrypted per ARFC 0012. A volume may also
carry its own key supplied through the API, which Alloy holds only in memory
while the volume is attached.

3.11 Performance floor, measured in CI on the reference machine in
`tests/system/README.md`, for a `local` NVMe volume at queue depth 32: at least
80% of the raw device's 4 KiB random read and write IOPS.

## 5. Seams

| Kind | Trait | Implementations | Suite |
|---|---|---|---|
| `block-backend` | `BlockBackend` | `native`, `mem` | `tests/conformance/block-backend` |

## 6. Failure behavior

Loss of a PG member: I/O continues if the PG is at or above `min_size`, with
latency bounded by the failure detection time of 5 seconds. Below `min_size`,
writes block for up to 60 seconds and then return an I/O error to the guest.
`alloy-blk` restart: guests see a pause of under 2 seconds and no error.

## 7. Limits

1 GiB to 64 TiB per volume. 1,024 snapshots per volume. Clone depth of 16, after
which a clone is flattened in the background.

## 8. Security

A deleted volume's chunks are unreadable from the moment of deletion, and freed
space never exposes old data to a new owner. Upholds I-05, I-12, I-13.

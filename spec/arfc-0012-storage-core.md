# ARFC 0012: Storage core

- Status: accepted
- Depends on: 0001, 0005, 0006
- Crates: alloy-store
- Decisions: 0025

## 1. Purpose

Alloy has its own storage system. This ARFC defines its base: disks, pools,
placement, and a replicated chunk store. Block volumes (ARFC 0013), objects (ARFC
0014), and the registry (ARFC 0015) are built on it. Every layer is a runtime
seam, so backends can be mixed and migrated per pool.

## 2. Model

```
 volume / object / blob         ARFC 0013, 0014, 0015
 chunk store                    get, put, delete chunks by id, replicated
 placement groups               shards that replicate as a unit
 disks                          one per device or directory, one node each
```

| Term | Is |
|---|---|
| disk | A device or directory a node contributes. Has a class (`nvme`, `ssd`, `hdd`), a weight, and a failure domain path: zone, rack, node |
| pool | A set of disks, a redundancy policy, and a failure domain level |
| redundancy | `replica(n)`, n from 1 to 5, or `erasure(k, m)` with Reed-Solomon coding |
| chunk | Up to 4 MiB of data named by `(pool, object_id, index)` with a version |
| placement group (PG) | `hash(object_id) mod pg_count`. Each PG maps to an ordered set of disks |
| cluster map | Disks, pools, PG counts, and an epoch. Held in the control plane |

## 3. Behavior

### 3.1 Placement

3.1.1 The disks of a PG are a pure function of `(cluster map, pool, pg)`, computed
by weighted rendezvous hashing that takes at most one disk per failure domain at
the pool's level. Any party with the map computes the same answer. There is no
per-chunk lookup service.

3.1.2 Adding or removing a disk must move only the share of data that the change
in weight implies, within 10%.

### 3.2 Replication

3.2.1 Each PG has a primary, the first live disk in its set. Clients send writes
to the primary. The primary orders them, applies them, forwards them to the other
members, and acknowledges only when every live member of the acting set has
synced the write.

3.2.2 A PG accepts writes only while at least `min_size` members are live:
`n/2 + 1` for `replica(n)` with n above 1, and `k + 1` for `erasure(k, m)` with m
above 1.

3.2.3 An acknowledged write must survive the loss of `n - 1` disks for
`replica(n)`, and `m` disks for `erasure(k, m)`, in distinct failure domains
(I-12).

3.2.4 Membership changes go by epoch. A member must reject any request from an
older epoch. A primary must confirm it is primary in the current epoch before
serving a read.

3.2.5 Each PG keeps a log of recent writes. A member that returns after a short
absence catches up from the log. A member too far behind is backfilled. Recovery
I/O is throttled to leave at least half of each disk's measured throughput for
clients, by default.

### 3.3 Integrity

3.3.1 Every 4 KiB block is stored with a checksum, verified on every read. A
mismatch must never be returned to a client: the read is served from another
member and the bad copy is repaired.

3.3.2 Each PG is scrubbed, every checksum verified and members compared, at least
every 7 days.

### 3.4 Encryption

A pool may be encrypted at rest. Each chunk is sealed with XChaCha20-Poly1305
under a per-object key wrapped by a pool key from the `key-provider` seam.

### 3.5 Disks

3.5.1 The `extent` disk implementation owns a raw block device, uses direct I/O,
allocates extents itself, and keeps its metadata in a write-ahead log on the same
device. The `file` implementation stores chunks as files in a directory and is
meant for small machines and development.

3.5.2 A disk must survive power loss at any moment with no acknowledged write
lost and no corrupt chunk exposed.

### 3.6 One node

A pool with `replica(1)` on one disk is valid and is what a single-node cluster
gets by default (I-16). The CLI and console mark such a pool as unprotected.

### 3.7 Changing backends

A pool is bound to one `chunk-store` implementation by name. Data moves between
pools, and so between implementations, online: `alloy pool migrate` copies in the
background, then switches each volume, bucket, or repository over with no
downtime.

## 5. Seams

| Kind | Trait | Implementations | Suite |
|---|---|---|---|
| `disk` | `Disk` | `extent`, `file`, `mem` | `tests/conformance/disk` |
| `chunk-store` | `ChunkStore` | `native`, `mem` | `tests/conformance/chunk-store` |

## 6. Failure behavior

Disk failure: its PGs re-form without it and recover to other disks after 10
minutes, or immediately when a cloud admin marks it out. Below `min_size` a PG
stops accepting writes and reports `degraded-blocked`, it does not accept writes
it could lose. A full disk stops accepting writes at 95% and reports it at 85%.

## 7. Limits

4 MiB per chunk. 1,024 disks per node. 100,000 disks per region. 65,536 PGs per
pool. Pools per cluster: 1,024.

## 8. Security

Storage nodes authenticate each other and clients by node record. Chunk ids carry
the org, and a disk refuses a request whose authenticated scope does not match.
Upholds I-05, I-12.

## 9. Open questions

- PG count autoscaling and splitting. Research task to be cut in M4.
- Erasure coded partial overwrite strategy. Research task to be cut in M4.

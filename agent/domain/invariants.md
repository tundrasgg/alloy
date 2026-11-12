# Global invariants

These hold everywhere, always. Reviewers check each one a change can reach. A
violation is a `block` finding. Each has, or must gain, a conformance or
simulation test.

## Isolation

- **I-01** A guest never sends or receives a packet for a VPC it has no ENI in.
- **I-02** A principal never reads, writes, lists, or learns the existence of a
  resource in another org unless a policy in that org grants it.
- **I-03** A node receives only the cluster state it needs for the guests,
  volumes, and peers it serves.
- **I-04** Guest-controlled input (packets, disk writes, metadata requests,
  virtio traffic) never crashes, blocks, or starves a host process serving other
  guests.
- **I-05** Stored data of one org is never returned to another, including through
  reused chunks, snapshots, images, or freed address space.

## Mesh

- **I-06** Every packet accepted from the underlay is authenticated before any
  state changes beyond a bounded, attacker-independent cost.
- **I-07** A nonce is never used twice with the same key.
- **I-08** Loss of every anchor breaks no established direct path.
- **I-09** A node whose record is revoked or expired is unable to open or keep
  sessions after the validity bound in `spec/arfc-0002-identity-and-join.md`.
- **I-10** The mesh never emits an underlay packet that needs IP fragmentation.

## State

- **I-11** Committed control plane state is linearizable and survives the loss of
  a minority of control nodes.
- **I-12** An acknowledged storage write survives any failure the pool's policy
  claims to tolerate.
- **I-13** At most one writer holds a volume at a time. A stale writer's writes
  are rejected, not merged.
- **I-14** No sequence of crashes and restarts leaves a resource that neither
  completes nor reports an error.
- **I-15** Every mutating API call is authorized, then recorded in the audit log,
  and the audit log is append-only and tamper evident.

## Operation

- **I-16** A single-node cluster is a fully working cluster.
- **I-17** Restarting or upgrading `alloyd` does not stop running guests or drop
  established sessions.
- **I-18** Any two nodes whose versions are one minor release apart interoperate.
- **I-19** Every seam implementation is selected at runtime and passes its seam's
  conformance suite.
- **I-20** The same seed produces the same simulation trace.

## Limits

- **I-21** Every queue, table, cache, and buffer has a stated bound and a defined
  behavior at the bound.
- **I-22** Secrets and key material never appear in logs, errors, metrics, or API
  responses.

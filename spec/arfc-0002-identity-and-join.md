# ARFC 0002: Identity and join

- Status: accepted
- Depends on: 0000, 0003
- Crates: alloy-id, alloy-crypto
- Decisions: 0012, 0015, 0018

## 1. Purpose

Defines what a cluster and a node are cryptographically, how the first node
creates a cluster, and how one token admits another machine.

## 2. Model

| Key | Type | Held by | Purpose |
|---|---|---|---|
| cluster key | Ed25519 | control plane | Signs node records and the cluster map |
| node identity key | Ed25519 | node | Signs the node's requests to the control plane |
| node mesh key | X25519 | node | Static key of the AL handshake (ARFC 0003) |
| API CA key | Ed25519 | control plane | Issues TLS certificates for API endpoints |

A **node record** is `{node_id, identity_pub, mesh_pub, roles, zone, infra_addr,
capabilities, not_after, serial}` signed by the cluster key. `node_id` is the
first 16 bytes of `BLAKE3(identity_pub)`. Peers accept a node only by a valid,
unexpired record.

## 3. Behavior

### 3.1 Init

`alloy init --advertise <addr>[,<addr>...]` generates the cluster key, a random
128-bit `cluster_id`, the API CA, and the node's keys. The node takes all four
roles, starts a control plane of one, creates the `default` organization and an
initial admin credential, and prints a join token and the admin credential once.

### 3.2 Join token

`alyt_` followed by unpadded base32 (RFC 4648 alphabet, lower case) of:

| Field | Size | Content |
|---|---|---|
| version | 1 | `1` |
| cluster_id | 16 | |
| cluster_key_fp | 16 | First 16 bytes of `BLAKE3(cluster_pub)` |
| token_id | 8 | Public lookup id |
| secret | 16 | Random. The control plane stores only `BLAKE3(secret)` |
| anchor count | 1 | 1 to 4 |
| per anchor | 32 + n | Anchor mesh public key, then an address list (type, address, port) |

Tokens have a time limit (default 24 hours), a use limit (default unlimited within
the time limit), and optional preset roles and zone. `alloy token create`,
`list`, and `revoke` manage them. Revocation takes effect at the next join
attempt.

### 3.3 Join

1. `alloy join <token>` generates node keys and opens an AL session to an anchor
   from the token, as an unregistered initiator.
2. An anchor must accept sessions from unregistered initiators only to its join
   service, rate limited to 10 handshakes per second per source address and 100
   per second in total.
3. The joiner sends `{token_id, secret, identity_pub, mesh_pub, facts}` where
   `facts` are architecture, cores, memory, virtualization support, and disks.
4. The control plane checks the token, checks `cluster_key_fp` against itself,
   and either admits the node or, when the cluster requires approval, leaves it
   `pending` until a cloud admin approves.
5. The joiner receives the cluster public key, its node record, and the bootstrap
   view. It must verify the cluster key against `cluster_key_fp` from the token
   before trusting anything else.
6. Time from running the command to an `active` node must be under 10 seconds on a
   LAN, excluding approval.

### 3.4 Addresses

Each cluster has an infrastructure prefix `fd00::/8` plus 40 bits from
`cluster_id`, giving a `/48`. A node's infrastructure address is that prefix, a
zero subnet, and 64 bits from `BLAKE3(mesh_pub)`. Control plane, storage, and
migration traffic use these addresses over the mesh interface `al0`.

### 3.5 Validity, renewal, revocation

Node records are valid for 24 hours. A node renews at half life by signing a
request with its identity key. Removing a node deletes its record: peers drop its
sessions when the map update arrives, and in the worst case, a partitioned peer,
when the record expires (I-09). Renewal needs the control plane, so a cluster
without quorum for more than 12 hours starts losing node records. Records in
that situation are extended by a signed "grace" statement only when a cloud admin
runs `alloy cluster grace` on a quorum-less recovery, see ARFC 0006.

### 3.6 Rotation

Node keys rotate with `alloy node rotate`: the new record lists both mesh keys for
one validity period. The cluster key rotates by cross signing: the old key signs
the new one, both are trusted for 7 days, then the old one is dropped.

## 6. Failure behavior

A join that fails at any step leaves no state on the joiner except its generated
keys, and a retry with the same token must succeed if the token is still valid. A
token whose secret does not match must count as a use and must be logged in the
audit log with the source address.

## 7. Limits

4 anchors per token. Token string length at most 512 characters. 10,000 live
tokens per cluster.

## 8. Security

A token is a bearer secret until it expires or is revoked. It grants the ability
to add a node, which is cluster-level trust. Upholds I-09, I-22.

## 9. Open questions

- Threshold custody of the cluster key across control nodes. Research task to be
  cut in M2.
- Hardware-bound node keys (TPM). Research task to be cut in M10.

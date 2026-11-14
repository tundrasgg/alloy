# ARFC 0002: Identity and join

- Status: accepted
- Depends on: 0000, 0003, 0007, 0009, 0017
- Crates: alloy-id, alloy-crypto
- Decisions: 0012, 0015, 0018, 0040, 0041, 0042, 0048, 0053, 0062, 0065, 0068

## 1. Purpose

Defines what a cluster and a node are cryptographically, how the first node
creates a cluster and hands its first credential to a cloud admin, how one token
admits another machine, and how a machine leaves.

## 2. Model

| Key | Type | Held by | Purpose |
|---|---|---|---|
| cluster key | Ed25519 | control plane | Signs node records and the cluster map |
| node identity key | Ed25519 | node | Signs the node's requests to the control plane |
| join token key | Ed25519 | whoever holds the token, derived from its secret (section 3.2) | Signs `join_request` (section 3.3 step 6) |
| node mesh key | X25519 | node | Static key of the AL handshake (ARFC 0003) |
| API CA key | ECDSA P-256 | control plane | Issues TLS certificates for API endpoints |

The API CA key is the one signing key that is not Ed25519, because the browsers
of ARFC 0019 clause 3.2.4 verify ECDSA P-256 in a TLS certificate chain and do
not verify Ed25519 (ADR 0048, R-0006). The API CA and the certificates it
issues must meet these rules:

- The API CA certificate is self-signed. It and every certificate the API CA
  issues carry an ECDSA P-256 public key, encoded as a SubjectPublicKeyInfo
  (RFC 5280 section 4.1.2.7) with algorithm `id-ecPublicKey` and named curve
  `secp256r1` (RFC 5480 section 2.1.1) and the point uncompressed (RFC 5480
  section 2.2). In DER that SubjectPublicKeyInfo is 91 bytes.
- Every signature the API CA key makes, on its own certificate and on each
  certificate it issues, is ECDSA with SHA-256, algorithm `ecdsa-with-SHA256`
  (RFC 5758 section 3.2). No certificate it issues carries any other signature
  algorithm.
- Every ECDSA P-256 signature Alloy makes, by the API CA key or by an API
  server certificate key, derives its per-signature nonce deterministically by
  RFC 6979 section 3.2 with SHA-256. Signing therefore draws on no random
  source, and the same key signing the same message yields the same signature.
  The private keys themselves are generated from the `alloy-rt` randomness
  trait.

Every certificate the API CA key signs, its own included, is an X.509 v3
certificate (RFC 5280) with these fields, and with no extension the table does
not list:

| Field | API CA certificate | API server certificate |
|---|---|---|
| serial number | 16 bytes from the `alloy-rt` randomness trait, with the high bit of the first byte cleared | The same rule |
| subject | `CN=Alloy API CA <cluster_id>`, the id as 32 lower case hexadecimal digits | `CN=<node name>` |
| issuer | Its own subject | The API CA certificate's subject |
| validity | From 1 hour before issue to 10 years after | From 1 hour before issue to 30 days after |
| subject key identifier | The leftmost 160 bits of the SHA-256 hash of the subjectPublicKey BIT STRING value (RFC 7093 section 2, method 1) | The same rule |
| authority key identifier | None | The API CA certificate's subject key identifier |
| basic constraints, critical | `cA` true, path length 0 | `cA` false |
| key usage, critical | `keyCertSign`, `cRLSign` | `digitalSignature` |
| extended key usage | None | `id-kp-serverAuth` |
| subject alternative name | None | One `iPAddress` entry per advertise address of the node (section 3.10), then `127.0.0.1` and `::1` |

Each control node holds one API server certificate and key for itself. It
generates a new key and gets a new certificate from the API CA when 10 days of
the current one's validity remain, and within 10 seconds after its advertise
addresses change. The API CA certificate is replaced only by a rotation that a
later task specifies (section 9).

A **node record** is the Protocol Buffers message `NodeRecord` of section 3.13,
signed by the cluster key. It holds `cluster_id`, `node_id`, `name`,
`identity_pub`, `mesh_pub` (one key, or two during a rotation, section 3.6),
`roles`, `zone`, `infra_addr`, `advertise`, `mesh_port`, `api_port`,
`not_after`, and `serial`. `node_id` is the first 16 bytes of
`BLAKE3(identity_pub)`. `advertise` is the node's list of advertise addresses
(section 3.10). `mesh_port` is the UDP port of its AL socket (ARFC 0004 section
3.6) and `api_port` the TCP port it serves the API and the `stream` underlay on
(ARFC 0008 section 3.8). The signature is Ed25519 by the cluster key over the
bytes `alloy/id/node-record/v1` followed by the record's encoded bytes, and
every holder keeps and forwards those bytes as it received them, never
re-encoded. Peers accept a node only by a valid, unexpired record. The control
plane stores with each node the `token_id` of the join token that admitted it.

The API reports each node in one **state**:

| State | Meaning |
|---|---|
| `pending` | A join request waits for a cloud admin's approval. No node record exists |
| `joining` | A node record is issued and the node has not yet reported to the control plane. It ends when the node reports, or when the record expires (section 6) |
| `active` | The control plane has heard from the node within the last 30 seconds |
| `unreachable` | The node holds a valid record and the control plane has not heard from it for 30 seconds |

`alloyd` keeps a machine's **local join state** in its data directory:
`unjoined`, `joining` (`alloyd` holds a join request, including one that is
`pending`), or `joined` (it holds a node record). An `unjoined` machine may still
hold the keys and handshake counter of a failed join attempt (section 6).

## 3. Behavior

### 3.1 Init

`alloy init` takes `--advertise <addr>[,<addr>...]`, `--name`, `--mesh-port`
(default 7476, ARFC 0004 section 3.6), and `--api-port` (default 443, ARFC 0008
section 3.8), none of them required. Its data directory is `/var/lib/alloyd`
(section 3.12). It does this:

1. It runs the checks of section 3.8, in the order stated there. They include
   that it runs as root on Linux, and that the local join state is `unjoined`,
   else it refuses with `Conflict`. Any `fail` stops it before anything is
   written.
2. It picks the advertise addresses: those given with `--advertise`, else the
   source address the kernel uses for the default IPv4 route, else for the
   default IPv6 route. With no default route and no `--advertise` it fails with
   a usage error. It reports each address, where it came from, and whether it is
   private, meaning inside 10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16,
   100.64.0.0/10, fc00::/7, 169.254.0.0/16, or fe80::/10.
3. It generates the cluster key, a random 128-bit `cluster_id`, the API CA key
   and certificate, the node's keys, its at-rest key, and its API server key and
   certificate (section 2), signs the node's record, and stores each as section
   3.14 says. It then creates:

   | Object | Value |
   |---|---|
   | node | Named by the rule of section 3.3, zone `default`, roles `anchor`, `control`, `compute`, `storage`, without `compute` when the virtualization check of section 3.8 is `warn` |
   | control plane | One voter (ARFC 0006 section 3.2) |
   | organization | `default` |
   | project | `default`, in `default` |
   | user | `admin`, in `default`, with cluster role `cloud-admin` and role `owner` of `default` (ARFC 0007) |
   | pool | `default`, `replica(1)`, on one `file` disk at `disks/0` in the data directory (ARFC 0012 sections 3.5 and 3.6) |
   | join token | 24 hours, unlimited uses, no preset roles or zone (section 3.2) |
   | login code | For `admin`, with `issued_by` `init` (section 3.7) |

4. It enables and starts `alloyd` and waits until the node is `active` and the
   API answers TLS on its port at the first advertise address that is on one of
   the node's interfaces. When no advertise address is on the node's
   interfaces, as for a forwarded address behind NAT, it checks the API at the
   loopback address instead, and says that it did not check whether other
   machines reach the advertise address.
5. It prints the join token and the login code once each, on standard output,
   and writes neither to any log or file. It then prints the standing warnings
   that apply (ARFC 0019 section 3.2.5).

When a step after the checks fails, `init` stops and disables `alloyd`, removes
everything it wrote, and exits with the failure. The local join state is then
`unjoined`, so a rerun starts clean. From start to exit, `init` must take under
30 seconds on a machine whose checks all pass.

### 3.2 Join token

`alyt_` followed by unpadded base32 (RFC 4648 alphabet, lower case) of these
fields, with integers big endian:

| Field | Size | Content |
|---|---|---|
| version | 1 | `1` |
| cluster_id | 16 | |
| cluster_key_fp | 16 | First 16 bytes of `BLAKE3(cluster_pub)` |
| token_id | 8 | Public lookup id |
| secret | 16 | Random. The seed of the token key below. No API call and no node message carries it |
| preset roles | 1 | Bit 0 `anchor`, bit 1 `control`, bit 2 `compute`, bit 3 `storage`, bits 4 to 7 zero. `0` for no preset roles |
| anchor count | 1 | 1 to 4 |
| per anchor | 33 + addresses | Anchor mesh public key (32), address count (1, value 1 to 4), then each address |
| checksum | 4 | First 4 bytes of `BLAKE3` over every preceding byte |

An address is a family byte (`4` or `6`), the address (4 or 16 bytes), the
anchor's `mesh_port` (2), and the TCP port of its `stream` underlay (2), which
is its `api_port` (ARFC 0004 section 3.6), or `0` for none. A token lists anchors in ascending order of
node name, each with its first 4 advertise addresses, and leaves out every
anchor that would make the string longer than 512 characters. One anchor always
fits: with 4 IPv6 addresses it is 180 bytes, 293 characters.

A string is a valid join token only when it starts with `alyt_`, the rest
decodes, it is at most 512 characters, the version is `1`, every count, family,
and reserved bit is in range, the fields use every byte exactly, and the
checksum matches. The CLI must check this before it contacts any machine. An
invalid string is a usage error: nothing is sent and no use is counted.

Tokens have a time limit (default 24 hours), a use limit (default unlimited
within the time limit), and optional preset roles and zone, fixed when the
token is created. The commands `alloy token create`, `list`, and `revoke` manage
them, through the methods `CreateJoinToken`, `ListJoinTokens`, and
`RevokeJoinToken` (action `cluster:CreateJoinToken` for the first, ARFC 0007
clause 3.10.1). Revocation takes effect at the next join attempt. A token's
**token key** is the Ed25519 key (RFC 8032 section 5.1.5) whose 32-byte seed is
`BLAKE3` over the 21 ASCII bytes `alloy/id/token-key/v1` followed by the
16-byte secret, and `token_pub` is its public key. The client that creates a
token generates its secret, derives the token key, and sends the API only
`token_pub`, which is all of the token key that the control plane stores, so
no API response carries a token secret (I-22). A joiner proves that it holds
the token by the token key's signature (section 3.3 step 6), never by sending
the secret. The token key signs nothing else, lasts as long as its token, and
is never rotated: a new token replaces it. A token that
is expired, revoked, or used up is kept, unusable, until 7 days after its expiry
time, so that a later attempt can be told why it was refused.

### 3.3 Join

`alloy join <token>` takes these flags, none of them required.

| Flag | Default |
|---|---|
| `--roles <list>` | The token's preset roles, else `compute,storage`. `compute` is left out of this default when the virtualization check of section 3.8 is `warn`. On Windows, `none`. `none` names no roles: the node is a client node (section 3.11) |
| `--zone <name>` | The token's preset zone, else `default` |
| `--name <name>` | From the hostname, by the rule below |
| `--advertise <addr>[,...]` | As in section 3.1 step 2, except that with no default route the node gets no advertise address, which is a usage error only when its roles include `anchor` or `control` |
| `--mesh-port <port>` | 7476, the default mesh port of ARFC 0004 section 3.6 |
| `--api-port <port>` | 443. Used only while the node holds `anchor` or `control` (ARFC 0008 section 3.8) |
| `--no-wait` | Off. When on, the command returns with exit 0 once `alloyd` holds the request |

The data directory is `/var/lib/alloyd`, and on Windows `Alloy` in the folder
Windows reports as `%ProgramData%` (section 3.12). No flag changes it.

The default name is the hostname in lower case, cut at the first dot, with every
character outside `[a-z0-9-]` replaced by `-`, prefixed with `node-` when it
does not start with a letter, and cut to 63 characters. When that name is in use
the control plane appends the lowest free suffix of `-2`, `-3`, and so on,
cutting the base so the result stays within 63 characters. A name given with
`--name` is used as given or refused. `--roles` naming `compute` on a machine
whose virtualization check is `warn` is a usage error, since such a node holds
`compute` only after a cloud admin dedicates it to one organization (ARFC 0011
section 3.6). `--roles` that differs from non-zero preset roles in the token is a
usage error, found before any machine is contacted. On Windows, any roles other
than `none`, given with `--roles` or preset in the token, are a usage error found
at the same point.

1. The CLI runs the checks of section 3.8, in the order stated there. Among
   them, it refuses with `Conflict`, changing nothing, when the local join state
   is `joined`, or `joining` with another token. When it is `joining` with the
   same token, it skips the remaining checks and resumes waiting at step 7.
2. It writes the join request, the token included, to the data directory as
   section 3.14 says, then enables and starts `alloyd`, which holds the request
   from here on and carries out steps 3 to 7. The local join state is now
   `joining`. On Windows, enabling sets the start type of the service `alloyd`
   (ARFC 0017 clause 3.15) to automatic. How the CLI learns the outcome from
   `alloyd` is internal to one release, and it learns it within 1 second.
3. `alloyd` generates node keys, or reuses the keys and handshake counter kept
   from an earlier failed attempt (section 6), and opens an AL session to an
   anchor from the token as an unregistered initiator, from its mesh port. It
   tries every address of every anchor in token order, UDP before `stream`, for
   at most 10 seconds in total. As soon as the session opens it sends a
   reflection request on it and answers the anchor's challenge (ARFC 0004
   section 3.1.2), since the anchor sends on the session only to its current
   endpoint. Steps 5 to 7 and the approval below are node messages on that
   session (section 3.13).
4. An anchor must accept sessions from unregistered initiators only to its join
   service (section 3.13), rate limited to 10 handshakes per second per source
   address and 100 per second in total.
5. The joiner sends `join_hello` with its release version. Without looking at
   any token, the anchor replies with either the refusal `version`, carrying the
   accepted minor releases and the highest release that any node runs, or the
   current cluster public key and the anchor's own node record. During the
   7-day overlap of a cluster key rotation (section 3.6) the reply also carries
   the previous cluster public key and its signature over the current one. A
   joiner is
   accepted when its `MAJOR.MINOR` is the lowest minor release L that any node
   runs, or the next minor release after L (ARFC 0017 sections 3.1 and 7).
   When the joiner or any node runs a preview release (ARFC 0017 clause 3.13),
   the joiner is accepted only when every node runs exactly the joiner's
   release. The
   joiner must check that `cluster_key_fp` equals the first 16 bytes of
   `BLAKE3` over the current key, or over the previous key when the reply
   carries it with a valid signature over the current key; that the record is
   validly signed by the current key and unexpired; and that its `mesh_pub` is
   the key the AL session authenticated. If any check fails it closes the
   session and sends nothing more.
6. The joiner sends `join_request`. Its `body` is the encoded `JoinRequestBody`
   `{token_id, identity_pub, name, name_is_default, roles, zone, advertise,
   mesh_port, api_port, facts}`, with `roles` and `zone` empty when not given,
   and `facts` being operating system, architecture, cores, memory,
   virtualization support, and disks. The request names no mesh key: the
   joiner's mesh key, in the record it may be issued and in every check below,
   is the key the AL session authenticated, as a `forwarded` carries it
   (section 3.13). The **join message** is the 16 ASCII bytes
   `alloy/id/join/v1`, `cluster_id` (16 bytes), that mesh key (32 bytes), and
   the bytes of `body` as sent. `token_signature` is the token key's signature
   over the join message (section 3.2), and `identity_signature` the node
   identity key's signature over the same bytes. The request carries no secret,
   and changing any byte of `body`, or naming another mesh key, breaks both
   signatures. The joiner waits at most 10 seconds for the answer. The control
   plane checks, in this order:
   1. The signatures, as below. An unknown `token_id`, a `token_signature` that
      does not verify against that token's `token_pub`, and an
      `identity_signature` that does not verify against the body's
      `identity_pub` are each refused with `token_refused`.
   2. The joiner. When a node or a `pending` request exists whose `identity_pub`
      equals the request's, whose mesh key is the one the AL session
      authenticated, and which this `token_id` admitted, this is a retry: the
      answer is that node's current record, or `pending`. A retry creates
      nothing, counts no use, and is answered whatever the token's state and
      whatever the rest of the request holds. When the `identity_pub` or the
      authenticated mesh key belongs to any other node or request, the refusal
      is `key_in_use`.
   3. Whether the token is revoked, expired, or used up.
   4. The roles and zone.
   5. The name.
   6. The pending bound.

   It then admits the node, issuing its record and putting it in state
   `joining`, or, when the cluster requires approval, leaves the request
   `pending`.
7. The joiner receives its node record and the bootstrap view, or `pending`, or
   a refusal, as `join_reply`. With a record it stores what section 3.14 lists,
   and its local join state becomes `joined`. The session it joined on stays
   open as a home session (ARFC 0004 section 3.1.2). With roles including
   `storage`, the node's one `file` disk at `disks/0` in the data directory
   joins pool `default` when that pool exists, and the pool's redundancy is
   unchanged.
8. Time from running the command to an `active` node must be under 10 seconds on
   a LAN, excluding approval.

The token check verifies `token_signature` with the `token_pub` the control
plane stores for the `token_id`. An unknown `token_id` and a signature that
does not verify get the same refusal, `token_refused`, byte for byte. Only after
both signatures verify does the control plane give any other refusal. Every
refusal is audit-logged with the source address.

| Refusal | When | Counts a use | `alloy join` exit |
|---|---|---|---|
| `version` | The joiner's release is outside the accepted window. Sent before any token is checked | no | 1 |
| `token_refused` | Unknown `token_id`, or a `token_signature` or `identity_signature` that does not verify | yes, when the `token_id` is known | 4 |
| `key_in_use` | The joiner's `identity_pub` or mesh key belongs to another node or request | no | 5 |
| `token_revoked`, `token_expired`, `token_used_up` | The token is revoked, past its time limit, or at its use limit. `token_expired` carries the expiry time | no | 4 |
| `preset_conflict` | `roles` or `zone` differs from the token's preset | no | 2 |
| `compute_unsupported` | Explicit or preset roles include `compute` and the facts show no hardware virtualization | no | 2 |
| `name_taken` | A name given with `--name` is in use | no | 5 |
| `busy` | 1,000 requests are `pending` | no | 6 |
| `rejected` | A cloud admin rejected the pending request | counted when it became `pending` | 4 |

An admitted or `pending` request counts one use. When an address answers but its
AL handshake or a check in step 5 fails, the joiner sends it nothing more and
moves on to the next address. When no address passes step 5 within the 10
seconds, `alloy join` exits 1 if any address answered, and 6 if none did. When
no answer to step 6 arrives within its 10 seconds, `alloy join` exits 6.

**Approval.** The cluster setting `join_approval` is `off` by default or
`required`. A `pending` request shows its name, roles, zone, and facts through
the API and lasts until a cloud admin calls `ApproveNode`, which issues the
record, or `RejectNode`, which deletes the request and sends `rejected`, or
until the token it used expires, which deletes it and sends `token_expired`.
Once it has received `pending`, the joiner's `alloyd` holds the request: it
keeps or reopens its session to an anchor and asks for the request's state with
`join_state` every 2 seconds, each a request with a time limit of 2 seconds
(section 3.13), the request known by the mesh key that session
authenticates, never sending `join_request` again. The CLI may stop waiting without cancelling the request. After approval
the node must become `active` within 10 seconds while an anchor is reachable.

### 3.4 Addresses

Each cluster has an infrastructure prefix, a `/48`: the byte `0xfd` followed by
the first 5 bytes of `cluster_id`. A node's infrastructure address is that
prefix, a zero 16-bit subnet, and the first 8 bytes of `BLAKE3(mesh_pub)`. Control plane, storage, and
migration traffic use these addresses over the mesh interface `al0`. Every node,
a client node included, holds its infrastructure address on `al0` with prefix
length 64, so the cluster's zero subnet is reached through `al0`, and holds no
other address there. On Windows `al0` is a Wintun adapter (ADR 0062). The
address and its route follow ARFC 0017 clause 3.17.

### 3.5 Validity, renewal, revocation

Node records are valid for 24 hours. A node's first record has `serial` 1, and
every record the control plane later issues for that node, by renewal, rotation
(section 3.6), or any other change, has the serial of the node's current record
plus 1, so a node's serials only grow. A node renews at half life by signing a
request with its identity key, and, when it could not then, as a computer that
was asleep could not, as soon as it can while its record is valid. Each `renew`
(below) is a request with a time limit of 10 seconds (section 3.13), and after
one that gets no reply the node sends the next 60 seconds after that one ended.
Removing a
node deletes its record: peers drop its
sessions when the map update arrives, and in the worst case, a partitioned peer,
when the record expires (I-09). Renewal needs the control plane, so a cluster
without quorum for more than 12 hours starts losing node records. Records in
that situation are extended by a signed "grace" statement only when a cloud admin
runs `alloy cluster grace` on a quorum-less recovery, see ARFC 0006.

**Expired record.** A node in local join state `joined` whose own record has
expired renews through the join service of an anchor, since peers no longer
accept it as a registered node. Its `alloyd` tries the anchors of its last view
by the mesh keys and addresses their last records there hold, expired or not,
as an unregistered initiator, with the steps, time limits, rate limits, and
checks of section 3.3 steps 3 to 5. It then sends `renew`: `{node_id, serial,
signature}`, where `serial` is that of its expired record and `signature` is
its identity key's signature over the bytes `alloy-renew-v1`, `cluster_id`,
`node_id`, and `serial` (8 bytes, big endian). A node whose record is valid
sends the same `renew`, with the serial of that record, on an AL session it
holds with a control node. The control plane answers with
`join_reply` within 10 seconds. It issues a new record only when the node exists, the signature verifies
against the node's `identity_pub`, the mesh key the AL session
authenticated is the node's `mesh_pub`, and `serial` is the serial of the
node's current record, expired or not. When all of these hold but `serial` is
below that serial, it issues nothing, writes no audit record, and answers with
the node's current record, expired or not. So a signed `renew` issues at most
one record, ever: one handled twice (section 3.13), or sent again by an anchor
that carried it (section 8), issues nothing the second time, whether or not
the record it issued has since expired. Otherwise it refuses with
`node_unknown`, byte for byte the same for a deleted node, an unknown
`node_id`, a bad signature, another mesh key, and a serial above the current
one. A renewal needs no token and counts no use. A node that receives a record
whose serial is above its own takes it as its record, and when that record has
expired it sends `renew` with that record's serial at once.

While the renewal fails, `alloyd` tries again every 60 seconds, logs that the
record expired each time, and `alloy node list` shows the node `unreachable`.
Once it succeeds the node is `active` within 10 seconds, with no command from
its user. On `node_unknown`, or when an anchor answers and fails the cluster key
check of section 3.3 step 5 because the cluster key was rotated after the
node's record expired (section 3.6), `alloyd` stops trying and logs that the
machine is no longer a node of the cluster and that `alloy node leave --force`,
then `alloy join` with a new token, makes it one again.

### 3.6 Rotation

Node keys rotate with `alloy node rotate`: the new record lists both mesh keys for
one validity period. The cluster key rotates by cross signing: the old key signs
the new one, both are trusted for 7 days, then the old one is dropped. The cross
signature is Ed25519 by the previous key over the bytes
`alloy/id/cluster-key/v1`, `cluster_id`, and the current public key (32 bytes). A join
token made before the rotation keeps working during those 7 days (section 3.3
step 5). When the old key is dropped, the control plane revokes every live token
whose `cluster_key_fp` names it.

### 3.7 Login code

A login code logs one computer in as one user. It is `alyc_` followed by
unpadded base32 (RFC 4648 alphabet, lower case) of these fields, with integers
big endian:

| Field | Size | Content |
|---|---|---|
| version | 1 | `1` |
| cluster_id | 16 | |
| api_ca_fp | 16 | First 16 bytes of `BLAKE3` over the DER encoding of the API CA certificate's SubjectPublicKeyInfo, the 91 bytes of section 2 |
| code_id | 8 | Public lookup id |
| secret | 16 | Random. The control plane stores only `BLAKE3(secret)` |
| endpoint count | 1 | 1 to 4 |
| per endpoint | 7 or 19 | Family byte (`4` or `6`), address (4 or 16), API TCP port (2) |
| checksum | 4 | First 4 bytes of `BLAKE3` over every preceding byte |

The endpoints are the first advertise address of each of up to 4 control nodes,
in ascending order of node name, each with that node's API port, so a code is at
most 226 characters. Validity is checked as for a join token (section 3.2),
before any machine is contacted.

- A code is for exactly one user, works once, and expires 24 hours after it is
  issued.
- `init` issues one for `admin`. The method `IssueLoginCode` in the `iam`
  service issues one for the user that ARFC 0007 clause 3.9.2 allows the caller
  to name. The client generates the secret and sends only `BLAKE3(secret)`. The
  response carries the other fields and never a secret.
- Each code records its `issued_by` (ARFC 0007 clause 3.9.5). A code is voided
  when ARFC 0007 clauses 3.9.7 and 3.9.8 say.
- A user has at most 10 unused, unexpired codes. Issuing another deletes the
  oldest.
- To use a code, a client opens TLS 1.3 to its endpoints in order, for at most
  10 seconds in total. It must go on only when the presented chain verifies to a
  CA certificate whose SubjectPublicKeyInfo, as DER-encoded in that
  certificate, has the fingerprint `api_ca_fp`. Otherwise it closes the
  connection having sent no request. Because the fingerprint covers the
  algorithm and curve identifiers as well as the point, a key of any other type
  or encoding never matches.
- It then calls `ExchangeLoginCode` in the `iam` service with `code_id`, the
  secret, `name`, and, when it holds one, `replaces` (ARFC 0007 clauses 3.9.3
  and 3.9.10). `ExchangeLoginCode` is the one method of the native API that
  takes neither a session token nor a SigV4 signature: it is authenticated by
  the login code alone and authorized as the code's user when it commits (ARFC
  0007 clause 3.9.4).
- Calls to `ExchangeLoginCode` are limited to 10 per second per source address,
  counting an IPv6 source by its /64, and to 100 per second summed over all
  control nodes. A call over either limit fails with `RateLimited` and its
  retry delay (ARFC 0008 section 3.9) before the code is looked up. It spends
  nothing, creates nothing, writes no audit record, and is not an exchange
  attempt in the sense of ARFC 0007 clause 3.9.9.
- An unknown `code_id` and a wrong secret get the same `Unauthenticated` error,
  byte for byte. Only after the secret matches is any other error given: reason
  `login_code_used`, `login_code_voided`, or `login_code_expired` (the latter
  with the expiry time), then the failures of ARFC 0007 clauses 3.9.3 and 3.9.4
  and section 7. Every exchange attempt is audit-logged as ARFC 0007 clause
  3.9.9 says.
- An exchange spends the code when it succeeds and when the secret is wrong for
  a known `code_id`. Every other failure leaves the code as it was.
- When the code is valid, the control plane spends it and creates an access key
  for its user in one transaction (ARFC 0007 clause 3.9.4). The response carries
  what that clause names: the access key id, the name used, and the secret. It
  also carries the user's name, the organization the user lives in, and
  `project`, which is `default` when that organization has a project named
  `default` and is empty otherwise.
- Spent, voided, and expired codes are kept until 7 days after their expiry
  time, then deleted.

A computer, a node of any kind included, reaches the API only through a context
that `alloy login` made (ARFC 0019 clause 3.1.11), or, as user id 0 on a control
node, through the local admin socket for the commands ARFC 0019 clause 3.1.14
lists. `init` and `join` make no context: a join token admits a node and carries
no user's credential, and a node's keys authenticate it to the control plane
only on the node channel (section 3.13), never on the API. A new cluster
therefore gets the CLI working on each machine as follows. `init` prints one
login code. User id 0 on the control node issues one more per machine with
`alloy login --issue-code`. Each machine, the Windows client node and the
control node itself included, runs `alloy login <code>` once. The CLI reaches
the API over the underlay at the code's endpoints, never over the mesh (ARFC
0008 section 3.8).

### 3.8 Machine checks

Once their arguments parse, `init` and `join` run the `host` check first: when
it fails, nothing else runs. A join token is checked as part of argument parsing
(section 3.2), so an invalid one ends `join` with exit 2 before any check. They
then exit 4 when not run as root, or on Windows from an elevated administrator
process, then check the local join state (sections 3.1 and 3.3), exiting 5 on
`Conflict`, and then run the other checks in table order:

| Check | `ok` | `warn` | `fail`, exit code |
|---|---|---|---|
| host | Linux 6.1 or later on x86_64 or aarch64. For `join` of a client node, also Windows build 17763 or later on x86_64 or aarch64 | | Anything else, 1 |
| hardware virtualization | `/dev/kvm` opens for reading and writing | Otherwise | |
| ports | The mesh UDP port (default 7476, ARFC 0004 section 3.6) is free, and, when the node's roles include `anchor` or `control`, so is its API TCP port (default 443, ARFC 0008 section 3.8) | | A port is in use, 5. The message names the process and pid holding it when they can be found |
| data directory | The data directory does not exist, or is an Alloy data directory, or is a directory that section 3.12 lets `init` and `join` adopt | | Any other directory or file at its path, 5. The message names the path and its owner |
| names | No object that ARFC 0017 clause 3.17 reserves by name exists, other than an Alloy data directory and the install's files and service. On Linux, every id of the guest id range is free (ARFC 0017 clause 3.17.7) | | One exists, or an id is held, 5. The message names the object, or the entry, line, or process that holds the id |
| addresses | The cluster's infrastructure prefix (section 3.4) overlaps no route and no address, as ARFC 0017 clause 3.17.3 says | | An overlap, 5. The message names the route or the interface and address |
| free space | At least 10 GiB free under the data directory | 1 to 10 GiB | Under 1 GiB, 1 |

For a client node the hardware virtualization check does not run, and free
space is `ok` from 1 GiB, with no `warn`. `join` takes the `cluster_id` for the
addresses check from the token. `init` draws its `cluster_id` before the checks,
draws again while the prefix overlaps, at most 16 draws in all, and fails the
check only when all 16 overlap.

The roles that the ports check uses are known before any machine is contacted:
for `init` the roles of section 3.1, for `join` those of `--roles`, else the
token's preset roles, else the default of section 3.3.

Every check in the table runs and prints its result even after an earlier
`fail`. The exit code is that of the first `fail` in table order. A run with any
`fail` writes nothing: no file under the data directory, and `alloyd` neither
enabled nor started.

### 3.9 Leave

`alloy node leave` runs as root on the machine itself, takes no name, and asks
for confirmation as any destructive command does (ARFC 0019 section 3.1.6).

- `unjoined`: it changes nothing and exits 0.
- `joining`: it withdraws the request through an anchor with `withdraw`
  (section 3.13), a request with a time limit of 10 seconds, then removes the
  local state. `withdraw` carries `signature`, the node identity key's
  signature over the bytes `alloy/id/withdraw/v1`, `cluster_id`, and the mesh
  key of the session it is sent on (32 bytes). When `alloyd` holds no node
  keys yet it sends nothing and only removes the local state.
- `joined`: it asks the control plane with `leave`, signed with the node
  identity key over the bytes `alloy/id/leave/v1`, `cluster_id`, and `node_id`,
  to delete its node. The control plane refuses with `Conflict`, and the machine is
  unchanged, when the node is the last control node, hosts any guest, or has a
  disk that belongs to a pool. Otherwise it deletes the node (ARFC 0006 section
  3.6).

Then `leave` stops every process of ARFC 0000 section 2.3 that runs on the
machine, VMMs included, and removes every object that a line of the host file
matches (ARFC 0017 section 4.2), interfaces with their addresses and routes.
`alloyd` is stopped and disabled, on Windows by setting the start type of its
service to `Disabled`. The node's keys, handshake counter, and cluster state
under the data directory are removed, and the host file is left saying `join
unjoined` and listing no object. The machine then holds no reserved object of
ARFC 0017 clause 3.17 other than its data directory and the install's files and
service, so `join` on it passes the `names` check of section 3.8, and a `join`
after a `leave` succeeds as a first join does. `leave` is a request with a time
limit of 10 seconds (section 3.13). When the control plane does not
answer within that limit, `leave` fails with `Unavailable` and changes nothing
on the machine, unless `--force` is given: then it removes the local state anyway, and the node
record lasts until it expires (section 3.5) or a cloud admin deletes it. A
`leave` that gets no answer may still have deleted the node, as a write answered
`Unavailable` may (ARFC 0006 clause 3.12.2): its reply can be lost after the
deletion, and the control node then drops the node's sessions, so no resend
reaches it (section 3.13). The control node that deletes a node on its `leave`
sends the reply before it drops the node's sessions. A node already deleted
gets no answer to a later `leave`, and `alloy node leave --force` removes its
local state.

### 3.10 Advertise addresses

Every node has a list of 0 to 16 advertise addresses, set by `init`, by `join`,
or by `--advertise` on `alloy node update`, and carried in its node record. A
node uses them while it holds `anchor`, in join tokens (section 3.2), and while
it holds `control`, in login codes (section 3.7). A node holding `anchor` or
`control` must have at least one. A change of roles that adds `anchor` or
`control` to a node with none must fail with `InvalidArgument`, changing
nothing, unless the same call sets advertise addresses.

When advertise addresses are set, `alloyd` records which of them were on one of
the node's own interfaces at that moment. When a recorded address is on none of
its interfaces, the node must report it in its status within 60 seconds,
together with the node's current addresses, leaving out loopback, link-local,
and `al*` interfaces. While the node holds `anchor` or `control`, the control
plane then shows the standing warning `advertise-address-gone` for the node
(ARFC 0019 section 3.2.5), and clears it within 60 seconds after the address
returns, the advertise addresses are changed, or the node holds neither role.
`alloyd` never changes advertise addresses on its own, since join tokens and
contexts depend on them. Addresses that were not on the node's interfaces when
set are not watched.

### 3.11 Client nodes

A **client node** is a node with no roles. It lets a person's computer take part
in the mesh and reach VPC addresses by ARFC 0009 clause 3.13, which the spec
task of M11 rung 6 adds. It is a node in
every other sense: a cloud admin's join token admits it (ADR 0018), it holds a
node record, and `alloy node list` shows it with its state and mesh path.

- It runs on Linux, as any node, after `alloy join --roles none`, and on
  Windows, where `join` makes every node a client node. It runs `alloyd` and
  `alloy-net` and none of the other processes of ARFC 0000 section 2.3. On
  Windows `alloyd` runs as the service `alloyd` (ARFC 0017 clause 3.15). macOS
  client nodes are not supported in this version (ARFC 0017 section 9).
- Its mesh address is its infrastructure address on `al0` (section 3.4). It
  reaches other nodes through the path manager of ARFC 0004 like any node.
- It holds no role, so it relays for no other node, admits no join, serves no
  API, votes in no consensus group, runs no guest, and contributes no disk. An
  `UpdateNode` that adds a role to a client node on Windows fails with
  `InvalidArgument` and changes nothing. A client node on Linux may be given
  roles, and is then no longer a client node.
- Its view (I-03) holds its own record, the records of the anchors it homes on,
  and the records of the peers it holds sessions with, and of guest, volume, and
  VPC state only what ARFC 0009 clause 3.13 gives it.
- It may be off or asleep for days. Its record then expires (section 3.5), and
  once the machine is back on a network, its `alloyd` renews it as section 3.5
  says, with no command from its user.
- `init` refuses on Windows at its `host` check. `alloy node leave` works as
  section 3.9 says.

### 3.12 Data directory

The data directory holds a node's keys, its handshake counter, its cluster
state, and its host file (ARFC 0017 section 4.2), as section 3.14 lists. Its
path is fixed: `/var/lib/alloyd` on Linux, and on Windows `Alloy` in the folder
Windows reports as `%ProgramData%`. No flag, setting, or file changes it, so
`alloyd` started by the service manager, the install, and the uninstall (ARFC
0017 clauses 3.15 and 3.16) all find it at that path. On Linux, a cloud admin
who wants it on another disk mounts that disk at the path, which `init` and
`join` adopt as below. On Windows a mounted folder is a reparse point, which the
rule below refuses, so the data directory stays on the volume that holds
`%ProgramData%`. An **Alloy data directory** meets all of these:

- On Linux, it is not a symbolic link, is owned by user id 0, and has mode
  `0700`. Every directory above it is not a symbolic link, is owned by user
  id 0, and is not writable by its group or by others, so the directories
  checked are the ones the path resolves through. Its host file is owned by
  user id 0 and has mode `0600`.
- On Windows, it is not a reparse point, its owner is `SYSTEM` or the
  `Administrators` group, and its access control list is protected from
  inheritance and grants full control to `SYSTEM` and `Administrators` and no
  access to anyone else. Its host file has the same owner and inherits that
  list. No directory above it is a reparse point, and each is owned by
  `SYSTEM`, `Administrators`, or `TrustedInstaller`. No principal other than
  those three may delete or rename it or any directory above it: no access
  control entry that applies to one of these directories itself, rather than
  only to what it holds, grants anyone else `DELETE`, `WRITE_DAC`,
  `WRITE_OWNER`, or a generic right that includes one of them, on it, or
  `FILE_DELETE_CHILD` on its parent.
- It holds a host file whose first line is `alloy-host 1`.

`init` and `join` use a data directory that is an Alloy data directory or that
does not exist. They create a missing one with its owner and its mode or access
control list set when it is created, on Windows in the call that creates it,
and write the host file in it before anything else. On Linux they also adopt an
empty directory that is not a symbolic link, is owned by user id 0, is not
writable by its group or by others, and whose parent directories meet the rule
above, as a mount point often is, and set its mode to `0700` first. Any other
directory or file at the path fails the `data directory` check of section 3.8,
and nothing is written in it. On Windows no existing directory is adopted,
whoever owns it, since a local user may have created it or may hold it open.
The local join state that `init`, `join`, `leave`, and `alloyd` act on is the
`join` line of the host file of an Alloy data directory, and is `unjoined` when
the path holds none.

Every file and directory that Alloy writes under the data directory grants no
access beyond the data directory's own. `alloyd` checks at every start, before
it reads any secret, that its data directory is an Alloy data directory. When
it is not, `alloyd` reads no secret, serves nothing, starts no process, writes
nothing, and logs the reason every 60 seconds.

`alloy` and `alloyd` do all of this through the `disk-io` seam (ARFC 0005
clause 3.1.15). The `root` of their `os` instance (ARFC 0005 section 4.2) is
`/` on Linux and, on Windows, the root of the volume that holds
`%ProgramData%`, and they name the data directory by `rt_path` of its native
path. They read each owner, mode, access control list, and link or reparse
point fact the rules name with `security`, which does not follow a link at
the path itself, on the data directory, its host file, and each directory
above it up to the root, one `security` call per entry. When `security`
returns an error for any entry it reads, the entry fails the rules: `init`
and `join` exit 5 and `alloyd` reads no secret, as for any other failure,
and the message names that entry and the error, and names the entry's owner
as `unknown` when it could not be read. They create the data directory with
`Protection::PrivateRoot`, and every file and directory under it with
`Protection::Private`, which on Windows gives it owner `Administrators` and
the list it inherits from the data directory. On Linux they set an adopted
directory's mode with `set_mode` and complete `sync_dir` of it before writing
in it. A file under the data directory that a library opens itself, such as a
`native` store file (ARFC 0006 clause 5.2), is first created through the seam
as ARFC 0005 clause 3.1.15 describes.

On Windows, after an unprivileged local user creates the directory `Alloy` in
`%ProgramData%`, `alloy join` must exit 5 naming that directory and its owner,
and the directory must stay empty. Without that directory, `join` creates it,
and `icacls` on it lists only `SYSTEM` and `Administrators`, each with full
control and neither inherited. After an administrator adds to `%ProgramData%`
an entry that grants the local group `Users` `FILE_DELETE_CHILD` on it, `alloy
join` must exit 5 naming `%ProgramData%` and that group, and `alloyd`, when it next starts,
must read no secret and log the same reason. After an unprivileged local user
creates the directory `Alloy` in `%ProgramData%` with a list protected from
inheritance that grants only that user, `alloy join` must exit 5 naming that
directory and its owner as `unknown`, and the directory must stay empty. On
Linux, when `/var/lib` is a symbolic link to a directory that meets every
other rule, `alloy join` must exit 5 naming `/var/lib`, and write nothing.

### 3.13 Node channel

The **node channel** carries every message between a node's `alloyd` and the
control plane: the join of section 3.3 steps 5 to 7 and its approval, the
renewal of section 3.5, the withdrawal and leave of section 3.9, and the status
reports of ARFC 0006. It runs inside AL sessions (ARFC 0003) and needs no `al0`
interface and no path but the session's own.

**Carriage.** A node message is one `NodeMessage` below, encoded as Protocol
Buffers (proto3). One that holds a `forwarded` is at most 16,512 bytes, and
every other at most 16,384, so that a `forwarded` holds any request or reply,
since its other fields take at most 66 bytes. The bounds that the definitions
below state on strings and lists keep every message this section defines
within 16,384 bytes. It is the payload of one UDP datagram
(RFC 768) in one IPv6 packet (RFC 8200), sealed as an IPv6 frame on VNI 0 of an
AL session (ARFC 0003 section 3.4), and split by AL when it exceeds the payload
budget (ARFC 0003 section 3.5). The IPv6 header has traffic class 0, flow label
0, next header 17, hop limit 64, and no extension header. The source address is
the sender's infrastructure address and the destination the receiver's (section
3.4). Both UDP ports are 7477. The UDP checksum is computed as RFC 8200 section
8.1 says. A receiver accepts a node message only when its source address is the
infrastructure address that the mesh key the session authenticated has in the
receiver's cluster, its destination is the receiver's own infrastructure
address, both ports are 7477, the checksum verifies, and the payload decodes
and is within the bounds of this section. It drops anything else silently. A
joiner takes the `cluster_id` for both addresses from its token.

**Who speaks.** The node that sends a request is its **requester**, and the
control node that handles it answers. A joiner, and a node renewing an expired
record (section 3.5), sends node messages only on the session it opened to an
anchor as an unregistered initiator, and only the requests of the join service
below. An anchor sends on such a session only at the initiator's current
endpoint (ARFC 0004 section 3.1.2). An anchor that holds `control` handles a
join service request itself. One that does not forwards it to a control node,
as below. A node that holds a valid record sends its node messages only to a
control node, on an AL session it holds with that control node, and the
control node handles them itself: no other node carries them, and an anchor
without `control` drops them. A node that holds
`control` handles its own requests in its own process and sends no node message
for them.

**Control node choice.** A node with a valid record sends each request to one
control node of its latest view (the bootstrap view of section 3.3 step 7 or a
later one) that it can reach (This version, below): the one that last replied
to it, or, until one has, the first in the view's order. When a request gets no
reply within its time limit, or gets a `NodeStatusReply` with result
`UNAVAILABLE` (ARFC 0006 section 4.3), its next request goes to the next such
control node in the view's order, wrapping around. This is all a node keeps for
choosing.

**Forwarding.** An anchor without `control` sends a join service request that
arrives on a session from an unregistered initiator to a control node as a
`forwarded`: the request's `NodeMessage` unchanged, its `request_id` included,
with the mesh key that the session authenticated and the source address and
port of the request's underlay packet. The `NodeMessage` that holds a
`forwarded` has `request_id` 0. The control node answers with a `forwarded`
under `request_id` 0 that holds the same mesh key, no source address, source
port 0, and the reply under the wrapped request's `request_id`. The anchor sends
that reply to the node with that mesh key, on a session it holds with that
key, and drops it when it holds none. An AL rekey between the request and the
reply therefore loses no reply, and neither does the joiner's record becoming
valid, which makes its session a home session (section 3.3 step 7). The anchor
forwards to one control node of its latest view: the one that last sent it a
`forwarded` reply, or, until one has, the first in the view's order. When 3
seconds pass after it forwarded a request to that control node with no
`forwarded` reply from it since, it moves to the next control node in the
view's order, wrapping around. Beyond that one control node and that one time,
the anchor keeps nothing per forwarded request and holds no queue: a request it
cannot send at once is dropped, and the node's resend 1 second later carries
it. An anchor forwards at most 2,000 requests per second, and drops the rest,
which their resends carry later.

A control node accepts a `forwarded` only from a node that holds a valid record
with `anchor`, and only when its wrapped message is a join service request. It
treats the mesh key the `forwarded` names as the one an unregistered
initiator's session authenticated, and the source address and port as the
request's. An anchor accepts a `forwarded` only from a node that holds a valid
record with `control`. A `NodeMessage` that holds a `forwarded` and has a
`request_id` other than 0, and a `forwarded` whose wrapped message holds a
`forwarded`, are dropped.

The mesh key a `forwarded` names is the anchor's word, so no forwarded request
acts for a joiner or a node on that word alone. `join_hello` and `join_state`
change no state, and write no audit record but that of a `version` refusal,
which names no node. A `join_request` acts only under the token key's and the
node identity key's signatures over that mesh key and every byte of its body
(section 3.3 step 6). It carries neither key nor the token's secret, so an
anchor can neither change a `join_request` it carries nor send it under another
mesh key, and makes none of its own for a mesh key it does not hold unless it
holds a token by other means. One whose keys a request or node already holds is
a retry, which creates nothing, or is refused. A `renew` issues a record only under the
node's identity key signature over the serial of the node's current record,
so a `renew` once handled issues nothing again, before or after that record
expires (section 3.5). A `withdraw` deletes only under the identity key
signature of the request or node it deletes, over that request's or node's
mesh key (section 3.9), and the joiner removes those keys once it has sent
it, so the same `withdraw` sent again deletes only what that key's holder
asked to delete. These rules are the same for a request that comes without a
`forwarded`.

**Join service.** On a session from an unregistered initiator an anchor accepts
only keepalives, probe messages (ARFC 0004 section 3.1), and node messages
whose body is `join_hello`, `join_request`, `join_state`, `renew`, or
`withdraw`, the **join service requests**. It drops every other frame and
message silently. It takes at most
1 new request per second per such session and 100 per second over all such
sessions. A request beyond that is dropped, and its resend is taken later.
`leave` and the status messages of ARFC 0006 are accepted only on a session
whose peer holds a valid record, and never in a `forwarded`, so a control node
takes them only from the node they concern. A
`withdraw` deletes the `pending` request, or the `joining` node, that holds the
session's mesh key, only when its `signature` verifies against that request's
or node's `identity_pub` over the bytes of section 3.9 with that mesh key. It
is answered `DONE_CODE_OK` whether or not one existed and whether or not the
signature verified.

**Requests and replies.** A node takes each new `request_id` from its handshake
counter (ARFC 0003 section 3.2): the request uses the next counter value v that
no initiation and no request has used, as an initiation would, and its
`request_id` is v + 1. So the `request_id`s a node sends are never 0 and
only grow, across AL rekeys, new sessions, other peers, and restarts of
`alloyd`. A node has at most one request outstanding, to any peer. It sends the
same request again, byte for byte and to the same peer, every 1 second until a
reply with its `request_id` arrives or the time limit of the step that sent it
passes, counted from its first send. Only these end a request: a node sends a
new request only once the one before has ended, so no request ends another. A
request that falls due while another is outstanding waits until that one ends.
A request's **kind** is its `body` field, and the requests a node sends are of
7 kinds: `join_hello`, `join_request`, `join_state`, `renew`, `withdraw`,
`leave`, and `node_status` (ARFC 0006 section 4.3). A node holds at most one request of each
kind, outstanding or waiting, so at most 6 wait. A request that falls due while
the node holds one of its kind is not added: the one held stands for it, and
whatever asked for it, a schedule or a command such as a second `alloy node
leave`, takes that one's outcome. When the outstanding request ends, a waiting
heartbeat (ARFC 0006 clause 3.13) is sent first, then the other waiting
requests in the order they fell due.
A reply whose `request_id` is not that of the node's outstanding request is
dropped, so a reply that arrives after its request's time limit is not taken.
No step's time limit is over 10 seconds, so no request is sent more than 10
seconds after it was first sent.

A control node keeps one **entry** per **origin**: the highest `request_id` it
has taken from that origin and, once it exists, that request's reply. The
origin of a request is the mesh key that the AL session it arrived on
authenticated, and for a request in a `forwarded`, that key together with the
mesh key the `forwarded` names. An origin is made of mesh keys, never of an AL
session: an AL rekey (ARFC 0003 section 3.3) makes a new session but leaves
every origin, every entry, and the order of `request_id`s unchanged, and so does
a new session after one closes. A request with an entry's id gets the kept
reply again, or nothing while the reply does not yet exist, and is never
handled twice. A lower id is dropped. A higher id, or any id when its origin has
no entry, is handled, and its reply replaces the kept one. An entry is
forgotten 30 seconds after the last request for it arrived, when no resend of a
request it took can still come. A control node keeps at most 8,192 entries
whose origin is one mesh key, and at most 8,192 for requests that one anchor
forwarded: a request that would add an entry beyond its bound is dropped, and a
resend adds it once an entry is forgotten. The control plane replies within 10
seconds of first taking a request.

A request is therefore handled at most once by one control node while that
control node's `alloyd` runs: a resend counts no second token use and writes
no second audit record. Only two cases can handle a request twice: a forwarded request whose
anchor moves to another control node between its resends, and any request
whose resend reaches a control node that restarted in between. Handling a
request again then has this outcome. `join_hello`, `join_state`, and `withdraw`
change nothing more. A `join_request` already admitted or `pending` is a retry
(section 3.3 step 6). A `join_request` refused once is checked again from the
start, and its refusal writes one more audit record and, when it is
`token_refused`, counts one more use. A `renew` issues no second record (section 3.5). A status message
commits nothing a second time (ARFC 0006 clause 3.14). A `leave` whose node was
deleted finds no session to come on, so it gets no reply (section 3.9).

```proto
syntax = "proto3";
package alloy.node.v1alpha1;   // proto/alloy/node/v1alpha1/node.proto

message NodeMessage {
  uint64 request_id = 1;
  oneof body {
    JoinHello join_hello = 2;          // node to anchor
    JoinHelloReply join_hello_reply = 3;
    JoinRequest join_request = 4;      // node to anchor
    JoinReply join_reply = 5;          // answers join_request, join_state, renew
    JoinState join_state = 6;          // node to anchor
    Renew renew = 7;                   // node to anchor, or to control node while its record is valid
    Withdraw withdraw = 8;             // node to anchor
    Leave leave = 9;                   // node to control node
    Done done = 10;                    // answers withdraw, leave
    Forwarded forwarded = 11;          // anchor without control to control node, and the reply back
    // Numbers 16 to 31 are the status messages of ARFC 0006.
  }
}
enum Role { ROLE_UNSPECIFIED = 0; ROLE_ANCHOR = 1; ROLE_CONTROL = 2;
            ROLE_COMPUTE = 3; ROLE_STORAGE = 4; }
message NodeRecord {
  bytes cluster_id = 1;              // 16 bytes
  bytes node_id = 2;                 // 16 bytes
  string name = 3;
  bytes identity_pub = 4;            // 32 bytes
  repeated bytes mesh_pub = 5;       // 32 bytes each, 1 or 2 (section 3.6)
  repeated Role roles = 6;           // ascending, no repeats, empty for a client node
  string zone = 7;
  bytes infra_addr = 8;              // 16 bytes
  repeated bytes advertise = 9;      // 4 or 16 bytes each, 0 to 16
  uint32 mesh_port = 10;
  uint32 api_port = 11;
  int64 not_after = 12;              // seconds since 1970-01-01T00:00:00Z
  uint64 serial = 13;
}
message SignedNodeRecord { bytes record = 1; bytes signature = 2; }  // 64-byte signature
message JoinHello { string release = 1; }  // MAJOR.MINOR.PATCH with any pre-release suffix
message JoinHelloReply { oneof result { VersionRefusal version = 1; ClusterProof cluster = 2; } }
message VersionRefusal { repeated string accepted_minors = 1; string highest_release = 2; }
message ClusterProof {
  bytes cluster_pub = 1;             // 32 bytes
  SignedNodeRecord anchor_record = 2;
  bytes previous_cluster_pub = 3;    // empty outside a rotation overlap
  bytes cross_signature = 4;         // section 3.6, empty outside a rotation overlap
}
message Disk { string path = 1; uint64 size_bytes = 2; }   // path at most 255 bytes
message Facts { string os = 1;                  // at most 64 bytes
                string arch = 2;                // at most 32 bytes
                uint32 cores = 3; uint64 memory_bytes = 4; bool virtualization = 5;
                repeated Disk disks = 6; }      // at most 32
message JoinRequest {
  bytes body = 1;                    // JoinRequestBody, encoded (section 3.3 step 6)
  bytes token_signature = 2;         // 64 bytes, by the token key over the join message
  bytes identity_signature = 3;      // 64 bytes, by the node identity key over the join message
}
message JoinRequestBody {
  bytes token_id = 1;                // 8 bytes
  reserved 2;                        // no node message carries a token's secret (section 3.2)
  bytes identity_pub = 3;            // 32 bytes
  reserved 4;                        // the mesh key is the session's (section 3.3 step 6)
  string name = 5;                   // at most 63 bytes
  bool name_is_default = 6;
  repeated Role roles = 7;
  string zone = 8;                   // at most 63 bytes
  repeated bytes advertise = 9;      // as in NodeRecord
  uint32 mesh_port = 10; uint32 api_port = 11;
  Facts facts = 12;
}
message JoinState {}
message Renew { bytes node_id = 1; uint64 serial = 2; bytes signature = 3; }  // section 3.5
message JoinReply { oneof result { Admitted admitted = 1; Pending pending = 2; Refusal refusal = 3; } }
message Admitted { SignedNodeRecord record = 1; BootstrapView view = 2; }
message Pending {}
message BootstrapView { bytes cluster_pub = 1; repeated SignedNodeRecord nodes = 2; }
enum RefusalReason {
  REFUSAL_REASON_UNSPECIFIED = 0; REFUSAL_REASON_TOKEN_REFUSED = 1;
  REFUSAL_REASON_KEY_IN_USE = 2; REFUSAL_REASON_TOKEN_REVOKED = 3;
  REFUSAL_REASON_TOKEN_EXPIRED = 4; REFUSAL_REASON_TOKEN_USED_UP = 5;
  REFUSAL_REASON_PRESET_CONFLICT = 6; REFUSAL_REASON_COMPUTE_UNSUPPORTED = 7;
  REFUSAL_REASON_NAME_TAKEN = 8; REFUSAL_REASON_BUSY = 9;
  REFUSAL_REASON_REJECTED = 10; REFUSAL_REASON_NODE_UNKNOWN = 11;
}
message Refusal { RefusalReason reason = 1; int64 expire_time = 2; }  // expire_time only for TOKEN_EXPIRED
message Withdraw { bytes signature = 1; }  // 64 bytes, section 3.9
message Leave { bytes node_id = 1; bytes signature = 2; }  // section 3.9
enum DoneCode { DONE_CODE_UNSPECIFIED = 0; DONE_CODE_OK = 1; DONE_CODE_CONFLICT = 2; }
message Done { DoneCode code = 1; string reason = 2; }
message Forwarded {
  bytes mesh_pub = 1;                // 32 bytes, the key the requester's session authenticated
  bytes source_address = 2;          // 4 or 16 bytes; empty in a reply
  uint32 source_port = 3;            // 0 in a reply
  NodeMessage message = 4;           // a join service request or its reply, request_id 1 or more
  reserved 5;                        // origins are mesh keys, not sessions
}
```

The bootstrap view holds the cluster's current public key and the records of
its anchors and control nodes, at most 16, those holding `control` first, then
by name. A node takes its homes from them (ARFC 0004 section 3.1.2).

**This version.** Before the mesh of ARFC 0004 runs (M11 rung 3), the node
channel on home sessions and on sessions from unregistered initiators is the
only traffic between nodes. It goes directly over the underlay: over UDP, or
`stream`, to an anchor's published address. A node then reaches a control node
only over a home session whose anchor holds `control`, and an anchor without
`control` reaches no control node, so it forwards nothing, and a node none of
whose homes holds `control` cannot reach the control plane. M11 rung 2 runs
with every anchor holding `control`, and needs no forwarding. Relay envelopes, signaling, probe sets and direct paths between nodes,
`al0` with the infrastructure address on hosts (section 3.4), and every other
use of the infrastructure network wait for the mesh. The API is never reached
over the mesh (ARFC 0008 section 3.8). Once `al0` exists, node messages to and
from a node's own infrastructure address travel through it like any other
packet on VNI 0, with the same bytes, on the AL session that the mesh holds
with the peer, over a direct or a relayed path (ARFC 0004). From then on a node
reaches every control node of its view, and an anchor without `control` the
control node it forwards to, that way, and the bytes and rules of this section
do not change.

### 3.14 Stored data

Each node keeps these files in its data directory (section 3.12), each written
whole: Alloy writes `<name>.new`, flushes it to disk, and renames it over
`<name>` with `replace` of the `disk-io` seam (ARFC 0005 clause 3.1.16), which
returns once the rename is durable, before it writes any other file. `replace`
makes the rename durable on Linux by `fsync` of the data directory, since
`fsync` of a file "does not necessarily ensure that the entry in the directory
containing the file has also reached disk"
(https://man7.org/linux/man-pages/man2/fsync.2.html), and on Windows by
renaming with `MoveFileExW` and the flags `MOVEFILE_REPLACE_EXISTING` and
`MOVEFILE_WRITE_THROUGH`, which returns only once the file is moved on the disk
(https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-movefileexw).
So after a crash each file holds either its last durable content or its
content before that write, and the writes reach the disk in the order they
were made. Readers ignore `.new` files.

| File | Content | Present |
|---|---|---|
| `alloy-host` | The host file (ARFC 0017 section 4.2) | Always, written first |
| `at-rest.key` | 32 bytes from the `alloy-rt` randomness trait, the node's at-rest key (ARFC 0020 clause 3.7). During an at-rest key rotation, 64 bytes: the old key, then the new | From the first key generation until `leave` |
| `secrets` | A 24-byte nonce, then the XChaCha20-Poly1305 encryption under the at-rest key, with that nonce and the 24 ASCII bytes `alloy/at-rest/secrets/v1` as associated data, of `NodeSecrets` below | With `at-rest.key` |
| `handshake-counter` | 8 bytes, little endian: the end of the highest block of handshake counter values reserved (ARFC 0003 section 3.2) | With `at-rest.key` |
| `node` | `NodeState` below, not secret | While `joining` or `joined` |
| `disks/0` | The `file` disk (section 3.1) | Nodes with `storage` |

Each write of `secrets` draws a new nonce from the `alloy-rt` randomness trait.
A 192-bit random nonce repeats under one key with probability below 2^-128 for
fewer than 2^32 writes, and a node writes `secrets` only when what it holds
changes. When `at-rest.key` holds 32 bytes, `secrets` opens under that key.
When it holds 64, `alloyd` opens `secrets` under whichever of the two keys
authenticates it, and finishes the rotation below before it does anything
else. A `secrets` file that opens under no key `at-rest.key` holds, or an
`at-rest.key` of any other length, is not read, and `alloyd` then logs that the
data directory is damaged and serves nothing.

**At-rest key rotation.** `alloy node rotate` replaces the at-rest key in three
writes, each made durable before the next:

1. `at-rest.key` becomes the old key followed by the new key, 64 bytes.
2. `secrets` is written under the new key.
3. `at-rest.key` becomes the new key, 32 bytes.

A crash before write 1 leaves the old key and `secrets` under it. A crash after
write 1 and before write 2 leaves both keys and `secrets` under the old one. A
crash after write 2 and before write 3 leaves both keys and `secrets` under the
new one. A crash after write 3 leaves the new key and `secrets` under it. In
each case `secrets` opens. When `alloyd` starts with 64 bytes in
`at-rest.key`, it finishes the rotation: from write 2 when `secrets` opened
under the old key, else from write 3. The old key is gone from the data
directory once write 3 is durable.

```proto
message NodeSecrets {
  bytes identity_key = 1;          // Ed25519 seed, 32 bytes
  bytes mesh_key = 2;              // X25519 private key, 32 bytes
  bytes previous_mesh_key = 3;     // during a node key rotation (section 3.6)
  string join_token = 4;           // while joining: the token string
  JoinRequest join_request = 5;    // while joining: the request of section 3.3 step 6
  bytes cluster_key = 6;           // control nodes: Ed25519 seed
  bytes previous_cluster_key = 7;  // control nodes, during a rotation overlap
  bytes api_ca_key = 8;            // control nodes: PKCS#8 DER (RFC 5208)
  bytes api_server_key = 9;        // control nodes: PKCS#8 DER
}
message NodeState {
  bytes cluster_id = 1;
  repeated bytes cluster_pub = 2;  // current, then previous during an overlap
  SignedNodeRecord record = 3;     // once joined
  BootstrapView view = 4;          // once joined
  bool pending = 5;                // a `pending` reply was received
  bytes api_ca_certificate = 6;    // control nodes: DER
  bytes api_server_certificate = 7; // control nodes: DER
}
```

`join_token` and `join_request` are cleared in the write that records the
node's record, or the local join state `unjoined`. A failed attempt (section 6)
keeps `at-rest.key`, `secrets` with only the node keys, `handshake-counter`, and
the host file, and removes `node`. `leave` removes every file but the host
file.

Key lifetimes: the node identity and mesh keys last until `alloy node rotate`
or `leave`, the at-rest key until `leave` or the next `alloy node rotate`,
which replaces it as above. The cluster key lasts until rotated (section 3.6).
The key of the self-signed `stream` certificate of an anchor without `control`
(ARFC 0004 section 3.6) is in no file: it is held in memory from its generation,
when `alloyd` starts or the certificate is renewed, until the next renewal or
until `alloyd` stops. The API server key lasts as long as its
certificate, at most 30 days (section 2). The API CA key lasts as long as its
certificate, 10 years.

The control plane keeps in its replicated state (ARFC 0006), and nowhere else:
each node with its signed record bytes, its state, its facts, and the
`token_id` that admitted it; each `pending` request, with
the mesh key and source address it came from; each join token as `token_id`,
`token_pub`, `cluster_key_fp`, expiry time, use limit, uses, preset roles
and zone, and whether it is revoked; each login code as `code_id`,
`BLAKE3(secret)`, its user, `issued_by`, expiry time, and whether it is spent or
voided; and the API CA certificate. Join tokens are resources of kind
`JoinToken` (ARFC 0006 section 2.9), and login codes of kind `LoginCode` (ARFC
0007 section 2.5). No token, code, or access key secret is kept in clear. A
client node keeps the files of the table and nothing of the control plane's.

### 3.15 First deployment

M11 rung 2 (ADR 0068) builds this ARFC except what this section leaves out.
Rung 2 is released only as preview releases, and a cluster on a preview never
changes release (ARFC 0017 clause 3.13), so what a later rung adds needs no
change to a cluster that rung 2 made: that rung's release runs `init` and
`join` on clean machines.

**Init.** Steps 1, 2, 4, and 5 as written. Step 3 generates and stores every
key and certificate it names, as written, and creates of its objects:

| Object | In rung 2 |
|---|---|
| node, control plane, join token, login code | As step 3 says. The token is a `JoinToken` (ARFC 0006 section 2.9) |
| organization | `default`, enabled (ARFC 0007 section 2.5) |
| user | `admin`, enabled, in `default`, with the cluster role `cloud-admin` and no role of `default` |
| project | None |
| pool | None, and no `disks` directory or `disks/0` in the data directory |

The node holds every role step 3 names, `storage` included, though rung 2 runs
no compute or storage service. Nothing `init` prints names a project or a
pool: step 5 prints the join token, the login code, and the standing warnings,
and rung 2 reports no standing warning, so `init` prints none. The exchange of
the login code returns an empty `project`, since `default` holds no project
named `default` (section 3.7), and `alloy login` then makes a context with no
default project (ARFC 0019 clause 3.1.11).

**Join.** Steps 1, 2, and 4 to 8 as written, except that in step 7 no disk
joins a pool, since there is none. There is no approval: rung 2 has no setting
`join_approval`, every request that passes the checks of step 6 is admitted,
no control node answers `pending`, and no joiner sends `join_state`. The refusals
`busy` and `rejected` therefore never occur.

Step 3 opens the AL session as written, but no node of rung 2 sends or answers
a reflection request, on that session or on any home session (ARFC 0004 clause
3.1.2). In place of the current endpoint that an echo sets, an anchor sends on a
session over UDP only to the source endpoint of the handshake initiation it
answered to make that session, and only from the moment it accepts, from that
same endpoint, a transport packet or probe message under the session, which
confirms the session (ARFC 0003 section 3.6). The initiator seals a keepalive
as soon as it makes the session (ARFC 0003 section 3.4), so this takes one
round trip after the response. A session that the anchor itself initiates
with the node, at a rekey, takes the endpoint of the session it rekeys. No
packet changes a session's endpoint later, and it never lapses. A session
confirmed by a packet from another endpoint gets nothing from the anchor, and
a node whose endpoint changes is reached again only on a session it opens from
its new endpoint. Only the initiator that received the response can make a
packet that authenticates under the session, so a party that cannot read the
node's packets moves nothing. A session over `stream` follows ARFC
0004 section 3.6 as written. From rung 3, which builds `alloy-relay`, step 3
and ARFC 0004 clause 3.1.2 apply as written.

**Other sections.**

- Section 3.2: the token string, its checks, and the token key as written. The
  methods `CreateJoinToken`, `ListJoinTokens`, and `RevokeJoinToken`, and the
  deletion 7 days after expiry, are not built, so the token that `init`
  creates is the cluster's only one.
- Section 3.4: the infrastructure address is computed as written and used as
  the source address of audit records. `al0` waits for rung 3 (section 3.13).
- Section 3.5: as written, except the grace statement.
- Section 3.6 and the at-rest key rotation of section 3.14: not built, and
  neither is `alloy node rotate`.
- Section 3.7: as written, except that a login code is issued only by `init`
  and through the local admin socket, never on the API port (below).
- Section 3.9: not built, nor `withdraw`. The rung 2 check clears a machine by
  an uninstall with `--purge --force` (ARFC 0017 clause 3.16 step 4), which goes
  on when `alloy node leave` fails.
- Section 3.10: advertise addresses are set by `init` and `join` only. The watch
  of addresses that are gone and its warning are not built.
- Sections 3.8, 3.11 to 3.14: as written, without `disks/0`.

**API and CLI.** The API port serves `ListNodes` and `ExchangeLoginCode`, and
answers `IssueLoginCode` with `PermissionDenied`. The local admin socket serves
`ListNodes` and `IssueLoginCode`, and answers `ExchangeLoginCode` with
`PermissionDenied` (ARFC 0007 clause 3.10.2). Each serves by the rules of its
listener (ARFC 0008 clauses 3.7 and 3.7.2). A call of any other method, on
either, ends with gRPC status `UNIMPLEMENTED` (12), which gRPC gives a method
the server does not implement
(<https://grpc.github.io/grpc/core/md_doc_statuscodes.html>), and no
`alloy-error-bin` trailer. No standing warning is reported (ARFC 0019 clause
3.2.5). Of the CLI (ARFC 0019), rung 2 builds `init`, `join`, `alloy login
<login code>`, `alloy login --issue-code` through the local admin socket (clause
3.1.14), and `alloy node list`. On a new cluster, `init` prints one login code,
user id 0 on the control node issues one for each other machine with `alloy
login --issue-code`, and each machine, the control node included, runs `alloy
login <code>` once and can then run `alloy node list` (section 3.7).

## 6. Failure behavior

A join attempt fails when it gets a refusal, when no address passes section 3.3
step 5 within 10 seconds, or when no answer to step 6 arrives within 10 seconds.
Receiving `pending` is not a failure. A failed attempt leaves on the joiner only
its generated keys, its handshake counter (ARFC 0003 section 3.2), and its host
file listing no object, with every process of ARFC 0000 section 2.3 stopped,
`alloyd` disabled, and the local join state `unjoined`. The next
attempt reuses both, so that its initiations are not dropped as replays.

A retry with the same token and those keys must get the node's current record,
or `pending`, without a second record and without a second counted use, when an
earlier attempt was admitted or is still `pending` (section 3.3 step 6),
whatever the token's state since. Otherwise a retry must succeed when the token
is still valid. When `alloyd` restarts in local join state `joining`, it resumes
the attempt at section 3.3 step 3 with the same keys and request, or, once it
has received `pending`, by asking for the request's state.

A node whose record expires while it is still `joining`, because the joiner
never received the record or never reported, must be deleted by the control
plane within 60 seconds after the record's `not_after`, with an audit record,
and its name becomes free. A later attempt with the same keys is then a new join.

A `join_request` refused with `token_refused` for a known `token_id` must count
as a use, and every `token_refused` must be logged in the audit log with the
source address. A refusal for any other reason counts no
use (section 3.3). An `init` that fails leaves the machine `unjoined` (section
3.1). A failed `ExchangeLoginCode` spends the code only as section 3.7 says, and
creates and revokes no access key (ARFC 0007 section 6).

## 7. Limits

4 anchors per token and 4 addresses per anchor in a token. 16 advertise
addresses per node. Token string length at most 512 characters, login code at
most 226. 10,000 live tokens per cluster: `CreateJoinToken` beyond that fails
with `QuotaExceeded` naming the limit and creates nothing. 10,000 kept expired,
revoked, or used up tokens, the oldest deleted first at the bound. 1,000
`pending` requests per cluster, then `busy`. 10 unused login codes per user, the
oldest deleted at the bound, and 10,000 kept login codes per cluster, the oldest
deleted first at the bound. `ExchangeLoginCode`: 10 calls per second per source
address or IPv6 /64, and 100 per second in total, then `RateLimited` (section
3.7).

Node channel (section 3.13): 16,384 bytes per node message, and 16,512 for one
that holds a `forwarded`. In `Facts`, 64 bytes of `os`, 32 of `arch`, and 32
disks with paths of at most 255 bytes. One outstanding request at the node,
at most one request of each of the 7 kinds outstanding or waiting there, so
at most 6 waiting, a request of a kind already held not added, and one control
node to try first. At a control node, one entry, a
kept `request_id` with its reply, per origin, each forgotten 30 seconds after
its last request: at most 8,192 entries whose origin is one mesh key, and 8,192
for the requests of each anchor that forwards; a request that would add an
entry beyond its bound is dropped. At an anchor without `control`, one control
node to forward to and one time, nothing per request, and 2,000 forwarded
requests per second, the rest dropped.
On sessions from unregistered initiators, 1 new request per second per session
and 100 per second per anchor, the rest dropped. 16 records in a bootstrap
view.

## 8. Security

A join token is a bearer secret until it expires or is revoked. It grants the
ability to add a node, which is cluster-level trust. A login code is a bearer
secret for its user until it is spent, voided, or expires. Both secrets are
generated where the string is assembled and never appear in an API response.
The control plane stores a login code's secret only as `BLAKE3(secret)` and a
token's only as `token_pub` (section 3.2), and no node message carries a
token's secret. The access key secret
that an exchange creates is returned by that exchange alone, as ARFC 0007 clause
3.9.4 says. A joiner sends `join_request` only after it has authenticated the
anchor's mesh key and checked the cluster key against the token, and a CLI sends
a login code only after the API certificate verified against `api_ca_fp`.
Refusal reasons are given only after the token's or the code's secret is
proven, so a guessed `token_id` or `code_id` learns nothing. A retry is
recognized only by a mesh key that the AL session authenticated together with
signatures by the token key and by the admitted node's identity key over that
mesh key, so it hands a record to no one but the holder of the admitted node's
keys, and so
does the renewal of an expired record, which also needs the node to exist. The
node's secrets sit in a data directory that only `SYSTEM` and `Administrators`,
or user id 0, can open (section 3.12), and that `init`, `join`, and `alloyd`
refuse to use otherwise. A client
node is bounded as ARFC 0020 clause 3.3 bounds a compromised compute node that
serves no guest, volume, or VPC: holding no role, it can read only traffic
addressed to itself and can write only its own status.
Unauthenticated exchange calls are bounded per source and in total before they
reach the store or the audit log, and so are join service requests (section
3.13). A node message is accepted only from the infrastructure address of the
mesh key its AL session authenticated. A node with a valid record speaks only
for itself: a control node takes `leave`, the status messages of ARFC 0006,
and every other request from a node with a valid record only on an AL session
that node authenticated, never in a `forwarded`, so no node, an anchor
included, can send them for another, and a heartbeat that an anchor forges for
another node commits nothing and writes no audit record (ARFC 0020 clause
3.3). What an anchor without `control` can assert for another node is bounded
to the join service: a `forwarded` names the mesh key, source address, and
port of a join service request, and a control node believes them from an
anchor's valid record alone, but no request acts on that word alone (section
3.13). A compromised anchor can therefore read the state of a `pending` request
or a `joining` node under any mesh key, and drop or delay the requests it
carries. It learns no token's secret from a `join_request` it carries, and
cannot change one or send it under another mesh key, since both its signatures
cover that mesh key and every byte of its body. So it cannot set a joiner's
name, roles, zone, advertise addresses, ports, or facts, in a record or in the
view an approving cloud admin sees, and cannot make a joiner's own request a
retry of one it made, since a retry needs the joiner's identity signature
(section 3.3 step 6). It can send a carried `join_request` again, unchanged,
while its token is usable. While the request or node that request made exists,
that is a retry, which changes nothing. Once that request or node is gone, it
makes again, under the same keys, the request or node that request asked for,
which counts a use of the token. When the joiner has withdrawn or left, nobody
holds those keys any more: such a node opens no session and is deleted once
its record expires (section 6), and such a request lasts until a cloud admin
approves or rejects it or its token expires. Revoking the token ends this.
Only with a token it holds by other means can it make a
`join_request` of its own, and under a mesh key it does not hold such a request
makes, with its own identity key, a request or node that only that key's holder
could use, and refuses that key's own join with `key_in_use` until that request
or node is deleted. It cannot withdraw
a request or a node, or renew a node, without a signature by that request's or
node's identity key, and it cannot make one such signature act twice: a
`renew` already handled issues nothing, whether or not the record it issued has
since expired, and a `withdraw` sent again deletes only what that key's holder
asked to delete (sections 3.5 and 3.13). A `renew` it holds back and sends
later issues at most the one record its node asked for, which that node,
holding its keys, can get by itself at any time while it exists. It cannot
report status for any node but itself, or put an address other than its own
infrastructure address in the audit record of a request it forwards (ARFC 0018
clause 3.3.7). A `forwarded` reply is believed only from a node whose valid record holds
`control`. The node channel
reaches no endpoint that the anchor has not seen echo a challenge (ARFC 0004
section 3.1.2), so a spoofed or copied packet cannot point the bootstrap view or
any other reply at a third party. Secrets at rest are sealed under the node's
at-rest key (section 3.14, ARFC 0020 clause 3.7). On Windows no principal but
`SYSTEM`, `Administrators`, and `TrustedInstaller` can delete, rename, or
replace the data directory or any directory above it (section 3.12).
Upholds I-09, I-14, I-15, I-21, I-22.

## 9. Open questions

- Threshold custody of the cluster key across control nodes. Research task to be
  cut in M2.
- Hardware-bound node keys (TPM). Research task to be cut in M10.
- How a second control node receives the cluster key and the API CA key, and
  how the API CA certificate rotates. Spec task to be cut with multi-voter
  membership (ARFC 0006 clause 3.2).

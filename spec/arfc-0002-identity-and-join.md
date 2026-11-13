# ARFC 0002: Identity and join

- Status: accepted
- Depends on: 0000, 0003, 0007, 0009, 0017
- Crates: alloy-id, alloy-crypto
- Decisions: 0012, 0015, 0018, 0040, 0041, 0048, 0062

## 1. Purpose

Defines what a cluster and a node are cryptographically, how the first node
creates a cluster and hands its first credential to a cloud admin, how one token
admits another machine, and how a machine leaves.

## 2. Model

| Key | Type | Held by | Purpose |
|---|---|---|---|
| cluster key | Ed25519 | control plane | Signs node records and the cluster map |
| node identity key | Ed25519 | node | Signs the node's requests to the control plane |
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

A **node record** is `{node_id, identity_pub, mesh_pub, roles, zone, infra_addr,
advertise, capabilities, not_after, serial}` signed by the cluster key. `node_id`
is the first 16 bytes of `BLAKE3(identity_pub)`. `advertise` is the node's list
of advertise addresses (section 3.10). Peers accept a node only by a valid,
unexpired record. The control plane stores with each node the `token_id` of the
join token that admitted it.

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

`alloy init` takes `--advertise <addr>[,<addr>...]`, `--name`, `--mesh-port`,
`--api-port`, and `--data-dir` (default `/var/lib/alloyd`, section 3.12), none
of them required. It does this:

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
3. It generates the cluster key, a random 128-bit `cluster_id`, the API CA key,
   and the node's keys, then creates:

   | Object | Value |
   |---|---|
   | node | Named by the rule of section 3.3, zone `default`, roles `anchor`, `control`, `compute`, `storage`, without `compute` when the virtualization check of section 3.8 is `warn` |
   | control plane | One voter (ARFC 0006 section 3.2) |
   | organization | `default` |
   | project | `default`, in `default` |
   | user | `admin`, in `default`, with cluster role `cloud-admin` and role `owner` of `default` (ARFC 0007) |
   | pool | `default`, `replica(1)`, on one `file` disk at `<data-dir>/disks/0` (ARFC 0012 sections 3.5 and 3.6) |
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
| secret | 16 | Random. The control plane stores only `BLAKE3(secret)` |
| preset roles | 1 | Bit 0 `anchor`, bit 1 `control`, bit 2 `compute`, bit 3 `storage`, bits 4 to 7 zero. `0` for no preset roles |
| anchor count | 1 | 1 to 4 |
| per anchor | 33 + addresses | Anchor mesh public key (32), address count (1, value 1 to 4), then each address |
| checksum | 4 | First 4 bytes of `BLAKE3` over every preceding byte |

An address is a family byte (`4` or `6`), the address (4 or 16 bytes), the
anchor's UDP mesh port (2), and the TCP port of its `stream` underlay (2, `0`
for none, ARFC 0004 section 3.6). A token lists anchors in ascending order of
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
clause 3.10.1). Revocation takes effect at the next join attempt. The client
that creates a token generates its secret and sends the API only
`BLAKE3(secret)`, so no API response carries a token secret (I-22). A token that
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
| `--mesh-port <port>` | The default mesh port of ARFC 0004 |
| `--data-dir <path>` | `/var/lib/alloyd`. On Windows, `Alloy` in the folder Windows reports as `%ProgramData%`. Either way it follows section 3.12 |
| `--no-wait` | Off. When on, the command returns with exit 0 once `alloyd` holds the request |

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
2. It enables and starts `alloyd`, which holds the request from here on and
   carries out steps 3 to 7. The local join state is now `joining`. On Windows,
   enabling sets the start type of the service `alloyd` (ARFC 0017 clause 3.15)
   to automatic.
3. `alloyd` generates node keys, or reuses the keys and handshake counter kept
   from an earlier failed attempt (section 6), and opens an AL session to an
   anchor from the token as an unregistered initiator. It tries every address of
   every anchor in token order, UDP before `stream`, for at most 10 seconds in
   total.
4. An anchor must accept sessions from unregistered initiators only to its join
   service, rate limited to 10 handshakes per second per source address and 100
   per second in total.
5. The joiner sends its release version. Without looking at any token, the
   anchor replies with either the refusal `version`, carrying the accepted minor
   releases and the highest release that any node runs, or the current cluster
   public key and the anchor's own node record. During the 7-day overlap of a
   cluster key rotation (section 3.6) the reply also carries the previous
   cluster public key and its signature over the current one. A joiner is
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
6. The joiner sends `{token_id, secret, identity_pub, mesh_pub, name,
   name_is_default, roles, zone, advertise, facts}`, with `roles` and `zone`
   empty when not given, and `facts` being operating system, architecture,
   cores, memory, virtualization support, and disks. It waits at most 10 seconds for the
   answer. The control plane checks, in this order:
   1. The secret, as below. An unknown `token_id` or a wrong secret is refused
      with `token_refused`.
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
   a refusal. With a record its local join state becomes `joined`. With roles
   including `storage`, the node's one `file` disk at `<data-dir>/disks/0` joins
   pool `default` when that pool exists, and the pool's redundancy is unchanged.
8. Time from running the command to an `active` node must be under 10 seconds on
   a LAN, excluding approval.

The token check compares `BLAKE3(secret)` with the stored hash in constant time.
An unknown `token_id` and a wrong secret get the same refusal, `token_refused`,
byte for byte. Only after the secret matches does the control plane give any
other refusal. Every refusal is audit-logged with the source address.

| Refusal | When | Counts a use | `alloy join` exit |
|---|---|---|---|
| `version` | The joiner's release is outside the accepted window. Sent before any token is checked | no | 1 |
| `token_refused` | Unknown `token_id`, or wrong secret | yes, when the `token_id` is known | 4 |
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
keeps or reopens its session to an anchor and asks for the request's state,
known by the mesh key that session authenticates, never sending the secret
again. The CLI may stop waiting without cancelling the request. After approval
the node must become `active` within 10 seconds while an anchor is reachable.

### 3.4 Addresses

Each cluster has an infrastructure prefix `fd00::/8` plus 40 bits from
`cluster_id`, giving a `/48`. A node's infrastructure address is that prefix, a
zero subnet, and 64 bits from `BLAKE3(mesh_pub)`. Control plane, storage, and
migration traffic use these addresses over the mesh interface `al0`. Every node,
a client node included, holds its infrastructure address on `al0` with prefix
length 64, so the cluster's zero subnet is reached through `al0`, and holds no
other address there. On Windows `al0` is a Wintun adapter (ADR 0062). The
address and its route follow ARFC 0017 clause 3.17.

### 3.5 Validity, renewal, revocation

Node records are valid for 24 hours. A node renews at half life by signing a
request with its identity key, and, when it could not then, as a computer that
was asleep could not, as soon as it can while its record is valid. Removing a
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
checks of section 3.3 steps 3 to 5. It then sends `{node_id, serial,
signature}`, where `serial` is that of its expired record and `signature` is
its identity key's signature over the bytes `alloy-renew-v1`, `cluster_id`,
`node_id`, and `serial` (8 bytes, big endian). The control plane answers within
10 seconds. It issues a new record when the node exists, the signature verifies
against the node's `identity_pub`, and the mesh key the AL session
authenticated is the node's `mesh_pub`. Otherwise it refuses with
`node_unknown`, byte for byte the same for a deleted node, an unknown
`node_id`, a bad signature, and another mesh key. A renewal needs no token and
counts no use.

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
the new one, both are trusted for 7 days, then the old one is dropped. A join
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
| ports | The mesh UDP port is free, and every TCP port the node's roles listen on (ARFC 0004 section 3.6, ARFC 0008 section 3.8) | | A port is in use, 5. The message names the process and pid holding it when they can be found |
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
- `joining`: it withdraws the request through an anchor, trying for at most 10
  seconds, then removes the local state.
- `joined`: it asks the control plane, signed with the node identity key, to
  delete its node. The control plane refuses with `Conflict`, and the machine is
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
after a `leave` succeeds as a first join does. When the control plane does not
answer within 10 seconds, `leave` fails with `Unavailable` and changes nothing,
unless `--force` is given: then it removes the local state anyway, and the node
record lasts until it expires (section 3.5) or a cloud admin deletes it.

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
state, and its host file (ARFC 0017 section 4.2). An **Alloy data directory**
meets all of these:

- On Linux, it is not a symbolic link, is owned by user id 0, and has mode
  `0700`. Every directory above it is owned by user id 0 and is not writable by
  its group or by others. Its host file is owned by user id 0 and has mode
  `0600`.
- On Windows, it is not a reparse point, its owner is `SYSTEM` or the
  `Administrators` group, and its access control list is protected from
  inheritance and grants full control to `SYSTEM` and `Administrators` and no
  access to anyone else. Its host file has the same owner and inherits that
  list.
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

On Windows, after an unprivileged local user creates the directory `Alloy` in
`%ProgramData%`, `alloy join` must exit 5 naming that directory and its owner,
and the directory must stay empty. Without that directory, `join` creates it,
and `icacls` on it lists only `SYSTEM` and `Administrators`, each with full
control and neither inherited.

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

A token whose secret does not match must count as a use and must be logged in
the audit log with the source address. A refusal for any other reason counts no
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

## 8. Security

A join token is a bearer secret until it expires or is revoked. It grants the
ability to add a node, which is cluster-level trust. A login code is a bearer
secret for its user until it is spent, voided, or expires. Both secrets are
generated where the string is assembled, are stored by the control plane only as
`BLAKE3(secret)`, and never appear in an API response. The access key secret
that an exchange creates is returned by that exchange alone, as ARFC 0007 clause
3.9.4 says. A joiner sends its secret only after it has authenticated the
anchor's mesh key and checked the cluster key against the token, and a CLI sends
a login code only after the API certificate verified against `api_ca_fp`.
Refusal reasons are given only after the secret matches, so a guessed
`token_id` or `code_id` learns nothing. A retry is recognized only by a mesh key
that the AL session authenticated together with the matching secret, so it
hands a record to no one but the holder of the admitted node's keys, and so
does the renewal of an expired record, which also needs the node to exist. The
node's secrets sit in a data directory that only `SYSTEM` and `Administrators`,
or user id 0, can open (section 3.12), and that `init`, `join`, and `alloyd`
refuse to use otherwise. A client
node is bounded as ARFC 0020 clause 3.3 bounds a compromised compute node that
serves no guest, volume, or VPC: holding no role, it can read only traffic
addressed to itself and can write only its own status.
Unauthenticated exchange calls are bounded per source and in total before they
reach the store or the audit log. Upholds I-09, I-14, I-15, I-21, I-22.

## 9. Open questions

- Threshold custody of the cluster key across control nodes. Research task to be
  cut in M2.
- Hardware-bound node keys (TPM). Research task to be cut in M10.

# ADR 0040: Init and join need no flags, and a one-use login code carries the first credential

- Status: accepted
- Subject: operations
- Specs: arfc-0002, arfc-0019

## Context

PB-0001 asks that `alloy init` and `alloy join` work with no flags, that the first admin credential reach a laptop with no trust-on-first-use prompt, and that common mistakes fail early and plainly. ARFC 0002 required `--advertise`, left the admin credential undefined, counted every mistyped token as a use, and said nothing on joiner defaults, version skew, approval, retries, an already joined machine, or leaving. I-22 forbids secrets in API responses. ADR 0041 defines user access keys and the local admin.

## Decision

- `--advertise` defaults to the source address of the default IPv4 route, else IPv6, and `init` says whether it is private. Every node has advertise addresses, carried in its record and used while it holds `anchor` or `control`. `init` checks its API at a local advertise address, or at loopback when none is local.
- `init` creates org, project, user `admin`, pool `default` on one `file` disk, one join token, and one login code (ARFC 0002 section 3.1).
- The first credential is a login code: `alyc_`, base32 of cluster id, API CA fingerprint, code id, 16-byte secret, up to 4 API endpoints, and a 4-byte BLAKE3 checksum. One use, 24 hours, stored as `BLAKE3(secret)`, exchanged once for a user access key as ARFC 0007 section 3.9 says.
- Join tokens gain the same checksum and a byte of preset roles. A string that fails the checksum is rejected locally and counts no use. The preset roles let `join` check its ports before contacting anyone.
- A joiner defaults to `compute,storage` (no `compute` without `/dev/kvm`) and a name from its hostname. It sends its release version before any secret and is refused outside the ARFC 0017 window. It sends the token secret only after checking the cluster key against the token, which accepts the previous cluster key during a rotation's 7-day overlap.
- A join retry from the same keys with the same token gets the existing record or `pending`, with no new record and no new use. A `joining` node that never reports is deleted when its record expires. A failed attempt keeps the keys and the handshake counter.
- Refusal reasons are given only after the secret matches. Unknown ids and wrong secrets get one identical refusal.
- Approval is a cluster setting, off by default. `alloyd` holds a `pending` request until `ApproveNode`, `RejectNode`, or token expiry.
- `init` or `join` on a machine already in a cluster fails with `Conflict`. `alloy node leave` returns it to `unjoined`.
- `alloyd` raises a standing warning when an advertise address of an anchor or control node leaves its interfaces, and never changes the address itself.
- Clients generate join token and login code secrets, so no API response carries one. The access key secret is returned once, by the exchange that creates it (ARFC 0007 clause 3.9.4).
- `ExchangeLoginCode` is limited per source address, an IPv6 source counted by its /64, and in total across the cluster, before any lookup or audit write.
- With no context, root on a control node issues login codes, creates join tokens, and updates its own node through the local admin socket (ARFC 0019 clause 3.1.14).
- CLI warnings take the error form (`Warning:`, then `Try:`) on standard error.

## Consequences

- Install to a running cluster is two commands with no flags. Login needs no prompt.
- A mistyped token or code costs nothing. A guessed id learns nothing. A lost reply costs a rerun, not a second node.
- The token format changes at version `1`. Nothing was released, so no reader exists.
- A node that joined without `anchor` can later become an anchor or a control node with no new address.
- Tokens made before a cluster key rotation work for 7 days and are then revoked.
- A one-node cluster cannot be removed with `alloy node leave`, which refuses on the last control node.

## Alternatives rejected

- Printing a raw admin password: needs a certificate prompt on first login and lives past its first use.
- Server-generated join token and login code secrets in API responses: breaks I-22.
- Telling a joiner why a token failed before the secret matches: lets anyone probe token ids.
- Making a joiner an anchor by default: exposes it to unregistered sessions without the user choosing it.
- Rejecting a join retry as a name conflict: a lost reply would leave a node that never completes (I-14).
- Checking every role's ports on every join: a compute node would fail on a busy TCP 443 it never uses.
- Capping token lifetime at the 7-day rotation overlap: limits automation more than revoking at the end of the overlap.

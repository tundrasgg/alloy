# ARFC 0007: Tenancy and IAM

- Status: accepted
- Depends on: 0006
- Crates: alloy-iam
- Decisions: 0017, 0018, 0050

## 1. Purpose

Alloy is multi-tenant throughout. This ARFC defines organizations and projects,
who can act, how they prove it, what they may do, and how much they may use.

## 2. Model

### 2.1 Scopes

`cluster > organization > project > resource`. An organization (org) is the
isolation boundary. A project groups resources and carries quotas. Names are
unique within their parent.

### 2.2 Principals

| Principal | Lives in | Authenticates with |
|---|---|---|
| user | org | Password and second factor, or an external identity provider, or an access key issued for a login code (3.9) |
| service account | project | Access keys |
| instance role | project | Credentials from the metadata service (ARFC 0011) |
| node | cluster | Node identity key |
| cloud admin | cluster | A user with a cluster role |
| local admin | control node | Being user id 0 on that control node, through its local admin socket (3.10) |

A user is enabled or disabled. Every authentication as a disabled user must fail,
with any credential.

### 2.3 Credentials

| Credential | Form | Lifetime |
|---|---|---|
| session token | Ed25519-signed token, format below | 12 hours, refreshable for 30 days |
| access key | `AKAL` + 16 characters, with a 40-character secret | Until revoked |
| temporary key | Access key, secret, and session token | 15 minutes to 12 hours |

Access keys sign requests with AWS Signature Version 4, on every API surface. A
token is `base64url(payload) "." base64url(signature)` where the payload is a
Protocol Buffers message `{principal, org, issued, expires, key_id, scope,
derives_from, via_temporary_key}`. A temporary key carries such a token.

An access key belongs to exactly one principal, a service account or a user, and
grants that principal's policies, no more and no less. It has a `name`, unique
among the unrevoked keys of its principal, and an `issued_by` field naming what
created it. An access key that belongs to a user is a **user access key**. A user
access key authenticates without a password and without a second factor.

A session token or a temporary key **derives from** an access key when the
request that obtained it was authenticated by that access key, or by a session
token or temporary key that derives from that access key. A refreshed session
token derives from whatever the token it replaces derives from. A credential
therefore derives from at most one access key. The control plane writes the id of
that access key into `derives_from` of the credential's token, and leaves it empty
when the credential derives from no access key. It sets `via_temporary_key` when
the request that obtained the credential was authenticated by a temporary key, or
by a session token or temporary key whose `via_temporary_key` is set. A refresh
copies both fields. Both are covered by the signature, so a caller cannot set or
change them. Derivation is carried only in the token: the control plane keeps no
derivation record per session token or temporary key, so derivation adds no
control plane state that grows with the number of credentials issued (I-21).

### 2.4 Policies

A policy is a list of statements: `{effect: allow | deny, actions, resources,
conditions}`.

- Actions are `service:Verb`, for example `compute:CreateInstance`. `*` is allowed
  as a suffix.
- Resources are names of the form
  `arn:alloy:<service>:<region>:<org>/<project>:<kind>/<name-or-uid>`, with `*`
  allowed per segment.
- Policies attach to principals, groups, projects, and orgs. Some resources
  (buckets, repositories) carry a resource policy.

Built-in roles per org and per project: `owner`, `admin`, `member`, `viewer`.
Cluster roles: `cloud-admin`, `cloud-viewer`.

## 3. Behavior

3.1 Evaluation: gather every policy that applies. Any matching deny denies.
Otherwise any matching allow allows. Otherwise deny. A local admin is the one
exception: its fixed policy is its whole authorization, and no deny binds it
(3.10.1).

3.2 A request from a principal in one org for a resource in another must be denied
unless a resource policy on the target, or a policy in the target's org, allows
that principal. A denied cross-org request must be indistinguishable from a
request for a resource that does not exist (I-02).

3.3 Cloud admins manage nodes, pools, address pools, and orgs. A cloud admin role
does not grant access to the contents of an org's resources. Access to tenant data
by a cloud admin requires an explicit, audited grant from that org or a break
glass action that the org's audit log records.

3.4 Authorization must run on every API call, before any state is read on the
caller's behalf, and its decision must take under 1 ms at the 99th percentile with
warm caches.

3.5 Quotas exist per org and per project for: vCPUs, memory, instances,
containers, volumes, volume bytes, snapshots, object bytes, buckets, VPCs, public
addresses, repositories. A request that would exceed a quota fails with
`QuotaExceeded` naming the quota. Quota checks and the resource creation commit
atomically.

3.6 Metering: the control plane emits one usage record per resource per 5 minutes
`{org, project, resource, kind, measure, quantity, interval}`. Records are
append-only, kept 13 months, and exportable through the API.

3.7 Deleting an org deletes everything in it, after a 7-day grace period in which
it is disabled and restorable.

3.8 Credentials at rest: passwords under Argon2id, access key secrets encrypted
under a key from the `key-provider` seam, since SigV4 needs the secret to verify.

### 3.9 User access keys

3.9.1 A user access key must be created only by exchanging a login code (ARFC
0002) that was issued for that user. Every other request to create an access key
for a user must fail with `PermissionDenied`, whatever the caller's policies.

3.9.2 A login code for a user is issued only by: `alloy init`, for the user it
creates (ARFC 0002 section 3.1); the user itself, in a request authenticated by
one of its access keys or by a session token; or a local admin (3.10). A request
authenticated by a temporary key, or by a session token whose `via_temporary_key`
is set (2.3), must not issue a login code, so no credential obtained through a
temporary key, directly or through any chain of credentials, can issue one. A
user must not be able to issue a login code for any other principal. Issuing is
authorized as action `iam:IssueLoginCode` with the user the code is for as the
resource. Every
built-in org role and the cluster role `cloud-admin` allow it on the holder's own
user. A user with no role cannot issue one. A matching deny in a policy attached
to that user, to a group it belongs to, or to its org refuses the call whoever
the caller is, except a local admin, which no deny binds (3.10.1). Issuing must
also be refused if, when the call commits, the user is disabled or its org is
disabled (3.7), or the request was authenticated by an access key, or by a
session token that derives from an access key (2.3), and that access key is not
unrevoked. These checks read the state committed in the transaction that creates
the code, not the view that authenticates requests, which may lag a revocation
by up to 5 seconds (3.9.12). Every refusal in this clause fails with
`PermissionDenied` and creates no login code. A revocation and an issuing through
the revoked key are ordered by the control plane (I-11): if the revocation
commits first, the issuing is refused here, and otherwise the revocation voids
the code (3.9.7). So no unspent login code is ever issued through a revoked key,
and no key created after a revocation has the revoked key as `issued_by`.

3.9.3 The exchange request carries the login code, a `name` for the key, and
optionally `replaces` (3.9.10). The CLI sends as `name` the computer's hostname,
lower case, cut at the first dot, with characters outside `[a-z0-9-]` replaced by
`-`, prefixed `key-` if it does not start with a letter, and then cut to 63
characters. An exchange whose `name` does not meet section 7 must fail with
`InvalidArgument`, must create no key, and must not spend the login code. If the
user already has an unrevoked key with that name, the control plane must not
revoke or change the existing key. It must name the new key `<name>-<n>`, with
the lowest `n` from 2 upward that is free, and return the name it used.

3.9.4 One exchange creates exactly one access key and spends the login code in
the same control plane transaction. Of any number of concurrent exchanges of one
login code, at most one must succeed. The response carries the access key id, the
name, and the secret. The secret must not be returned by any later call (I-22).
The exchange is authenticated by the login code and by nothing else. It is
authorized, when it commits, as the user the code was issued for: it must fail
with `PermissionDenied` and create no key if that user is disabled or deleted, or
its org is disabled (3.7). No policy is evaluated at the exchange, so no action
and no resource are checked: a valid, unspent login code and the checks in this
clause are its whole authorization (3.4). Policies were evaluated when the code
was issued (3.9.2), and a deny attached after that does not refuse the exchange
(section 8).

3.9.5 `issued_by` of a user access key is one of: the id of the access key that
authenticated the request that issued the login code; the id of the access key
that the session token which authenticated that request derives from (2.3);
`session` when that session token derives from no access key; `root@<node name>`
when a local admin issued the code; or `init`. It is set by the control plane,
never by the caller, and is returned whenever the key is listed or read.

3.9.6 A user access key is listed and revoked through the same methods as a
service account's access key, and revocation must take effect within 5 seconds
across the cluster (ARFC 0020 section 3.4). Every built-in org role and the
cluster role `cloud-admin` allow a user to list and revoke its own keys. A key may
revoke itself.

3.9.7 Revoking an access key must, in the same transaction, void every unspent
login code that was issued by that key or by a session token that derives from
it. Within 5 seconds of the revocation, across the cluster, every session token
and temporary key that derives from the revoked key must be refused, and such a
session token must not be refreshable. A session token or temporary key whose
`derives_from` names an access key must be accepted, and refreshed, only while
that key is kept (3.9.11), is unrevoked, and was created before the credential
was issued. A user access key **descends** from another access key when its
`issued_by` is that key, or a key that descends from that key. Revoking a key in
the middle of such a chain does not cut the chain: the keys that descended
through it still descend from the keys above it (3.9.11). The revocation response
must state how many unrevoked user access keys descend from the revoked key. A
revocation with `descendants: true` must revoke all of them in the same
transaction, each with the effects of this clause. A revocation request that
names a key that is already revoked and still kept (3.9.11) must succeed, however
the key was revoked, through `replaces` (3.9.10) included. It revokes and voids
nothing again for that key, its response states how many unrevoked user access
keys descend from it, and with `descendants: true` it revokes all of them in one
transaction, each with the effects of this clause.

3.9.8 Disabling or deleting a user must revoke every access key of that user and
void every unspent login code issued for it, in the same transaction.

3.9.9 Audit (ARFC 0018 section 3.3). A record is written in the user's org chain
for: each login code issued for the user, with the issuer as principal; each
exchange attempt, successful or not, as an authentication attempt with the source
address and the login code id; each user access key created, naming the key id,
the key name, and `issued_by`; and each revocation, naming every key it revoked,
a key revoked through `replaces` included. No record holds a login code secret or
an access key secret. A revocation by a local admin (3.10.1) is recorded there
too, with `root@<node name>` as principal.

3.9.10 One key per computer. `replaces` in an exchange request is an access key
id. If it names an unrevoked access key of the user the login code was issued
for, the control plane must revoke that key in the transaction of the exchange,
with the effects of 3.9.7 and without `descendants`, before it counts the user's
keys (section 7) and before it chooses the name (3.9.3). The login code being
exchanged is spent by the exchange and is not voided by that revocation. A new
key that asks for the name of the key it replaces therefore gets that name with
no number. A `replaces` that names anything else must be ignored: the exchange
proceeds as if it were absent, and its response and its error, if any, are those
of the same exchange without `replaces`. Anything else is a revoked key, a key
that is not kept (3.9.11), a key of another principal, and an id that no key has.
So a computer whose key was revoked and deleted elsewhere logs in again with the
stored id. If the exchange fails, the named key must not be revoked. The CLI
must send `replaces`, with the id of the access key stored in the context it is
logging in to, whenever that context holds one. A computer
therefore holds at most one unrevoked user access key per context for as long as
it keeps its key store. A computer that lost its key store cannot name its old
key: that key stays until it is revoked (3.9.6).

3.9.11 Lineage state. An access key is **kept** while it is unrevoked, and after
its revocation for exactly as long as at least one unrevoked user access key
descends from it (3.9.7). In the transaction after which no unrevoked key descends
from a revoked key, the control plane must delete that key: a key revoked with no
unrevoked descendant is deleted by its own revocation, and a revocation with
`descendants: true` deletes, among others, the key it names and every key it
revokes. A kept revoked key is listed and read like an unrevoked one, marked
revoked, with its id, name, `issued_by`, and creation time. A call whose target
is one access key, given by its id, which is reading that key or revoking it
with or without `descendants`, must fail with `NotFound` and change nothing when
the id names no kept key. The rule covers that target and nothing else. An
access key id anywhere else follows its own clause: the access key that signs a
request authenticates it (3.9.12), `replaces` in an exchange is ignored (3.9.10),
and `derives_from` in a token refuses the token (3.9.7, 3.9.12).
Every key that an unrevoked key descends from is therefore kept, so deleting a key
never changes which kept keys descend from which, and the guarantees of 3.9.7 and
section 8 hold for every kept key. Kept keys are bounded per user (section 7),
and derivation adds no state (2.3). Apart from the issuer recorded with each
unspent login code (ARFC 0002), nothing else is kept for lineage.

3.9.12 Refused access keys. A request signed with an access key id must fail
authentication when the id names no unrevoked key, whether the key is revoked
and kept, deleted (3.9.11), or was never issued, and when the signature does not
verify. A revocation, and any deletion it causes, binds this clause within 5
seconds of its commit (3.9.6). An id that was never issued fails at once. A
request authenticated by a session token or temporary key that 3.9.7 refuses
fails authentication too. On the native API such a request must fail with
`Unauthenticated`, must change nothing, must not be authorized or served any
further, and must be audited as an authentication attempt (ARFC 0018 section
3.3.1). Every case of this clause must return the same error code and the same
message, so a caller cannot learn from the response whether an access key id was
ever issued, was revoked, or was deleted. Another API surface returns the error
it maps `Unauthenticated` to (ARFC 0016 section 6), the same one in every case.

3.9.13 Revoking all keys of a user. A request names one user and revokes, in one
transaction, every unrevoked access key of that user, each with the effects of
3.9.7. It is authorized as action `iam:RevokeUserAccessKeys` with that user as
the resource. Every built-in org role and the cluster role `cloud-admin` allow it
on the holder's own user, and a local admin may call it for any user (3.10.1).
Every key a user's key descends from, and every key that descends from it, is a
key of the same user (3.9.2, 3.9.5), so the call is a revocation with
`descendants: true` of all of them. When it commits, the user has no unrevoked
key and therefore no kept key (3.9.11), both of its counts in section 7 are 0,
and no login code issued through any of its access keys is unspent or can be
issued any more (3.9.2, 3.9.7). The response states how many keys it revoked, 0
included. It does not void a login code issued by a local admin, by `alloy init`,
or through a session token that derives from no access key, and it ends no
session token or temporary key that derives from no access key.

### 3.10 Local admin

3.10.1 Each node that holds the `control` role has one built-in principal, its
local admin, named `root@<node name>`. It is not a user, belongs to no org, holds
no credential, and cannot be given policies, roles, or group membership. Its
policy is fixed and allows exactly these actions:

| Action | Limited to |
|---|---|
| `iam:IssueLoginCode` | A named user that holds the cluster role `cloud-admin` when the call commits. Default: user `admin` in org `default` |
| `iam:RevokeUserAccessKeys` | A named user, any user (3.9.13). Default: user `admin` in org `default` |
| `cluster:CreateJoinToken` | No further limit (ARFC 0002 section 3.2) |
| `cluster:UpdateNode` | The roles and advertise addresses in the node record of the node it runs on |

Every other call by a local admin must fail with `PermissionDenied` and must be
audited as an authorization denial. The fixed policy is the local admin's whole
authorization: no deny refuses a local admin call, wherever the deny is attached,
which is the one exception to 3.1. A login code issued by a local admin must still
be refused when, as the call commits, the user does not hold `cloud-admin`, or
the user or its org is disabled (3.9.2). A revocation by a local admin under
3.9.13 is refused by no policy and no user or org state: it must succeed for a
user that is disabled, holds no role, or is under a deny on
`iam:RevokeUserAccessKeys`.

The two `iam` actions are the recovery path of a user that holds no credential.
The local admin revokes all of that user's access keys (3.9.13), which ends a
stolen key's lineage and brings both of the user's counts in section 7 to 0, and,
for a user that holds `cloud-admin`, issues a login code that the user exchanges
(3.10.7). Either call may come first: a login code issued by a local admin is not
voided by the revocation, and an exchange refused at a limit of section 7 does
not spend it. Nothing a credential can change before the recovery stops it: no
deny binds the local admin, the revocation is limited to no user, and the
cluster always holds a user that a local admin can issue a login code for
(3.10.8).

3.10.2 A local admin authenticates only through the local admin socket: a Unix
domain stream socket at the path `/run/alloy/admin.sock`, owned by user id 0, with
mode `0600`, in a directory owned by user id 0 with mode `0700`. `alloyd` must
serve a connection on it only if the kernel reports the peer's user id as 0, and
must otherwise close the connection without reading a request and write an audit
record of a failed authentication. The socket serves the native API methods
behind the four actions above and nothing else. No TCP, UDP, mesh, or abstract
socket listener may accept a local admin.

3.10.3 `alloyd` must create the local admin socket only while its node's current
node record holds the `control` role, and must remove it within 5 seconds of
learning that the role or the record is gone.

3.10.4 The control plane must not rely on 3.10.3. It must take the node in
`root@<node name>` from where the call entered the control plane: the control
node whose own socket accepted it, or, for a write forwarded to the leader (ARFC
0006 section 3.4), the node record bound to the AL session that carried it. It
must never take it from the content of a request. A local admin call that arrives
over a session whose node record lacks the `control` role, or over any API
endpoint other than the local admin socket, must fail with `PermissionDenied`,
must change nothing, and must be audited as an authorization denial.

3.10.5 A local admin call passes authorization (3.4) and is committed through the
control plane like any other write. Without quorum it fails with `Unavailable`.
There is no path around consensus.

3.10.6 Audit. Every local admin call, allowed or denied, writes a record in the
cluster chain with principal `root@<node name>`, no org, and the node's
infrastructure address (ARFC 0002 section 3.4) as source address, in the same
transaction as the change (I-15). A login code issued by a local admin is also
recorded in the org chain of the user it was issued for (3.9.9).

3.10.7 A login code issued by a local admin is exchanged like any other (3.9.3
to 3.9.5). Everything done with the resulting access key is done, and audited, as
that user.

3.10.8 Last `cloud-admin`. The cluster must always hold at least one user that
holds the cluster role `cloud-admin`, is enabled, and belongs to an enabled org.
`alloy init` creates the first (ARFC 0002 section 3.1). A call that would leave
no such user, whether it removes that role from a user, disables or deletes a
user, or disables or deletes an org (3.7), must fail with `Conflict`, must change
nothing, and its message must say that it would remove the last enabled
`cloud-admin` user. So a stolen credential cannot leave the cluster with no user
that a local admin can issue a login code for (3.10.1): the cluster always keeps
a `cloud-admin` user that root on a control node can log in as.

## 5. Seams

| Kind | Trait | Implementations | Suite |
|---|---|---|---|
| `authn` | `Authenticator` | `local`, `oidc`, `mem` | `tests/conformance/authn` |
| `key-provider` | `KeyProvider` | `cluster`, `mem` | `tests/conformance/key-provider` |

## 6. Failure behavior

If policies cannot be loaded, the answer is deny. If the identity provider is
unreachable, existing sessions continue until they expire and new logins through
that provider fail.

An exchange of a login code that fails at any step must create no access key and
must revoke no access key. Whether a failed exchange spends the login code is
defined with the login code, in ARFC 0002, except where this ARFC says it must
not (3.9.3 and section 7). If the audit record of a local admin call or of an
exchange cannot be committed, the call must fail and change nothing. If the
control plane cannot read the state of the access key that a session token or
temporary key derives from (2.3), it must refuse that credential.

## 7. Limits

64 KiB per policy. 20 policies per principal. 1,000 projects per org. 100
unrevoked access keys per principal, counting user access keys. 1,000 kept access
keys per user, unrevoked and revoked together (3.9.11). An exchange for a user
that already has 100 unrevoked keys or 1,000 kept keys, counted after any
revocation through `replaces` (3.9.10) and the deletions it causes, must fail,
must not spend the login code, and its error must name the limit. No limit
refuses, delays, or narrows a revocation.
A re-login through `replaces` consumes one kept key when its login code was
issued by the key it replaces, or by a key that descends from that key, directly
or through a session token: the replaced key is revoked but stays kept, because
the new key descends from it (3.9.5, 3.9.10, 3.9.11). A computer that logs in
again this way n times holds n kept revoked keys, and they count until the
transaction after which no unrevoked key descends from them, which deletes them.
A user frees that room by ending every unrevoked key that descends from the
chain: by revoking it, by revoking all its keys (3.9.13), or, for the newest key
of the chain, by logging its computer in again with `replaces` naming it and a
login code from outside the chain. Such a code is issued by a local admin, by a
key that neither is in the chain nor descends from a key in it, or by a session
token that derives from no such key.
Lineage state is bounded by these limits alone: derivation keeps no control
plane state per session token or temporary key (2.3), and revoked keys are kept
only within the 1,000 (I-21).
Names, including access key names: 1 to 63 characters, `[a-z0-9-]`, starting
with a letter. A key name is cut to 59 characters before a number is appended
(3.9.3).
One local admin per control node. At least one enabled `cloud-admin` user in an
enabled org per cluster (3.10.8).

## 8. Security

User access keys. A user access key is a long-lived credential that needs no
second factor. Stolen, it grants exactly its user's policies until revoked (ARFC
0020 sections 2.1 and 3.4). Those policies include issuing login codes for the
same user, so a thief can create further keys, with the stolen key or with a
session token obtained with it. Every such key descends from the stolen key
(2.3, 3.9.5, 3.9.7), no credential obtained through a temporary key can issue a
login code (3.9.2), and revoking the stolen key ends the session tokens and
temporary keys that derive from it (3.9.7). So one revocation with `descendants:
true` removes every credential of that user that exists because of the stolen
key. The stolen key may already be revoked, through `replaces` on a new login or
otherwise: it stays kept while any unrevoked key descends from it, and the
revocation may name it (3.9.7, 3.9.11). Lineage state is bounded (section 7,
I-21) without dropping any key that an unrevoked key descends from. The price is
that a thief can fill the user's 100 unrevoked keys by issuing keys, or its 1,000
kept keys by issuing and replacing keys, which refuses every new login of that
user, one with a login code from a local admin included, as revoking the user's
keys could. The same revocation deletes every key the thief made and frees the
room. A revocation ends the lineage for good, even while a node still accepts
the revoked key for up to 5 seconds: no login code can be issued through a
revoked key in that window (3.9.2). A user whose only credential was stolen has
nothing to send the revocation with. A local admin revokes all of its keys
(3.9.13, 3.10.1), for any user and whatever the thief changed with the stolen
credential first: no deny binds the local admin, and no role, user state, or
limit narrows that call. If the user holds `cloud-admin`, the local admin then
issues it a login code, which works at either limit and when the user is the
cluster's only `cloud-admin`. If the thief removed that role, disabled the user,
or disabled its org, the local admin issues the code for another `cloud-admin`
user instead, one of which always exists (3.10.8), so the cluster never loses
its last administrator to a stolen credential. A user without `cloud-admin` can
also send the revocation with a session token from its password or identity
provider (2.2), or a principal whose policies allow `iam:RevokeUserAccessKeys` on
that user sends it for it (3.9.13). Only the user itself can then issue it a
login code (3.9.2). A user's own re-logins also fill the kept keys when each
login code comes from the key it replaces, and section 7 says how that room is
freed.

The remedy has two limits. It does not undo anything else done with the user's
policies, such as a service account's access key or an attached policy: the
audit log lists those.
And a user access key whose `issued_by` is `session` descends from no key: if a
session token that derives from no access key is stolen, the keys made with it
are found through `issued_by` and the audit log and are revoked one by one, or
with all of the user's keys at once (3.9.13).

A deny on `iam:IssueLoginCode` attached to a user, a group, or an org stops login
codes being issued for the users it covers, by every caller except a local admin
(3.9.2, 3.10.1). It does not void a login code issued before it was attached:
that code can be exchanged until it expires (ARFC 0002). A deny cannot bind the
local admin because the credential the recovery must remove can attach one: a
thief holding a `cloud-admin` user's only key, which in PB-0001 is also `owner`
of org `default`, would otherwise deny both `iam` actions on that user and its
org and keep the key for good. A disabled user gains no key by any path (3.9.2,
3.9.4, 3.9.8).

Local admin. User id 0 on a running control node is not a new trusted party. ARFC
0020 section 2.2 trusts the hardware and kernel of each node, section 2.1 names a
compromised compute node as an adversary and no compromised control node, and
section 2.3 has no boundary between user id 0 and `alloyd` on one node. Accepted
ARFCs already give a single control node the power of every principal that holds
an access key: every control node serves the API (ARFC 0006 section 3.4) and
authenticates each call it serves (ARFC 0008 section 3.7), verifying SigV4 needs
the access key secret (3.8), and at rest those secrets sit under a node key file
of mode `0600` (ARFC 0020 section 3.7), the same gate as the socket. The control
plane promises to survive the loss of a minority of control nodes (I-11). No ARFC
promises anything when a control node is hostile. The ruling does not rest on
where the cluster key is kept, which no ARFC states. The path gives that party
no power it lacks and makes its use evident in the audit log as
`root@<node name>`. It gives nothing to any adversary in ARFC 0020 section 2.1:
it has no network listener, it does not exist on a machine that is not running
as a control node, a guest reaches it only by breaking the guest to host
boundary, and a compromised compute node is refused by the control plane under
3.10.4, not by its own `alloyd`, so ARFC 0020 section 3.3 holds. Revoking all
keys of a user (3.9.13) adds no access either: it reads no data and grants
nothing, it only removes credentials, the user's org chain records it (3.9.9),
and it is what lets a stolen key be revoked, as ARFC 0020 section 3.4 requires,
when its user has no credential left or the thief has attached a deny. Exempting
the local admin from denies lets it issue a login code for a `cloud-admin` user
against that user's org's deny, recorded in the cluster chain and the user's org
chain (3.9.9, 3.10.6). It reaches only `cloud-admin` users, whose power that
party already holds wherever one of them holds an access key, and the deny still
binds every other principal. The local admin cannot be revoked short of
removing the node's `control` role or its record.

Upholds I-02, I-05, I-15, I-21, I-22.

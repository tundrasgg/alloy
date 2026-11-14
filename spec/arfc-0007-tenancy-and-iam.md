# ARFC 0007: Tenancy and IAM

- Status: accepted
- Depends on: 0006
- Crates: alloy-iam
- Decisions: 0017, 0018, 0055, 0068, 0070

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
| session token | Ed25519-signed token, format below | 12 hours, refreshable until 30 days after its session began (3.12.2) |
| access key | `AKAL` + 16 characters, with a 40-character secret | Until revoked |
| temporary key | Access key, secret, and session token | 15 minutes to 12 hours |

An access key id is `AKAL` followed by 16 characters of the RFC 4648 section 6
base32 alphabet in upper case (`A` to `Z`, `2` to `7`): the unpadded encoding of
10 bytes from the `alloy-rt` randomness trait. The control plane draws again
when the id is already in use. Its secret is 40 characters of the RFC 4648
section 5 base64url alphabet: the unpadded encoding of 30 bytes from the same
trait. The control plane generates both when it creates the key (3.9.4).

Access keys sign requests with AWS Signature Version 4, on every API surface. A
token is `base64url(payload) "." base64url(signature)` where the payload is a
Protocol Buffers message `{principal, org, issued, expires, key_id, scope,
derives_from, via_temporary_key, session, session_issued}`. A temporary key
carries such a token.

A session token belongs to one **session**: the tokens that one
`CreateSessionToken` call (3.12.1) and the refreshes that descend from it
produce. `session` is 16 bytes from the `alloy-rt` randomness trait, drawn when
the session is created, and `session_issued` is the time it was created. Every
token of a session carries both unchanged. This session is a family of session
tokens, not the security association between two nodes that the glossary and
ARFC 0003 call a session. Where both could be meant, this one is a **login
session**.

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

### 2.5 Resources

Identity state is resources of the global group (ARFC 0006 sections 2.1 and
2.2) in service `iam`. Besides the fields of ARFC 0006 section 2.1, each kind
below has the fields listed. Kinds for projects, groups, service accounts,
policies, and role bindings other than cluster roles are added by the tasks
that build them. Until a method serves a kind, its Protocol Buffers field
numbers are the developer's choice (ARFC 0008 clause 3.1).

| Kind | Scope | `name` | Fields |
|---|---|---|---|
| `Org` | cluster | The org's name | `enabled` (spec) |
| `User` | its org | The user's name | `enabled` (spec); `cluster_roles` (spec), each `cloud-admin` or `cloud-viewer` |
| `AccessKey` | the org of its user | The key id in lower case | `principal` (spec), the `uid` of its user; `key_name` (spec), the name of 3.9.3; `secret_sealed` (spec), the secret as 3.8 keeps it; `issued_by` (spec), as 3.9.5 says; `revoked` (status) |
| `LoginCode` | the org of its user | `code-` followed by the 16 lower case hexadecimal digits of its `code_id` | `code_id`, `user` (the `uid` of its user), `secret_hash` (`BLAKE3` of the secret, 32 bytes), `issued_by`, `expire_time` (spec); `state` (status), one of `unused`, `spent`, `voided` |
| `LocalAdminUse` | cluster | `local-admin-use` | The one record of 3.10.8: `principal`, `last_write_time`, `writes` (status) |

No field of any kind holds a secret in clear.

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
through the API by a cloud admin requires an explicit, audited grant from that org
or a break glass action that the org's audit log records. The same holds for a
local admin (3.10.1, 3.10.6).

The rule above binds calls through the API and nothing else. User id 0 on a
control node can read all tenant data, of every org. That includes the control
plane state on the node's disks, which holds every resource, of every org, in
each consensus group the node belongs to (ARFC 0006 sections 2.1 and 2.2), the
user data and SSH keys of instances among them (ARFC 0011 section 2), and the
access key secrets that let that party act as any principal that holds an access
key (section 8). It includes all data that any node the party controls stores,
serves, or carries: guests, volumes, objects, registry content, and VPC traffic
(ARFC 0020 clause 3.3, ARFC 0012 section 1). That holds whether the node already
holds the data, as the first node that `alloy init` creates holds every org's
guests and volumes (ARFC 0002 section 3.1), or the party makes a node hold it
through calls that manage nodes, pools, and placement (ARFC 0011 clause 3.4.1,
ARFC 0012 clause 3.1.1 and section 6). This ARFC does not bound that reach and
does not list its means. Every local admin is called by that party, user id 0 on
its own control node (3.10.2). A cloud admin who controls any node, through its
hardware or as user id 0 on it, can take the same reach, since `cloud-admin`
allows giving that node the `control` role (3.10.1). ARFC 0020 section 2.2
already trusts that party: it trusts the control node quorum and the hardware
and kernel of each node, and puts their compromise outside the model, and its
section 8 does not defend against a malicious cloud admin with access to a
node's hardware. So that reach lies outside the boundary that this clause and
3.2 enforce (section 8).

What the audit log records of that party is exactly what passes through the API.
ARFC 0018 clause 3.3.1 requires a record for every mutating API call, every
authentication attempt, every authorization denial, and every cloud admin
action, including break glass access, and its clause 3.3.3 commits each record
in the control plane transaction of the change it describes. An action that does
not reach the control plane through the API has no transaction for a record to
commit in, so this ARFC rules that a cloud admin action in ARFC 0018 clause 3.3.1
is one that reaches the control plane through the API. Recorded: every call
through the local admin socket, and every refused connection to it (3.10.2,
3.10.6), and every call signed with another principal's credential, such as an
access key whose secret was read from the node, which is recorded as a call of
that principal (ARFC 0018 clause 3.3.1). Not recorded: anything done outside the
API, such as reading a node's disks, memory, or key files, or the data a node
stores, serves, or carries. No record shows that such a read happened, and no
standing warning (3.10.8) is raised by it.

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
fails authentication too, and so does one authenticated by a session token
that 3.12.3 or 3.12.4 refuses. On the native API such a request must fail with
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
issued any more (3.9.2, 3.9.7). In the same transaction it voids every unspent
login code for that user whose `issued_by` is `session`, and ends every session
of that user as 3.12.6 does, so no session token of the user survives it,
whatever the token derives from. The response states how many keys it revoked, 0
included. It does not void a login code issued by a local admin or by `alloy
init`. It ends no temporary key that derives from no access key: such a key
lives at most 12 hours and can issue no login code (3.9.2).

### 3.10 Local admin

3.10.1 Each node that holds the `control` role has one built-in principal, its
local admin, named `root@<node name>`. It is the cluster's break-glass
administrator: the way to administer the cluster when no user credential works
or none can be trusted. It is not a user, belongs to no org, holds no
credential, and cannot be given policies, roles, or group membership. Its policy
is fixed and allows exactly these actions:

| Part | Actions | On |
|---|---|---|
| cluster administration | Every action that the cluster role `cloud-admin` allows: managing nodes, pools, address pools, and orgs (3.3), with `cluster:CreateJoinToken` (ARFC 0002 section 3.2) and `cluster:UpdateNode` on any node among them | Every resource on which that role allows the action |
| identity | Every action of the `iam` service, `iam:*` | Every resource of that service, in every org and at cluster scope: users, groups, service accounts, roles and who holds them, cluster roles included, policies and what they are attached to, access keys, and login codes |

A local admin may therefore do everything a cloud admin may. The first row is
the set of actions that the specification gives the built-in cluster role
`cloud-admin`, not the state of any role record. No API method may change which
actions a built-in role (2.4) allows, for any caller, so no call changes the
fixed policy. Every other call by
a local admin must fail with `PermissionDenied` and must be audited as an
authorization denial. The fixed policy is the local admin's whole authorization,
which is the one exception to 3.1: no deny refuses a local admin call, wherever
the deny is attached, and no role, group membership, or enabled state of any
principal or org is consulted to authorize it.

What is not policy binds a local admin call as it binds every caller:

- The fixed policy grants no access to the contents of an org's resources (3.3):
  like the cluster role `cloud-admin`, it allows no action that reads or changes
  them. Its `iam` part can still lead to such access through the API, for
  example by a change of who has access or by a new credential of a principal
  that already has it. Each such call is a break-glass action in the sense of
  3.3, and 3.10.6 records it in that org's chain (section 8). The fixed policy
  bounds what the API serves to a local admin. It does not bound what user id 0
  on that control node can read, which is all tenant data (3.3).
- A user access key is created only by an exchange (3.9.1). A local admin issues
  a login code instead.
- A login code is refused when, as the call commits, the user or its org is
  disabled (3.9.2). A local admin enables them first (3.10.7).
- Quotas (3.5), the limits of section 7, argument checks, a stale `generation`
  (ARFC 0008 clause 3.5), and every refusal that an ARFC states for all callers
  whatever their policies, such as deleting the last control node (ARFC 0002
  section 3.9).

A local admin call of `iam:IssueLoginCode`, `iam:RevokeUserAccessKeys`, or
`iam:RevokeUserSessions` that names no user acts on user `admin` in org
`default`. A revocation by a local admin under 3.9.13 or 3.12.6 is refused by no
policy and no user or org state: it must succeed for a user that is disabled,
holds no role, or is under a deny on its action.

3.10.2 A local admin authenticates only through the local admin socket: a Unix
domain stream socket at the path `/run/alloy/admin.sock`, owned by user id 0, with
mode `0600`, in a directory owned by user id 0 with mode `0700`. `alloyd` must
serve a connection on it only if the kernel reports the peer's user id as 0, and
must otherwise close the connection without reading a request and write an audit
record of a failed authentication. The socket serves the methods of the native
API (ARFC 0008), each as a local admin call whatever credential the request
carries, and nothing else. It does not serve `ExchangeLoginCode` (ARFC 0002
section 3.7), which fails on it with `PermissionDenied`. No TCP, UDP, mesh, or
abstract socket listener may accept a local admin.

`alloyd` learns the peer's user id from `peer_uid` of the `net` seam (ARFC 0005
clause 3.1.9.2), once per connection, before it reads a byte. When `/run/alloy`
is missing, it creates it `Private` (ARFC 0005 clause 3.1.15). When that path
is anything other than a directory owned by user id 0 with mode `0700`, it
creates no socket and logs why every 60 seconds. Before it listens, it removes
an entry left at the socket path by an earlier start.

3.10.3 `alloyd` must create the local admin socket only while its node's current
node record holds the `control` role, and must remove it within 5 seconds of
learning that the role or the record is gone.

3.10.4 The control plane must not rely on 3.10.3. It must take the node in
`root@<node name>` from where the call entered the control plane: the control
node whose own socket accepted it, or, for a call forwarded to the leader (ARFC
0006 section 3.4), the node record bound to the AL session that carried it. It
must never take it from the content of a request. A local admin call that arrives
over a session whose node record lacks the `control` role, or over any API
endpoint other than the local admin socket, must fail with `PermissionDenied`,
must change nothing, and must be audited as an authorization denial.

3.10.5 A local admin call passes authorization (3.4) and is committed through the
control plane like any other write. That holds for a read too, because every
local admin call commits an audit record (3.10.6). Without quorum every local
admin call fails with `Unavailable`, and none is served as a stale read (ARFC
0008 section 6). There is no path around consensus.

3.10.6 Audit. Every local admin call, read or write, allowed or denied, writes a
record in the cluster chain (ARFC 0018 section 3.3) with principal
`root@<node name>`, no org, and the node's infrastructure address (ARFC 0002
section 3.4) as source address. A call that the fixed policy allows and that
reads or changes an org, or a resource in an org, also writes a record with that
principal in the chain of each such org, where the org's admins read it (ARFC
0018 clause 3.3.5). That covers the records of 3.9.9. Every record commits in
the transaction of the call, together with the change when the call makes one
(I-15). A call whose records cannot be committed must fail, must change nothing,
and must return no data (section 6).

3.10.7 Restoring an administrator. Through calls that 3.10.1 allows, a local
admin must be able to do each of the following, for any org and any principal,
whatever policies are attached anywhere and whatever roles, group memberships,
and enabled states principals and orgs have:

| Step | What the local admin can do |
|---|---|
| see | List every org with its enabled state. List every user of an org with its enabled state, its groups, and the roles it holds, cluster roles included. List and read the policies attached to a principal, a group, a project, or an org. List a principal's access keys as 3.9.6 and 3.9.11 say, never a secret (I-22) |
| cut off | Revoke one access key (3.9.7) or all keys of a user (3.9.13). Disable a user. Remove a role from a principal, cluster roles included. Delete a service account. Disable an org (3.7) |
| unbind | Detach a policy from a principal, a group, a project, or an org. Delete a policy. Remove a user from a group |
| restore | Create an org, or restore a disabled one within its 7 days (3.7). Create a user in an org, or enable a disabled one. Grant a user a role, cluster roles included |
| log in | Issue a login code for any enabled user of an enabled org (3.9.2) |

A login code issued by a local admin is exchanged like any other (3.9.3 to
3.9.5), and everything done with the resulting access key is done, and audited,
as that user.

Take any user `U` in any org `O`, whether they exist yet or not, and any state
that API calls can have produced. A local admin creates or restores `O`, creates
or enables `U`, grants `U` the cluster role `cloud-admin`, detaches every policy
that holds a deny statement from `U`, from the groups `U` belongs to, and from
`O`, revokes all keys of `U` (3.9.13), and issues `U` a login code. When no
other principal changes that state in between:

- none of those local admin calls may fail with `PermissionDenied`;
- the exchange of that login code must not be refused by a limit of section 7,
  since both key counts of `U` are 0, nor by the state of `U` or `O` (3.9.4);
  and
- a call signed with the resulting access key must be allowed whenever the
  cluster role `cloud-admin` allows it, no deny statement matches it in a
  policy that still applies to it (3.1), and 3.2 does not refuse it.

After those detachments no policy attached to `U`, to a group of `U`, or to `O`
holds a deny. Two things can still refuse such a call, and only these.

First, the default deny of 3.2, when the target lies in an org other than `O`:
a project of that org, or a resource or principal in it. A cluster role is not
a policy in any org, so `cloud-admin` never lifts that deny, and the call is
allowed only when a resource policy on the target, or a policy in the target's
org, allows `U`, as for any principal of `O`. A target at cluster scope (2.1),
such as a node, a pool, an address pool, or an org itself, which are what 3.3
gives cloud admins to manage, lies in no org, and 3.2 does not refuse a call on
it.

Second, a deny in a policy attached on the side of the target, and only in
these: the resource policy of the target resource (2.4); a policy attached to
the target's project; a policy attached to the target's org, when that org is
not `O`; and, when the target is a principal, a policy attached to that
principal or to a group it belongs to (3.9.2). When the target is an org, that
org lies in no org (2.1), so the target has no org in this sense, and a policy
attached to that org is not on the side of the target. A deny in any other
place, such a policy included, must not refuse the call. The local admin can
list each of those policies and detach it (rows "see" and "unbind"), except a resource policy, which is changed through
the resource's own service and so lies outside the fixed policy (3.10.1).

Once the local admin has detached the ones that match, the call must be allowed
unless 3.2 refuses it or the target's own resource policy denies it.

A user created in an org that the local admin created in the same recovery has
no group and no attached policy, so nothing needs detaching for it. Cutting off
comes first: a principal that would change that state in between is stopped by
the calls of the second row, and nothing that principal can do refuses or
narrows a local admin call.

The cluster therefore need not hold any user with the cluster role
`cloud-admin`. A call that removes that role from the last user that holds it,
or that disables or deletes that user or its org, must not be refused for
leaving the cluster without one.

3.10.8 Standing warning. The control plane keeps one record of local admin use
per cluster. It holds the principal and the commit time of the latest local
admin write, and the number of local admin writes since the record was created.
A local admin write is a local admin call to a mutating method (ARFC 0008 clause
3.2) that succeeds, whether or not it changed anything. The transaction of each
local admin write must create the record, or update it. A local admin read, and
a local admin call that is denied or fails, must not create or change it.

While the record exists, starting with the commit that created it, the API must
report one standing warning (ARFC 0019 clause 3.2.5) of kind `local-admin-used`.
Its affected resource is the cluster. Its message names the principal
`root@<node name>` of the latest local admin write, the time of that write, and
the number of writes, and says that the audit log of the cluster lists each one.
Its command is empty: no command removes this warning. The control plane must
delete the record 7 days after the commit of the latest local admin write, and
the warning must no longer be reported 60 seconds after that. No call by any
principal, a local admin included, deletes the record, changes it in any other
way, or shortens the 7 days.

### 3.11 First deployment

M11 rung 2 (ADR 0068) builds of this ARFC what `alloy login <login code>` and
`alloy node list` need on each machine, and the local admin path that issues
the login codes:

- Sections 2.1 and 2.2 for the org `default`, its user `admin`, and the local
  admin. Section 2.3 for user access keys signed with SigV4 (ARFC 0008 clause
  3.7.1). Section 2.4 for the cluster role `cloud-admin` alone. Section 2.5 for
  the kinds it lists.
- Clauses 3.1 and 3.4. With no policy and no other role in rung 2, a call is
  allowed exactly when a local admin's fixed policy (3.10.1), or the cluster
  role `cloud-admin` of the calling user, allows it. `ListNodes` is action
  `cluster:ListNodes` on each `Node`, which `cloud-admin` allows (3.3).
- Clause 3.8 for access key secrets, kept as below.
- Clauses 3.9.1 to 3.9.5, 3.9.9, 3.9.10, and 3.9.12, and the limits of section
  7 on access keys and names. Login codes are issued only by `alloy init` and
  by a local admin (ARFC 0002 section 3.15), so every user access key has
  `issued_by` `init` or `root@<node name>`, no key descends from another
  (3.9.7), and no login code is issued by a key. A key that `replaces` revokes
  therefore voids no login code and is deleted in the transaction that revokes
  it (3.9.11).
- Clauses 3.10.1 to 3.10.6 for the methods rung 2 serves, and 3.10.8 except the
  standing warning: the record is created, updated, and deleted as 3.10.8
  says, and the API reports no standing warning in rung 2 (ARFC 0002 section
  3.15). The audit log records every local admin call (3.10.6).

Everything else is built by later tasks: projects, groups, service accounts,
instance roles, org and project roles, policies, session tokens and temporary
keys, the seams of section 5, quotas, metering, deleting and disabling users
and orgs, listing and revoking keys (3.9.6, 3.9.13), and the restore of
3.10.7. The checks of the enabled state of a user or org still run, and in
rung 2 always find it enabled. A call that carries a session token fails with
`Unauthenticated`, as one whose token does not verify does (ARFC 0008 clause
3.7.1).

Access key secrets at rest. Rung 2 has one control node and no `key-provider`
seam, so that node keeps each secret under its at-rest key (ARFC 0002 section
3.14, ARFC 0020 clause 3.7). `secret_sealed` is a 24-byte nonce from the
`alloy-rt` randomness trait, then the XChaCha20-Poly1305 encryption of the 40
ASCII bytes of the secret under the at-rest key, with that nonce and, as
associated data, the 27 ASCII bytes `alloy/at-rest/access-key/v1` followed by
the 20 ASCII bytes of the key id. The secret is in no other field, log, or
state export (I-22). A cluster that a rung 2 release made never changes release
(ARFC 0017 clause 3.13), so the later task that builds `key-provider` replaces
this rule for new clusters only.

### 3.12 Session tokens

3.12.1 `CreateSessionToken` in the `iam` service creates a session and returns
its first token. It is authenticated in one of two ways, and a request that
carries both, or neither, fails with `Unauthenticated`.

- By a login code. The request carries the `code_id` and secret of a login
  code (ARFC 0002 section 3.7) and no other credential. It is rate limited,
  checked, spent, refused, and audited exactly as `ExchangeLoginCode` is by
  ARFC 0002 section 3.7 and clause 3.9.4, and shares its rate limits, except
  that it creates no access key, takes no `name` and no `replaces`, and the key
  limits of section 7 do not apply to it. The session's principal is the user
  the code was issued for. Its `derives_from` is the access key id that the
  code's `issued_by` (3.9.5) holds, and is empty when `issued_by` is `session`,
  `root@<node name>`, or `init`. So a session made from a code that an access
  key issued, directly or through a session token, ends when that key is
  revoked (3.9.7).
- By an access key. The request is signed with SigV4 by an unrevoked access
  key. The session's principal is the key's, and its `derives_from` is the
  key's id. It is authorized as `iam:CreateSessionToken` with that principal as
  the resource, which every built-in org role and the cluster role
  `cloud-admin` allow on the holder's own user. A request authenticated by a
  session token or a temporary key fails with `PermissionDenied` and creates
  nothing.

Either way, `via_temporary_key` is not set, `issued` and `session_issued` are
the commit time, and `expires` is 12 hours later. When the commit time is not
after the principal's `sessions_not_before` (3.12.4), which a commit in the same
instant allows, `issued` and `session_issued` are instead 1 microsecond after
`sessions_not_before`, so a session created after a revocation is never ended
by it. The call fails with
`PermissionDenied`, and creates nothing, when the principal or its org is
disabled (2.2, 3.7). The response carries `token`, empty when the request set
`cookie` (ARFC 0008 clause 3.14.6), `expire_time`, `session_expire_time`, which
is `session_issued` plus 30 days, `principal`, and `org`.

3.12.2 `RefreshSessionToken` is authenticated by a session token and returns a
new token of the same session: the same `principal`, `org`, `scope`,
`derives_from`, `via_temporary_key`, `session`, and `session_issued`, with
`issued` the commit time and `expires` the earlier of `issued` plus 12 hours
and `session_issued` plus 30 days. It fails with `Unauthenticated`, and issues
nothing, when the presented token is refused by any clause of this ARFC, and
when the session began 30 days or more before. It must also fail so when, as
its transaction commits, a revoked session record names its session, its
`session_issued` is at or before the principal's `sessions_not_before`
(3.12.4), its principal or that principal's org is disabled, or the access key
it derives from is not kept, is revoked, or was created after the presented
token was issued (3.9.7). These checks read the state committed in that
transaction, not the view that authenticates requests, which may lag a
revocation by up to 5 seconds, as in 3.9.2. A refresh and a revocation of its
session are ordered by the control plane (I-11): if the revocation commits
first, the refresh fails, even while nodes still accept the presented token.
The presented token stays valid until its own `expires`. A request
authenticated by anything but a session token fails with `PermissionDenied`. No policy is evaluated: the call returns a credential of
the caller's own session with nothing added, and the checks of this clause,
2.2, and 3.9.7 are its whole authorization.

3.12.3 `RevokeSessionToken` is authenticated by a session token and revokes
its session. Within 5 seconds of its commit, across the cluster, every token
of that session must be refused (3.9.12) and must not be refreshable, and every
Watch stream that one of them authenticated must end (ARFC 0008 clause
3.15.6). A request authenticated by anything but a session token fails with
`PermissionDenied`. No policy is evaluated, as in 3.12.2. A token of a session
that is already revoked fails with `Unauthenticated`, as every refused token
does.

3.12.4 Revoked sessions. The transaction of a revocation writes a **revoked
session record** holding the principal, the `session`, and a delete time,
which is the session's `session_issued` plus 30 days, and the control plane
deletes the record at that time. No token of the session outlives the record:
every token's `expires` is at most `session_issued` plus 30 days (3.12.2),
whichever control node's clock gave its `issued`, and no refresh that commits
after the revocation issues one (3.12.2). So from 5 seconds after the
revocation commits, every token of the session must be refused, and must not
be refreshable, until its own `expires`. A principal has at most 1,000 revoked
session records. A revocation that would write one more instead
sets the principal's `sessions_not_before` to its commit time and deletes every
revoked session record of that principal in the same transaction. From then,
within 5 seconds across the cluster, every session token of that principal
whose `session_issued` is at or before `sessions_not_before` must be refused,
as 3.12.3 says. `sessions_not_before` is one time per principal and never moves
back. So a revocation is never refused or delayed by the bound, and always
ends at least the session it names. The times that 3.9.7 and this section
compare, `issued`, `session_issued`, an access key's creation time, and
`sessions_not_before`, are commit times of the global group, and the control
plane must give each commit of that group a time no earlier than that of the
commit before it, whichever control node leads. So a session created by a
transaction ordered before a revocation of 3.12.4 or 3.12.6 has a
`session_issued` at or before that revocation's time.

3.12.5 Audit (ARFC 0018 section 3.3). A record is written in the principal's
org chain for each `CreateSessionToken` attempt, successful or not, as an
authentication attempt with the source address and the login code id or the
access key id, and for each refresh and each revocation. Each names the
session by `session` in lower case hexadecimal. No record, log line, error, or
response other than those of 3.12.1 and 3.12.2 holds a session token (I-22).
Each call of 3.12.6 is recorded too, naming the user, with its caller as
principal, `root@<node name>` for a local admin.

3.12.6 Ending every session of a user. `RevokeUserSessions` in the `iam`
service names one user. In one transaction it sets that user's
`sessions_not_before` to its commit time, unless it is already later, and
deletes every revoked session record of that user, which that time now covers.
From then, within 5 seconds across the cluster, every session token of that
user whose `session_issued` is at or before that time must be refused and must
not be refreshable, and every Watch stream that one of them authenticated must
end (ARFC 0008 clause 3.15.6). So it ends every session of the user, whatever
its tokens derive from, the caller's own session included, and revokes no
access key. It is authorized as `iam:RevokeUserSessions` with that user as the
resource, which every built-in org role and the cluster role `cloud-admin`
allow on the holder's own user, and a local admin may call it for any user
(3.10.1). The response is empty.

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
not (3.9.3 and section 7). If an audit record of a local admin call, read or
write, or of an exchange cannot be committed, the call must fail, change
nothing, and return no data (3.10.6). If a revoked session record or `sessions_not_before` cannot
be read, every session token of that principal must be refused, and every
refresh of one must fail (3.12.2, 3.12.4). If the
record of local admin use cannot be
written with a local admin write (3.10.8), that write must fail and change
nothing. If the
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
1,000 revoked session records per principal, each deleted 30 days after its
session began, and one `sessions_not_before` per principal (3.12.4). A
revocation at the bound is never refused: one that would write the 1,001st
record instead ends every session of that principal that began at or before
it. A session token is at most 1,024 characters, so
it fits a cookie.
Names, including access key names: 1 to 63 characters, `[a-z0-9-]`, starting
with a letter. A key name is cut to 59 characters before a number is appended
(3.9.3).
One local admin per control node. One record of local admin use per cluster
(3.10.8). No number of users with the cluster role `cloud-admin` is required
(3.10.7).

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
limit narrows that call. The local admin then issues the user a login code,
which works at either limit once the keys are revoked. Whatever else the thief
changed to stop that, a removed role, a disabled user or org, or an attached
deny, the local admin undoes first, or it sets up another user (3.10.7). It
needs no user to be left for that, so no credential, stolen or not, can leave
the cluster without a way to a working administrator. A user can also send the
revocation with a session token from its password or identity provider (2.2), or
a principal whose policies allow `iam:RevokeUserAccessKeys` on that user sends it
for it (3.9.13). A user's own re-logins also fill the kept keys when each login
code comes from the key it replaces, and section 7 says how that room is freed.

The remedy has two limits. A revocation does not undo anything else done with
the user's policies, such as a service account's access key or an attached
policy. That includes what a revoked key does in the up to 5 seconds in which a
node still accepts it (ARFC 0020 section 3.4): it can issue no login code
(3.9.2), but it can still make a credential of another principal, which
outlives it. The audit log lists all of this, and a local admin can undo every
such change to identity state (3.10.7).
And a user access key whose `issued_by` is `session` descends from no key: if a
session token that derives from no access key is stolen, the keys made with it
are found through `issued_by` and the audit log and are revoked one by one, or
with all of the user's keys at once (3.9.13).

Session tokens. A session made from an access key derives from it, and one made
from a login code derives from the access key that issued the code, directly or
through a session token, when one did (3.12.1). So revoking that key ends the
session, and a thief cannot escape the lineage above by trading a login code
for a session token. A session token lives at most 12 hours without a refresh, and no
session lasts past 30 days. Revoking a session takes effect within 5 seconds
(ARFC 0020 section 3.4) and for good: a thief who refreshes in a loop while the
user logs out gets no token from a refresh that commits after the revocation,
since a refresh reads committed state (3.12.2), and the record lasts as long as
any token of the session can (3.12.4). The revoked state is bounded per
principal, and the bound widens a revocation rather than refusing it (3.12.4).
A session made from a login code that a local admin, `alloy init`, or a
key-less session issued derives from no access key, so no key revocation ends
it. The user ends it, with every other session of that user, through
`RevokeUserSessions`, and a local admin does so for any user, without disabling
the user (3.12.6). Revoking all of a user's keys does the same and also voids
the login codes that a key-less session issued (3.9.13), so after a local
admin's revocation a thief keeps no credential of the user but a temporary key
that lives at most 12 hours and can issue no login code. In a browser the
token is held where page script cannot read it and another site cannot send it
(ARFC 0008 clause 3.14.6, ARFC 0019 clause 3.2.7).

A deny on `iam:IssueLoginCode` attached to a user, a group, or an org stops login
codes being issued for the users it covers, by every caller except a local admin
(3.9.2, 3.10.1). It does not void a login code issued before it was attached:
that code can be exchanged until it expires (ARFC 0002). A deny cannot bind the
local admin because the credential the recovery must remove can attach one: a
thief holding a `cloud-admin` user's only key, which in PB-0001 is also `owner`
of org `default`, would otherwise deny both `iam` actions on that user and its
org and keep the key for good. For the same reason a local admin can detach any
policy (3.10.7): a deny attached to an org binds that org's users after they log
in, and a deny that covers every caller can be removed only by a caller that no
deny binds. A disabled user gains no key by any path (3.9.2, 3.9.4, 3.9.8).

Local admin. The local admin is the cluster's break-glass administrator: it may
do everything a cloud admin may and change all identity state (3.10.1). User id
0 on a running control node is still not a new trusted party. ARFC 0020 section
2.2 trusts the hardware and kernel of each node, section 2.1 names a compromised
compute node as an adversary and no compromised control node, and section 2.3
has no boundary between user id 0 and `alloyd` on one node. Accepted ARFCs
already give a single control node the power of every principal that holds an
access key, `cloud-admin` users and org owners among them: every control node
serves the API (ARFC 0006 section 3.4) and authenticates each call it serves
(ARFC 0008 section 3.7), verifying SigV4 needs the access key secret (3.8), and
at rest those secrets sit under a node key file of mode `0600` (ARFC 0020 section
3.7), the same gate as the socket. Where a control node holds the cluster key,
which ARFC 0002 section 2 lists as held by the control plane, user id 0 on that
node holds it too. The ruling does not rest on that, because how the key is kept
across control nodes is open (ARFC 0002 section 9). The control plane promises
to survive the loss of a minority of control nodes (I-11). No ARFC promises
anything when a control node is hostile.

That party can already read all tenant data (3.3), and the socket adds nothing
to what it can read. What the socket adds is calls that no access key in the
cluster could make: changes to identity state that no holder of an access key
could change, for example in an org whose admins all log in with a password, and
cluster administration where no cloud admin holds an access key. ARFC 0020
section 2.2 trusts that party there as well. In exchange the
socket moves its power from key files to calls that pass authorization and
consensus (3.10.5) and are evident: every call is in the cluster chain as
`root@<node name>`, every call that touches an org is in that org's chain
(3.10.6), and every write raises a standing warning that lasts 7 days and that
the local admin cannot clear (3.10.8).

The path gives nothing to any adversary in ARFC 0020 section 2.1. It has no
network listener, it does not exist on a machine that is not running as a
control node, a guest reaches it only by breaking the guest to host boundary,
and a compromised compute node is refused by the control plane under 3.10.4, not
by its own `alloyd`, so ARFC 0020 section 3.3 holds. A stolen credential gains
nothing either: the local admin holds no credential, cannot be assumed,
delegated, or granted, and nothing a credential can set binds it, so a stolen
key grants its principal's policies only until it is revoked (ARFC 0020 section
3.4) and can no longer stop its own revocation. Giving a node the `control`
role makes a new local admin, and nothing else does. A cloud admin could always
do that, and it makes the node a voter of the control plane whether or not a local admin
exists. Removing the role or the node record ends that local admin (3.10.3,
3.10.4), and nothing else revokes one.

Tenant isolation. The local admin lives in no org, and its fixed policy reaches
the identity state of every org under no policy of that org. I-02 and 3.2 guard
the boundary between orgs (ARFC 0020 section 2.3): a principal of one org never
reaches another org's resources. The local admin stands on the cluster side of
that boundary, where 3.3 already puts cluster administrators. The party that
calls it, user id 0 on a control node, can read all tenant data of every org
(3.3), and nothing in this section narrows that. What follows says what the API
gives a local admin, what the audit log records of it, and why this is
consistent with I-02.

No principal of any org gains reach through the local admin, since it has no
credential and cannot be assumed, delegated, or granted. Its fixed policy allows
no action that reads or changes the contents of an org's resources (3.3,
3.10.1). Its `iam` part can still lead to tenant data through the API by getting
a credential of a principal that has access in that org. For example:

| Kind of call | Local admin write | What else that org's chain records |
|---|---|---|
| Change who has access | Grants a role, attaches or changes a policy, or changes group membership, for a principal whose credential it holds or then makes as in the next row | What the next row records |
| Make a credential of a principal that already has access, an org `owner` for example | For a user, issues a login code (3.10.7). For a service account, creates an access key, which 3.9.1 does not forbid. For either, any other `iam` action that sets how the principal authenticates, such as a password or an identity provider | The exchange of the login code, which the API serves and the socket does not (3.10.2), and the new key with `issued_by` `root@<node name>` (3.9.5, 3.9.9). A service account's new key names `root@<node name>` in `issued_by` (2.3) |

Each such call is a write to that org's identity state that needs no grant from
the org, so it is a break-glass action in the sense of 3.3. It is recorded in
that org's own chain with principal `root@<node name>`, in the transaction that
makes the change (3.10.6), which is what 3.3 requires of break-glass access, and
it raises the standing warning (3.10.8). A call then made with such a credential
is authorized by the policies that bind its principal and is audited as any call
of that principal is (ARFC 0018 clause 3.3.1). No response to a local admin
holds a secret other than that of a credential the call creates (3.9.4, I-22).

What the audit log records of a local admin is the same whatever the call leads
to. Every local admin call is in the cluster chain as `root@<node name>`, every
allowed local admin call that reads or changes an org, or anything in it, is in
that org's chain too, and every local admin write raises the standing warning
(3.10.6, 3.10.8). A local admin call that reads and changes no org, such as
giving a node a role, joining a machine, or changing pools and placement, is in
no org's chain, even when it makes a node hold that org's data. What user id 0
then reads on a node, or reads with no call at all, such as control plane
state, access key secrets, or data that a node already holds, is in no chain and
raises no warning (3.3). A call signed with an access key whose secret user id 0
read on the node is audited as a call of that key's principal, not as
`root@<node name>` (ARFC 0018 clause 3.3.1). The record does not stop user id
0, whom ARFC 0020 section 2.2 trusts.

This is consistent with I-02 and 3.2, which bind what principals reach through
calls and are enforced by authorization on every call (ARFC 0020 section 2.3).
The local admin's responses go only to user id 0 on a control node, so no org
learns anything about another through them, and a denied cross-org request stays
indistinguishable from a request for a resource that does not exist (3.2). What
user id 0 reads outside the API passes no authorization check on tenant data: it
reads through the hardware and kernel of a node, whose compromise ARFC 0020
section 2.2 puts outside the model, and ARFC 0020 section 8 does not defend
against a malicious cloud admin with access to a node's hardware in version 1.
The local admin adds no new party to that case: it is user id 0 on a control
node, which ARFC 0020 section 2.2 already trusts, and any cloud admin who
controls a node can take the same reach with its own credential (3.3).

Upholds I-02, I-05, I-15, I-21, I-22.

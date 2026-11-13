# ARFC 0007: Tenancy and IAM

- Status: accepted
- Depends on: 0006
- Crates: alloy-iam
- Decisions: 0017, 0018, 0055

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
through the API by a cloud admin requires an explicit, audited grant from that org
or a break glass action that the org's audit log records. The same holds for a
local admin (3.10.1, 3.10.6).

There is one way to tenant data outside the API, and the rule above does not
cover it. A node reads and alters the guests, volumes, and VPC traffic it serves
(ARFC 0020 clause 3.3), and managing nodes and pools decides which node serves
them. A cloud admin who also controls a node, through its hardware or as user id
0 on it, can make that node serve an org's guests or volumes, by joining it
(ARFC 0002 section 3.2), giving it roles, or steering placement (ARFC 0011
clause 3.4.1 and section 3.5), and then read them there. A local admin is always
such a party for its own node: it is user id 0 on that node and may give it the
`compute` and `storage` roles (3.10.1). Every call that sets this way up is in
the audit log, as every cloud admin action is (ARFC 0018 clause 3.3.1). A local
admin's call is in the cluster chain and, when it is a write, raises the
standing warning (3.10.6, 3.10.8). It is in an org's chain only when it reads or
changes that org or a resource in it (3.10.6), and creating a join token or
changing a node's roles does neither. No audit record covers what is then read
on the node, since ARFC 0018 clause 3.3.1 records API calls and authentication
attempts. ARFC 0020 trusts the hardware and kernel of each node (section 2.2)
and does not defend against a malicious cloud admin with access to a node's
hardware (section 8), so this way lies outside the boundary that this clause and
3.2 enforce (this ARFC, section 8).

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
  them. Its `iam` part can still lead to such access, by a change of who has
  access or by a new credential of a principal that already has it. Each such
  call is a break-glass action in the sense of 3.3, and 3.10.6 records it in
  that org's chain (section 8). Its cluster part can lead to such access only
  outside the API, through a node it controls, its own node always among them,
  as 3.3 states for every cloud admin who controls a node.
- A user access key is created only by an exchange (3.9.1). A local admin issues
  a login code instead.
- A login code is refused when, as the call commits, the user or its org is
  disabled (3.9.2). A local admin enables them first (3.10.7).
- Quotas (3.5), the limits of section 7, argument checks, a stale `generation`
  (ARFC 0008 clause 3.5), and every refusal that an ARFC states for all callers
  whatever their policies, such as deleting the last control node (ARFC 0002
  section 3.9).

A local admin call of `iam:IssueLoginCode` or `iam:RevokeUserAccessKeys` that
names no user acts on user `admin` in org `default`. A revocation by a local
admin under 3.9.13 is refused by no policy and no user or org state: it must
succeed for a user that is disabled, holds no role, or is under a deny on
`iam:RevokeUserAccessKeys`.

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
principal or to a group it belongs to (3.9.2). A deny in any other place must
not refuse the call. The local admin can list each of those policies and detach
it (rows "see" and "unbind"), except a resource policy, which is changed through
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
nothing, and return no data (3.10.6). If the record of local admin use cannot be
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

What the socket adds to that party's reach is what no access key in the cluster
grants: identity state that no holder of an access key could change, for example
in an org whose admins all log in with a password, and cluster administration
where no cloud admin holds an access key, the way through a node of 3.3
included. ARFC 0020 section 2.2 trusts that party there as well. In exchange the
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
3.4) and can no longer stop its own revocation. The one way to make a new local
admin is to give a node the `control` role. A cloud admin could always do that,
and it makes the node a voter of the control plane whether or not a local admin
exists. Removing the role or the node record ends that local admin (3.10.3,
3.10.4), and nothing else revokes one.

Tenant isolation. The local admin lives in no org, and its fixed policy reaches
the identity state of every org under no policy of that org. I-02 and 3.2 guard
the boundary between orgs (ARFC 0020 section 2.3): a principal of one org never
reaches another org's resources. The local admin stands on the cluster side of
that boundary, where 3.3 already puts cluster administrators, and it is
consistent with I-02 for four reasons, of which the second names every way to
tenant data through the API. The one way outside the API, through a node, is
stated after them. No principal of any org gains reach
through it, since it has no credential and cannot be assumed, delegated, or
granted. Its fixed policy allows no action that reads or changes the contents
of an org's resources (3.3, 3.10.1). Its `iam` part can still lead to tenant
data, and only by getting a credential of a principal that has access in that
org. There are two ways to one, and each is a local admin write to that org's
identity state:

| Way | Local admin write | What else that org's chain records |
|---|---|---|
| Change who has access | Grants a role, attaches or changes a policy, or changes group membership, for a principal whose credential it holds or then makes in the second way | What the second way records |
| Make a credential of a principal that already has access, an org `owner` for example | For a user, issues a login code (3.10.7). For a service account, creates an access key, which 3.9.1 does not forbid. For either, any other `iam` action that sets how the principal authenticates, such as a password or an identity provider | The exchange of the login code, which the API serves and the socket does not (3.10.2), and the new key with `issued_by` `root@<node name>` (3.9.5, 3.9.9). A service account's new key names `root@<node name>` in `issued_by` (2.3) |

Neither way needs a grant from the org, so each of these writes is a
break-glass action in the sense of 3.3. Each is recorded in that org's own
chain with principal `root@<node name>`, in the transaction that makes the
change (3.10.6), which is what 3.3 requires of break-glass access, and each
raises the standing warning (3.10.8). A call then made with such a credential
is authorized by the policies that bind its principal and is audited as any
call of that principal is (ARFC 0018 clause 3.3.1). No other local admin call
leads to tenant data through the API: every other action of the fixed policy is
one that the cluster role `cloud-admin` allows, which grants no action on
contents (3.3), and no response to a local admin holds a secret other than that
of a credential the call creates (3.9.4, I-22), which is the second way. So the
org's admins can see every step by which a local admin comes to hold a
credential for their data. The record does not stop user id 0, whom ARFC 0020
section 2.2 trusts. Every other local admin call that reads or changes anything
in an org is recorded in that org's chain too (3.10.6). And its
responses go only to user id 0 on a control node, so no org learns anything
about another, and a denied cross-org request stays indistinguishable from a
request for a resource that does not exist (3.2).

Through a node. Outside the API, the local admin, like any cloud admin who
controls a node, can reach tenant data as 3.3 states. It gives its own node the
`compute` or `storage` role (`cluster:UpdateNode`, 3.10.1), or, where it also
controls another machine, joins that machine (`cluster:CreateJoinToken`). It
then steers placement with the node and pool management that `cloud-admin`
allows (ARFC 0011 clause 3.4.1 and section 3.5), and reads on that node the
guests and volumes it then serves (ARFC 0020 clause 3.3). Each of those calls is
a local admin write: it is in the cluster chain as `root@<node name>` and raises
the standing warning (3.10.6, 3.10.8), so other cloud admins can see that the
way was prepared. A call among them that neither reads nor changes an org, such
as a change of a node's roles, is in no org's chain, and no record covers what
is read on the node. This is consistent with 3.3, which states this way, and
with I-02 and 3.2, which bind what principals reach through calls and are
enforced by authorization on every call (ARFC 0020 section 2.3). This way passes
no authorization check on tenant data: it reads through the kernel of a node,
which ARFC 0020 section 2.2 trusts, by a cloud admin with access to that node,
which ARFC 0020 section 8 does not defend against in version 1. The local admin
adds no new party to that case: it is user id 0 on a control node, which ARFC
0020 section 2.2 already trusts, and any cloud admin who is user id 0 on a node
can prepare the same way with its own credential.

Upholds I-02, I-05, I-15, I-21, I-22.

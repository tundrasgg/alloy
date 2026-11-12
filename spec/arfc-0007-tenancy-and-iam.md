# ARFC 0007: Tenancy and IAM

- Status: accepted
- Depends on: 0006
- Crates: alloy-iam
- Decisions: 0017, 0018

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
| user | org | Password and second factor, or an external identity provider |
| service account | project | Access keys |
| instance role | project | Credentials from the metadata service (ARFC 0011) |
| node | cluster | Node identity key |
| cloud admin | cluster | A user with a cluster role |

### 2.3 Credentials

| Credential | Form | Lifetime |
|---|---|---|
| session token | Ed25519-signed token, format below | 12 hours, refreshable for 30 days |
| access key | `AKAL` + 16 characters, with a 40-character secret | Until revoked |
| temporary key | Access key, secret, and session token | 15 minutes to 12 hours |

Access keys sign requests with AWS Signature Version 4, on every API surface. A
token is `base64url(payload) "." base64url(signature)` where the payload is a
Protocol Buffers message `{principal, org, issued, expires, key_id, scope}`.

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
Otherwise any matching allow allows. Otherwise deny.

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

## 5. Seams

| Kind | Trait | Implementations | Suite |
|---|---|---|---|
| `authn` | `Authenticator` | `local`, `oidc`, `mem` | `tests/conformance/authn` |
| `key-provider` | `KeyProvider` | `cluster`, `mem` | `tests/conformance/key-provider` |

## 6. Failure behavior

If policies cannot be loaded, the answer is deny. If the identity provider is
unreachable, existing sessions continue until they expire and new logins through
that provider fail.

## 7. Limits

64 KiB per policy. 20 policies per principal. 1,000 projects per org. 100 access
keys per principal. Names: 1 to 63 characters, `[a-z0-9-]`, starting with a
letter.

## 8. Security

Upholds I-02, I-05, I-15, I-22.

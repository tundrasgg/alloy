# ADR 0050: Users hold access keys from login codes, a local admin can issue and revoke them

- Status: superseded by 0055
- Subject: tenancy
- Specs: arfc-0007, arfc-0020

## Context

This ADR supersedes ADR 0041, which gave the local admin exactly three actions, all bound by denies. A thief holding a user's only access key can fill that user's 100 unrevoked or 1,000 kept keys (ARFC 0007 section 7), and with the same key attach a deny on the recovery actions, remove the user's `cloud-admin` role, or issue a login code in the seconds before its revocation reaches every node. Under ADR 0041 each of these leaves a stolen key of the cluster's only `cloud-admin` unrevocable or the cluster with no administrator.
The facts of ADR 0041 still hold. ARFC 0020 section 2.2 trusts the hardware and kernel of each node, and section 2.1 names a compromised compute node as an adversary but no compromised control node. Every control node serves the API (ARFC 0006 section 3.4) and authenticates each call it serves (ARFC 0008 section 3.7), which for SigV4 needs the access key secret (ARFC 0007 section 3.8), and those secrets sit at rest under a node key file of mode `0600` (ARFC 0020 section 3.7).

## Decision

- A user may hold access keys. A user access key is created only by exchanging a login code issued for that user, by that user, by `alloy init`, or by a local admin. It is named after the computer, records `issued_by`, and is listed and revoked like any access key, within 5 seconds. A login code is neither issued for nor exchanged into a disabled user or a user of a disabled org.
- One key per computer: the exchange takes `replaces`, the id of the key the computer already holds for that context, and revokes that key in the same transaction. The CLI always sends it when it holds one. Without `replaces` an exchange never touches an existing key, and a taken name gets `-2`, `-3`, and so on.
- A session token or temporary key obtained with an access key derives from that key. A login code issued through such a session token is recorded as issued by the key. A temporary key cannot issue a login code.
- Revoking a key voids the unspent login codes issued by it or through it, ends the session tokens and temporary keys that derive from it within 5 seconds, and reports how many keys descend from it. `descendants: true` revokes them too. Descendants are not revoked by default. New: issuing a login code is refused when, in the committed state, the key it is issued through is revoked, so the 5 second window issues no code. New: action `iam:RevokeUserAccessKeys` revokes every access key of one user, all of their lineage. Every built-in org role and the cluster role `cloud-admin` allow it on the holder's own user.
- Each control node has a built-in principal `root@<node name>`, the local admin, reached only through a Unix socket of mode `0600` owned by user id 0, and only by a peer with user id 0. Its fixed policy allows four actions: issue a login code for an enabled `cloud-admin` user of an enabled org, revoke every access key of any user (new), create a join token, update its own node's roles and advertise addresses. The control plane derives the node in `root@<node name>` from the socket or the AL session's node record, never from the request, and refuses a node without the `control` role. Calls go through authorization, consensus, and the audit log.
- A deny on `iam:IssueLoginCode` attached to a user, a group it belongs to, or its org refuses that action on that user from every caller except the local admin. New: no deny binds the local admin; its fixed policy is its whole authorization.
- New: a call that would leave the cluster with no enabled `cloud-admin` user in an enabled org fails with `Conflict`.
- Ruling against ARFC 0020: no change weakens it. A stolen user access key grants its user's policies until revoked (section 3.4), and now nothing the key can change stops the revocation. The local admin adds no trusted party (section 2.2) and no adversary in section 2.1 gains from it: by the facts in Context, user id 0 on one control node can already act as any principal that holds an access key. Revoking a user's keys reads nothing and grants nothing. The deny exemption reaches only login codes for `cloud-admin` users. The ruling does not rest on where the cluster key is kept, which no ARFC states. ARFC 0020 is unchanged.

## Consequences

- A user access key bypasses password and second factor. An org that does not want that attaches a deny on `iam:IssueLoginCode`. The deny does not void login codes issued before it and does not stop a local admin issuing one for a `cloud-admin` user, which both audit chains record.
- One revocation with `descendants: true` removes every key, login code, session token, and temporary key of that user that exists because of the stolen key, and frees the room. A local admin does the same for any user at any key count, under any deny. Neither undoes other uses of the user's policies. A key whose `issued_by` is `session` descends from no key and is found through the audit log.
- The cluster always has a `cloud-admin` user that a local admin can issue a login code for, whatever a stolen credential changed. A local admin can log out any user's access keys, which the user's org chain records. Administering a cluster and holding root on one of its control nodes are formally the same power, and the audit log shows each use as `root@<node name>`.
- A computer that lost its key store cannot send `replaces`. Its old key stays until someone revokes it, and its next login gets a numbered name.
- The local admin must be removed or made to need a quorum, by a new ADR, if any of these changes: a single control node can no longer authenticate API calls or recover access key secrets by itself (ARFC 0006 section 3.4, ARFC 0008 section 3.7, ARFC 0007 section 3.8, ARFC 0020 section 3.7); a hostile control node becomes an adversary in ARFC 0020 section 2.1; or ARFC 0020 gains a boundary between user id 0 and `alloyd` on a node. Threshold custody of the cluster key (ARFC 0002 section 9) does not by itself change the ruling.
- Where a hardware key is configured, secrets at rest are guarded more strongly than the socket. The socket exposes no secret and does not exist on a stopped node. A node without the `control` role has no local admin, so its advertise address is changed from a logged-in computer.

## Alternatives rejected

- No user access keys, session tokens only: a CLI would need the password every 30 days, and PB-0001 has no password login.
- Replace an existing key of the same name on exchange, or always revoke descendants: hostnames collide, and logging in again on one computer would log out every computer set up from it.
- A local admin with the full `cloud-admin` role, none with recovery by editing state offline, or one a setting disables: more unattributed power than recovery needs, the same trust with no audit record, and no stop to user id 0, who can read the node key file (ARFC 0020 section 3.7).
- Denies bind the local admin (ADR 0041): the stolen key can attach one on both `iam` actions and stay unrevocable, against ARFC 0020 section 3.4.
- Local admin revocation limited to `cloud-admin` users: the stolen key removes the role first. A fourth action on one named key: a user with no credential cannot list its keys to find it.
- Exempt a login code from a local admin from the key limits: the thief's keys stay live until the user acts again, and kept keys lose their bound (I-21).
- Refuse at commit every write through a revoked key: sound, but it reaches every service's write path, and only a login code makes a credential that outlives the key.

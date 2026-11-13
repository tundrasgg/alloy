# ADR 0041: Users hold access keys from login codes, root on a control node is a local admin

- Status: superseded by 0050
- Subject: tenancy
- Specs: arfc-0007, arfc-0020

## Context

PB-0001 logs a computer in by exchanging a login code for an access key, and recovers a lost login through root on a control node. ARFC 0007 gave access keys only to service accounts and knew no principal without a credential. ARFC 0020 section 2.2 trusts the hardware and kernel of each node, and its section 2.1 names a compromised compute node as an adversary but no compromised control node. Every control node serves the API (ARFC 0006 section 3.4) and authenticates each call it serves (ARFC 0008 section 3.7), which for SigV4 needs the access key secret (ARFC 0007 section 3.8). Those secrets sit at rest under a node key file of mode `0600` (ARFC 0020 section 3.7).

## Decision

- A user may hold access keys. A user access key is created only by exchanging a login code issued for that user, by that user, by `alloy init`, or by a local admin. It is named after the computer, records `issued_by`, and is listed and revoked like any access key, within 5 seconds.
- One key per computer: the exchange takes `replaces`, the id of the key the computer already holds for that context, and revokes that key in the same transaction. The CLI always sends it when it holds one. Without `replaces` an exchange never touches an existing key, and a taken name gets `-2`, `-3`, and so on.
- A session token or temporary key obtained with an access key derives from that key. A login code issued through such a session token is recorded as issued by the key. A temporary key cannot issue a login code.
- Revoking a key voids the unspent login codes issued by it or through it, ends the session tokens and temporary keys that derive from it within 5 seconds, and reports how many keys descend from it. `descendants: true` revokes them too. Descendants are not revoked by default.
- A login code is neither issued for nor exchanged into a disabled user or a user of a disabled org.
- Each control node has a built-in principal `root@<node name>`, the local admin, reached only through a Unix socket of mode `0600` owned by user id 0, and only by a peer with user id 0. Its fixed policy allows three actions: issue a login code for a `cloud-admin` user, create a join token, update its own node's roles and advertise addresses.
- A deny on `iam:IssueLoginCode` attached to a user, a group it belongs to, or its org refuses a login code for that user from every caller, the local admin included.
- The control plane derives the node in `root@<node name>` from the socket or the AL session's node record, never from the request, and refuses a node without the `control` role. Calls go through authorization, consensus, and the audit log.
- Ruling against ARFC 0020: neither change weakens it. A stolen user access key grants its user's policies until revoked (section 3.4). The local admin adds no trusted party (section 2.2) and no adversary in section 2.1 gains from it: by the facts in Context, user id 0 on one control node can already act as any principal that holds an access key. The ruling does not rest on where the cluster key is kept, which no ARFC states. ARFC 0020 is unchanged.

## Consequences

- A user access key bypasses password and second factor. An org that does not want that attaches a deny on `iam:IssueLoginCode`. The deny does not void login codes issued before it.
- That deny also binds the local admin. A cluster whose only `cloud-admin` users are under such a deny, and have lost their credentials, cannot be recovered through the local admin.
- A stolen key can create further keys for the same user. One revocation with `descendants: true` removes every key, login code, session token, and temporary key of that user that exists because of the stolen key. It does not undo other uses of the user's policies.
- A key whose `issued_by` is `session` descends from no key. After the theft of a session token that derives from no access key, the keys made with it are found through the audit log and revoked one by one.
- A computer that lost its key store cannot send `replaces`. Its old key stays until someone revokes it, and its next login gets a numbered name.
- Administering a cluster and holding root on one of its control nodes are formally the same power. The audit log shows each use as `root@<node name>`.
- The local admin must be removed or made to need a quorum, by a new ADR, if any of these changes: a single control node can no longer authenticate API calls or recover access key secrets by itself (ARFC 0006 section 3.4, ARFC 0008 section 3.7, ARFC 0007 section 3.8, ARFC 0020 section 3.7); a hostile control node becomes an adversary in ARFC 0020 section 2.1; or ARFC 0020 gains a boundary between user id 0 and `alloyd` on a node. Threshold custody of the cluster key (ARFC 0002 section 9) does not by itself change the ruling.
- Where a hardware key is configured, secrets at rest are guarded more strongly than the socket. The socket exposes no secret and does not exist on a stopped node.
- A node without the `control` role has no local admin, so its advertise address is changed from a logged-in computer.

## Alternatives rejected

- No user access keys, session tokens only: a CLI would need the password every 30 days, and PB-0001 has no password login.
- Replace an existing key of the same name on exchange: hostnames collide, and a second `ubuntu` would silently log out the first.
- Always revoke descendants: logging in again on one computer would log out every computer set up from it.
- Let the local admin's fixed policy override an org's deny: the deny would then not mean what ARFC 0007 section 3.1 says, and an org could not rely on it.
- Local admin with the full `cloud-admin` role: more unattributed power than recovery needs. Real work is done, and audited, as a named user.
- No local admin, recovery by editing state offline: same trust, no audit record, and far easier to get wrong.
- A setting that disables the local admin: it would not stop user id 0, who can read the node key file (ARFC 0020 section 3.7).

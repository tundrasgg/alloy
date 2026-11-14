# ADR 0065: Node requests are keyed by mesh key and act only on the requester's proof

- Status: accepted
- Subject: api
- Specs: arfc-0002, arfc-0006, arfc-0018

## Context

ADR 0042 had an anchor without `control` keep a control session with a control node and number the sessions it forwards for, so the once-only rule hung on sessions that rekeys and restarts replace. ARFC 0020 clause 3.3 bars a compromised node from writing control plane state beyond its own status or forging audit records, yet a forwarded request acted on the anchor's word for its mesh key: an anchor read the token secret in every `join_request` it carried and could rewrite the request, and a `withdraw` or replayed `renew` needed no signature. A heartbeat could end a `leave` or `renew` early, and audit records of node messages had no defined source address.

## Decision

This replaces the node channel bullet of ADR 0042. Every other bullet of ADR 0042 stands as decided there.

- Node messages keep the carriage of ADR 0042: one `NodeMessage` per UDP datagram, ports 7477, in IPv6 between infrastructure addresses on VNI 0 of an AL session, the source address bound to the mesh key the session authenticated (ARFC 0002 section 3.13).
- A request uses the next handshake counter value v and carries `request_id` v + 1. A control node keeps one entry per origin, made of mesh keys (the session's, and the one a `forwarded` names), never of a session. There is no control session and no session number.
- A node has one request outstanding at a time, and at most one of each kind outstanding or waiting; one that falls due while its kind is held is not added. Only its reply or its time limit ends a request; a waiting heartbeat goes first; a reply to any other request is dropped.
- A node with a valid record sends node messages only to a control node, on an AL session it holds with it. An anchor without `control` forwards only join service requests from unregistered initiators, keeping one control node and one time and nothing per request.
- No node message carries a token's secret. A token's secret seeds an Ed25519 token key, and the control plane stores only its public key. `join_request` acts only under two signatures over `cluster_id`, the requester's mesh key, and every byte of its body: the token key's and the node identity key's. ADR 0040's secret "matches" when the token key's signature verifies.
- `renew` issues a record only for the serial of the node's current record, under the node's identity signature, and every record a node gets has the serial before it plus 1, so a signed `renew` issues at most one record; `withdraw` deletes only under the identity signature over its mesh key.
- The audit source address of a node message is one the control plane saw: the underlay address at an anchor holding `control`, the anchor's infrastructure address for a `forwarded`, and the sender's infrastructure address otherwise.

## Consequences

- Rekeys, new sessions, restarts, and handovers keep request order, and anchors hold no state per request.
- `CreateJoinToken` sends `token_pub` instead of `BLAKE3(secret)`. `JoinRequest` becomes a signed `body`, and `Withdraw` gains a `signature`. A `renew` with an old serial is answered with the current record.
- An anchor can still replay a carried `join_request` unchanged while its token is usable, which remakes only what that joiner asked for, and with a token it holds by other means can occupy a joiner's mesh key (ARFC 0002 section 8).
- A `leave` or `renew` may wait up to 4 seconds for a heartbeat, and a `renew` can delay heartbeat acceptance to 38 seconds.

## Alternatives rejected

- A control session with session numbers: a rekey or restart renumbers sessions, so ids reset and a resend can be handled twice.
- Forward only `join_request`, and renew or withdraw only through anchors holding `control`: a node none of whose anchors holds `control` could neither.
- A MAC keyed with the stored `BLAKE3(secret)`: whoever reads the store could then make join requests, and BLAKE3 has no MAC row in the primitives table.
- Only the token proof, without the identity signature: an anchor holding the token could take a joiner's keys and make its join a retry of a forged record.
- A control plane challenge in every forwarded request: one more round trip each, where serials and single-use keys already make each signature act once.
- Let a heartbeat end other requests and keep one entry per message kind: more state per origin, where waiting suffices.

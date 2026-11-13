# ADR 0046: Probes are sealed under session keys with a replay window

- Status: accepted
- Subject: mesh
- Specs: arfc-0003, arfc-0004, arfc-0020

## Context

Probes were sealed under a static-static key with no replay check, so ARFC 0020
section 3.2 was not met. Any receiver state that bounds replays of such messages
is lost when it is dropped or the node crashes, and then old messages pass again.
Keeping it durably costs a synced write per new message stream and per-peer state
for every peer ever heard, against ADR 0031. Probing runs only between nodes that
already exchange traffic, through a relay if nothing else, so they hold a session.

## Decision

Probe messages travel inside a session. Each handshake derives, with the session
keys, one probe key per direction. A probe message carries the receiver's session
index and a per-session probe counter, and the receiver keeps a 2,048-counter
window per session for it, held and erased with the probe receive key. A node
that must probe a peer it holds no session with opens one first.

## Consequences

- No probe message is accepted twice: the window rejects a repeat while the key
  lives, and nothing opens once the key is discarded or lost in a crash.
- Replays draw no answer, verify nothing and add no answer challenge, so they
  cannot evict the 64 answer challenges per peer (ARFC 0004 section 7).
- Probes cost no DH and no synced write: one AEAD operation, and 336 bytes of
  state per session held. Probe messages no longer carry a node id in clear.
- A node with no session to the peer starts probing one handshake later.
- ADR 0039 and ARFC 0004 section 8 say probes have no replay check; that is now
  false. ADR 0039's decision stands.

## Alternatives rejected

- Persistent epoch per probe set: replays pass again after a record drop or crash.
- Durable per-peer replay state: a synced write per epoch, state for every peer.
- Static-static key with a stateless cookie round trip: a replay still draws a reply.
- Timestamps: no reliable clock (ADR 0012).
- Probe keys derived from the transport keys: one key would serve two purposes.

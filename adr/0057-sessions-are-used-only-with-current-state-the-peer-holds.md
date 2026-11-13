# ADR 0057: Sessions are used only with current state the peer holds

- Status: accepted
- Subject: mesh
- Specs: arfc-0003, arfc-0017, arfc-0020

## Context

A failed handover leaves the old process with counters, windows and a handshake
replay table older than its successor's (ARFC 0017 section 3.5), so it repeats
nonces (I-07) and accepts messages again (ARFC 0020 section 3.2). A rekey leaves
a responder unsure whether the initiator got the response, so probes it seals
under the new session may not open (ARFC 0003 section 3.6).

## Decision

- Handover: the successor records a session consumed before it first seals or
  accepts under it or sends its peer a handshake message, and records each
  initiation counter before it stores it. After a failed handover the old process
  discards consumed sessions, raises its replay table to the recorded counters,
  reserves a new handshake counter block, and re-handshakes within 1 second every
  peer whose newest session it discarded (ARFC 0003 section 3.9).
- Rekey: a node seals probe messages under the newest session it has seen the
  peer hold, and keeps an older session's probe receive state until 10 seconds
  after the peer uses a newer one, never past the 180 second key discard.

## Consequences

- No nonce repeats and no handshake or session message is accepted twice across
  any handover, failed or not.
- A failed handover costs one handshake per consumed session, and drops the peer's traffic for at most 1 second plus that handshake.
- The successor needs a crash-safe record, written before each first use.
- Previous probe keys outlive the 10 seconds of ARFC 0003 section 3.3.

## Alternatives rejected

- Discard every session on failure: re-handshakes sessions nobody touched.
- Reserve counter bounds in the record: a synced write per key and queue on the data path, and rekeyed sessions still need the consumed mark.
- Hand counters back to the old process: lost when the successor crashes.
- Responder switches probes on a timer: fails when a lost response is retried late.

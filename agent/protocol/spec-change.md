# Protocol: specs and spec changes

## ARFCs

An ARFC (Alloy RFC) is the normative design for one subsystem. ARFCs live in
`spec/`, are indexed in `spec/README.md`, and follow `agent/templates/arfc.md`.
Only the architect role edits them.

## Status

| Status | Meaning | Changing it |
|---|---|---|
| `draft` | Under design. Not yet a basis for build tasks | Free |
| `accepted` | Normative. Build and verify tasks cite its sections | Rules below |
| `frozen` | A released wire format, on-disk format, or stable API depends on it | Pivotal |

Each section of an ARFC may carry its own status when the document is mixed.

## Changing an accepted ARFC

| Change | Process |
|---|---|
| Clarification, no behavior change | Architect edits in a `spec` task. No ADR |
| Behavior change | `spec` task plus an ADR, then one review. Affected build and verify tasks are recut |
| Change to a frozen section | Pivotal. The lead asks the pilot first |

## How a change starts

Any role that finds a spec wrong, silent, or self-contradictory reports a
`spec-issue` and, if it cannot proceed, a `blocked` status. It does not pick an
interpretation and build on it. The lead dispatches the architect, who rules in
writing by editing the ARFC.

If the gap is small and the task can proceed either way, the role continues with
the reading most consistent with `agent/domain/invariants.md` and states that
reading in its report, so the architect can confirm or reverse it.

## Code never leads the spec

An implementation detail that other crates, the network, the disk, or a user can
observe is behavior, and behavior is specified before it is built. Internal
structure that nobody else can observe is the developer's call.

## Normative language

ARFCs use "must", "must not", "should", and "may" in their plain senses. Every
"must" is testable and has a conformance test. A "must" that cannot be tested is
rewritten or removed.

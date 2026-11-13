# ADR 0056: Finalizing an upgrade rewrites stored seam bindings

- Status: accepted
- Subject: operations
- Specs: arfc-0001, arfc-0017

## Context

Seam bindings are stored in control plane state with a configuration version
(ARFC 0001 clause 3.13). Writers kept the old version until finalization, and
nothing rewrote a stored binding afterwards, so a binding left untouched at an
older version became unreadable two releases later once `MIN_VERSION` rose.

## Decision

Each consensus group holds a write version per implementation, and refuses a
binding write at any other version. Finalizing the upgrade to release `R` checks
that release `R` decodes every stored binding, then switches: one write per group
records `R` as finalized and sets every write version to `V(R)`, and from then no
node runs release `R - 1`. It then rewrites every stored binding not at `V(R)`
with a write conditional on the resource generation it read, and completes only
when none remains. The upgrade to `R + 1` does not start until then. `VERSION`
never decreases, and `MIN_VERSION` of release `R + 1` is no newer than `V(R)`. A
rewrite is not a binding change. `alloy-seam` registers every implementation's
configuration type on every supported host and exposes it by kind and name.

## Consequences

- Every release decodes at most the versions from its predecessor's `VERSION` to
  its own, and stored bindings stay readable across any number of upgrades.
- Rollback to `R - 1` is possible until the switch, not until completion, and a
  binding release `R` cannot decode stops finalization before the switch.
- A binding that the rewrite cannot replace keeps finalization incomplete, which
  blocks both rollback and the next upgrade until the resource is rebound or
  deleted.
- The control plane compares bindings by decoded configuration, not by bytes:
  `SeamConfig` requires `PartialEq` and `alloy-seam` gains `ConfigCodec`,
  `register_config`, `config`, and `configs`.
- A new cluster starts with its first release's finalization complete.

## Alternatives rejected

- Each release reads every version ever written: `MIN_VERSION` could never rise,
  so decoders for every past format live forever.
- Rewrite lazily on next use: a binding nobody touches is never rewritten.
- Write nothing at `V(R)` until every binding is rewritten: the rewrite itself
  writes `V(R)`, so it would have to be one atomic write of every binding.
- Rewrite with the controller of the node that owns the binding: nodes may lack an
  implementation (clause 3.2), so the rewrite needs configuration types that are
  present on every host anyway.

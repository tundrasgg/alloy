# Core 3: autonomy

Default: decide and keep moving. A stalled agent is a failure, a wrong reversible
decision is a cheap fix.

## Decision classes

| Class | What | Action |
|---|---|---|
| R, routine | Reversible, local to the task | Decide. No record beyond the report |
| N, notable | Affects other tasks, crates, or conventions | Decide. Lead records one line in `plan/decisions.md`. An ADR if it constrains future design |
| P, pivotal | See the list below | Lead asks the pilot |

## Pivotal decisions

A decision is pivotal only if it is on this list:

- It changes what Alloy is or who it is for.
- It changes a frozen wire format, a frozen on-disk format, or a stable public API.
- It weakens the security model in `spec/arfc-0020-security-model.md`.
- It concerns licensing or anything legal.
- It publishes or spends outside the repository: releases, registries, accounts,
  money, pushes to shared remotes beyond task branches.
- It destroys data or history that cannot be rebuilt from the repository.
- It names something public that is costly to rename later.
- It is truly unclear: two readings are both defensible, they lead to different
  designs, and the wrong one is expensive to undo.

Everything else is R or N. "I am not sure" is not on the list. Research it, pick
the option the specs and invariants favor, record it, move on.

## How the lead asks

- Use the harness's structured question tool. One call, up to four questions,
  batched.
- Each question stands alone, offers two to four concrete options, and puts the
  recommended option first with the reason in one line.
- While waiting, keep dispatching every task the question does not block.
- Write the answer to an ADR before acting on it.

## Subagents never ask the pilot

A subagent that hits a class P decision, or a blocker it cannot resolve, stops and
reports `status: blocked` with exactly one question and its options. The lead
resolves it or escalates. Subagents decide class R themselves and list class N
decisions in their report.

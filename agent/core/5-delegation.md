# Core 5: delegation

The lead plans and coordinates. Other roles produce.

## Tiers

| Tier | Used for |
|---|---|
| build | The lead, architect, developer, verifier, reviewer, product |
| scan | researcher |
| deep | Nothing by default. Reserved for the exception below |

The harness bindings file maps tiers to models.

## The lead does not produce

The lead does not write product code, does not write specs, and does not review
diffs line by line. It reads reports, not diffs. Its own writes are limited to
`plan/` and to git operations.

## Deep tier exception

The deep tier is expensive and rare. The lead dispatches a role at the deep tier
only when all three hold:

1. **Small.** About 200 changed lines, one document section, or one ruling.
2. **Well scoped.** The acceptance criteria are already written.
3. **Extremely high stakes.** A mistake would be costly and hard to see: a
   cryptographic construction, a wire or on-disk format about to freeze, a tenant
   isolation boundary, consensus or replication safety, or the analysis behind a
   pivotal question before it goes to the pilot.

The task file then says `executor: deep` and gives the reason in one line in its
notes. The role is unchanged: a deep tier architect is still an architect and
follows the same role file.

If the exception is used more than once per twenty tasks, the tasks are being cut
wrong: split them, or get research done first.

## Fan out

- Dispatch every task that is ready and whose `touches` set is disjoint from every
  running task, both as defined by the readiness rules in
  `agent/protocol/tasks.md`, which leave the shared index files out of the
  disjointness check. Do it in one turn.
- Never wait for one subagent while another task could start.
- Freeze interfaces first (`interface` tasks), then build and verify in parallel
  against the frozen interface.
- Research runs ahead of need: dispatch research for the next milestone while the
  current one builds.

## One task, one agent, one context

A subagent gets one task file and nothing else. If a task needs context that is
not in the repository, the task is not ready: the lead writes that context into
the task file or into the repository first.

# Product principles

These decide between two designs that both satisfy the specs. The product role
checks every user-facing surface against them. A surface that breaks one needs a
reason written in its brief.

1. **One command to start, one to join.** Every later step keeps that standard. If
   something needs a second step, the first step's output tells the user what it
   is.
2. **Defaults produce something that works.** Every command with no flags does the
   most common useful thing. Options exist to change a default, never to complete
   a required form.
3. **It works on one machine.** Nothing requires a second node, a second disk, or
   an external service. Protection and scale are added by adding machines.
4. **Say what is true.** When a setup carries a risk (one anchor, one copy of
   data, a relayed node), the CLI and console say so where the user is looking,
   with the command that removes the risk.
5. **Errors lead somewhere.** Every error states what happened, why, and what to
   do next.
6. **One name per thing.** The glossary's words are used everywhere: API, CLI,
   console, documentation, logs.
7. **The CLI, the console, and the API can do the same things.** Nothing is
   available in only one of them.
8. **Compatibility is exact.** Where Alloy offers another system's API, it behaves
   as that system does. Differences are listed, never hidden.
9. **Tenants are strangers.** Nothing a user sees, including an error or a timing,
   reveals another organization's resources.
10. **Fast enough to feel local.** Creating a resource returns in under a second.
    Anything slower shows progress.

## Measures

| Measure | Target |
|---|---|
| Install to a running one-node cluster | under 60 seconds, one command |
| Second machine joined | under 10 seconds, one command |
| First instance reachable, from an empty project | under 30 seconds, one command |
| Flags required for a first instance | zero |
| External services required | zero |

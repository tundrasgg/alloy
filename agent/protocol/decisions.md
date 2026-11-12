# Protocol: decisions

Decisions are recorded in one of two places, by weight.

## The decision log

`plan/decisions.md`, written only by the lead. One line per class N decision:

```
- D-NNNN (T-NNNN) <decision in one sentence>. Why: <reason in one sentence>.
```

Use it for choices that other tasks need to know about but that do not constrain
design: a crate name, a module layout, a test seed policy, which of two libraries
on the allowlist a crate uses.

## ADRs

`adr/NNNN-slug.md`, written by the architect from `agent/templates/adr.md`, at
most 40 lines. Write an ADR when a decision:

- constrains future design,
- chooses between alternatives that a reasonable engineer would debate,
- answers a pivotal question, or
- reverses or supersedes an earlier ADR.

ADRs are never edited after they are accepted, except to set
`status: superseded by NNNN`. A changed mind is a new ADR.

## Ids

The lead allocates `D-` and ADR numbers from `plan/STATE.md`. An architect task
that must write an ADR is given the number in its task file.

## Finding decisions

`adr/README.md` indexes every ADR by subject. Before proposing a design choice,
check whether it has already been made.

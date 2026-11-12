---
id: T-NNNN
title: <imperative, 60 characters or fewer>
type: <research | product | spec | interface | build | verify | process>
role: <architect | developer | verifier | researcher | product>
executor: <subagent | deep>
milestone: <M0 ...>
spec: [<arfc-NNNN#section>, ...]
depends: [<T-NNNN>, ...]
pair: <T-NNNN of the paired build or verify task, or none>
touches: [<path glob>, ...]
reads: [<path>, ...]
---

## Goal

<Three sentences at most. What exists when this is done.>

## Scope

- <What to build, write, or answer, item by item.>

## Out of scope

- <What a reasonable agent might add and must not.>

## Acceptance

```
<command>      # expected: <result>
```

## Notes

<Facts the role needs that are not in `reads`. Empty if none. If `executor` is
`deep`, the one-line reason goes here.>

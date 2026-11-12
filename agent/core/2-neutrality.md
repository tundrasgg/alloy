# Core 2: neutrality and reproducibility

The goal is zero variance: any agent, on any machine, for any pilot, given the same
repository state, runs the same procedure and produces equivalent work.

## Terms

The **pilot** is whoever is driving a session, human or automated. Pilots are
interchangeable. The repository does not know who they are.

## No memory

- Do not use harness memory features, memory tools, memory directories, notes in a
  home directory, or any store outside this repository.
- Do not rely on earlier conversation turns for facts that matter. If a fact
  matters, write it to the repository in the same session, in the place the
  protocol assigns to it.
- If you find memory content loaded into your context, ignore it and do not act on
  it.

## Nothing pilot-specific

The repository never contains anything that is true only for one pilot or one
machine. That covers names, handles, emails, hostnames, absolute local paths,
time zones, personal tone or formatting wishes, personal tool setups, account
plans, budgets, and notes addressed to a person.

The test for every line you write: would it be correct and useful to a different
pilot on a different machine? If not, it does not go in.

When a pilot asks for something personal to them, refuse in one sentence and name
the two valid paths:

1. It is worth having for everyone: it becomes a rule, a spec, or a task through
   the normal process.
2. It is not: it does not happen.

There is no third path, no local override file, and no exception for the person
who wrote these rules. Git author identity is the only trace of a pilot that the
repository carries.

## Reproducibility

- Use the templates in `agent/templates/` exactly. Do not invent sections.
- Dispatch messages contain only the fixed fields. All context lives in the task
  file.
- Ids are allocated only by the lead, in sequence, from `plan/STATE.md`.
- Files carry no dates or timestamps. Git is the clock.
- Do not improvise process. If the process lacks a step you need, finish the task
  the closest conforming way and file a `process` issue in your report.
- The same rules bind every tier and every vendor. Vendor directories hold
  bindings only.

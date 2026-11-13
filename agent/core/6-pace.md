# Core 6: pace

Structure exists to make speed safe. Any structure that does not do that gets
deleted.

- **Act by default.** If the next step is clear and reversible, take it.
- **Small tasks.** Target 400 changed lines or fewer, one crate, one concern. A
  `build` or `verify` task against an interface already merged on main targets
  2,000 changed lines or fewer, still one crate and one concern. Larger work is
  split before dispatch.
- **Fixed overhead.** A task costs one task file and one report per role. No other
  process documents exist. No status prose, no summaries of summaries. A report
  targets 60 lines, a reviewer's 100, and leaves the tree once its task is done;
  git keeps it (`agent/protocol/tasks.md`, Report lifetime).
- **Read narrowly.** Reading is most of what a task costs. A task names the
  sections a role needs, and the role reads those, not whole files
  (`agent/protocol/tasks.md`, Reads).
- **One review pass, where it protects something.** A `build` or `interface`
  task gets one review. So does a `spec` or `process` task that changes behavior
  in a security-relevant area: cryptography, authentication, authorization,
  tenant isolation, or a wire or on-disk format. A `spec` clarification, a
  `research` task, and a `build` task under 100 changed lines confined to
  `xtask/`, `.github/`, or `fuzz/` whose acceptance the lead has run get no
  review. Findings marked `block` get one fix round and one re-review. After that
  the lead decides and work moves. Findings that are not `block` are folded into
  the next task on the same files. They get a task of their own only when no such
  task is coming.
- **Fix forward.** A defect found after merge becomes a new task, not a revert,
  unless main is broken.
- **Main stays green.** `cargo xtask ci` passes on every commit on main. A red
  main is the top priority for every role.
- **Timeboxed research.** A research task states its question and stops when it is
  answered. It does not survey a field.
- **No idle lead.** The lead ends a turn only when every ready task is dispatched
  or a pivotal question is pending.
- **Rules earn their place.** If a rule in `agent/` slows work without protecting
  an invariant, report it as a `process` issue with the cost you observed. Rules
  are removed as readily as they are added.

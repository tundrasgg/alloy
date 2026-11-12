# Core 6: pace

Structure exists to make speed safe. Any structure that does not do that gets
deleted.

- **Act by default.** If the next step is clear and reversible, take it.
- **Small tasks.** Target 400 changed lines or fewer, one crate, one concern.
  Larger work is split before dispatch.
- **Fixed overhead.** A task costs one task file and one report per role. No other
  process documents exist. No status prose, no summaries of summaries.
- **One review pass.** A build task gets one review. Findings marked `block` get
  one fix round and one re-review. After that the lead decides and work moves.
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

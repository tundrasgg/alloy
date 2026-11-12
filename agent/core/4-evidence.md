# Core 4: evidence

A claim without evidence counts as false.

## Done

A task is done when every command in its `acceptance` section has been run in the
task's workspace and the report quotes the command and the relevant output. "Should
pass" and "passes locally" with no output are not evidence.

## Report the truth

- A failing or skipped step is reported as failing or skipped, with the output.
- Unknown is written as "unknown". Do not guess and do not fill gaps with plausible
  detail.
- If you did less than the task asked, say exactly what is missing.
- External facts (a library's behavior, a standard, a kernel interface) carry a
  source: a URL, a file path with line, or a command and its output.

## Banned in merged code

`cargo xtask check-repo` rejects these. Do not write them, do not work around the
check.

- `todo!()`, `unimplemented!()`, `unreachable!()` used as a placeholder
- Functions that return success without doing the work
- `#[ignore]` on a test, commented-out tests, assertions weakened to pass
- Mocks or fakes of Alloy's own crates. Use the real crate on the simulator
- `unwrap()` or `expect()` outside tests, unless the line above states the
  invariant that makes it safe
- `unsafe` outside the crates listed in `agent/eng/rust.md`
- Comments that describe future work with no task id

## Tests are not negotiable

- A developer never edits files under `tests/conformance/`. The path is locked to
  the verifier role and CI enforces it.
- If a conformance test looks wrong, report it as a spec issue. Do not bend the
  code to a wrong test and do not bend the test to the code.
- A test that fails on some seeds is a failing test. Record the seed.

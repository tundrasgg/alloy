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
   as that system does. Differences are listed, never hidden. Ergonomics never
   change compatible behavior: the rules that make the native surfaces easy stop
   at the compatibility endpoints.
9. **Tenants are strangers.** Nothing a user sees, including an error or a timing,
   reveals another organization's resources.
10. **Fast enough to feel local.** Creating a resource returns in under a second.
    Anything slower shows progress.
11. **Beautiful because it is useful.** The terminal and the console look good
    through alignment, spacing, type, and restraint, never through decoration.
    Everything on screen carries information or an action. The native API is as
    easy to call as the CLI is to type.

The four sections below make these principles checkable for each surface. Each
rule has an id, so a brief or a review can cite it: "breaks T9".

## Terminal

The `alloy` command. PB-0001 is the reference transcript. ARFC 0019 section 3.1
fixes the grammar, exit codes, and output modes; these rules fix how the output
looks.

**Layout**

- T1. Standard output carries only the result: a table, a `get` view, the name
  of what was created, a join token or login code, or the API representation
  under `--output json` or `yaml`. Everything else goes to standard error: step
  lines, progress, warnings, errors, and the `Next:` and `Try:` lines. A command
  run with `2>/dev/null` prints only its result.
- T2. A command with several steps prints a heading line for each group of steps
  (`Checking this machine`), then one line per step, indented two spaces, that
  starts with a status word padded to six columns: `ok`, `done`, `warn`, or
  `fail`. It ends with the result, then the standing warnings that apply, then
  one `Next:` line when there is a next command (principle 1).
- T3. Prose wraps at 80 columns, or at the terminal's width when that is
  narrower. It breaks only at spaces, never inside a command, path, address, or
  identifier. Continuation lines keep the indentation of the line they continue.
- T4. Nothing is decoration: no boxes, banners, horizontal rules, ASCII art,
  emoji, or icons. One blank line separates groups. Two blank lines never appear
  in a row.
- T5. A command shown to the user runs as printed, with every value the CLI
  knows filled in: `alloy node approve node2`, not `alloy node approve <name>`.
  A `<placeholder>` stands only for a value the CLI cannot know.
- T6. Names, identifiers, addresses, and secrets are never truncated or
  shortened on screen.
- T7. Sizes and durations always carry a unit. Sizes use binary units with one
  decimal below 10 (`1.8 TiB`, `412 GiB`). Elapsed times under a minute have one
  decimal (`6.2 s`). Longer durations and ages use the two largest whole units
  (`2h 14m`, `3d 4h`). Sentences use words (`works for 24 hours`). A `get` view
  shows a timestamp as RFC 3339 in UTC followed by its age in parentheses.
- T8. Every command's `--help` shows its usage line, every flag with its
  default, and at least one example.

**Color**

- T9. Color marks what needs attention and nothing else. Red: the `fail` status
  word and states that mean broken, such as `unreachable`. Yellow: `warn`,
  `Warning:`, and degraded states. Green: the `ok` and `done` status words only.
  Bold: table headers, group headings, and the command on a `Try:` or `Next:`
  line. Dim: elapsed times and parenthetical hints. Healthy states in a table
  are not colored.
- T10. Color is never the only signal. Output with color removed has exactly the
  same words as output with color.
- T11. Only the 16 standard ANSI foreground colors, bold, and dim, so the user's
  terminal theme decides the shades. No background colors, no 256-color or
  24-bit codes, no blinking.
- T12. Color is decided separately for standard output and standard error. A
  stream is colored only when it is a terminal, `NO_COLOR` is unset or empty
  (https://no-color.org), `TERM` is not `dumb`, and `--color` is not `never`.
  `--color always` colors a pipe too, but never turns on progress drawing. On
  Windows, color is used only where the console accepts ANSI sequences.

**Progress**

- T13. A step's line appears when the step starts, not when it ends. On a
  terminal, while the step runs, its status column shows a spinner and, after
  1 second, the elapsed time. When the step ends, the line is redrawn once as its
  final `ok`, `done`, `warn`, or `fail` line, which stays.
- T14. Work of known size shows how far it is with numbers: `312 MiB of 2.1 GiB,
  48 MiB/s`. A bar may stand beside the numbers, never instead of them.
- T15. Drawing redraws at most 10 times per second, only on the lines being
  redrawn. It never clears or rewrites anything above them. The cursor is hidden
  while drawing and restored on every exit, Ctrl-C included.
- T16. When the stream is not a terminal there are no spinners, redraws, or
  carriage returns. Each step prints its final line once, when it ends. A step
  that runs longer than 30 seconds also prints one plain line every 30 seconds
  saying it is still running, with its elapsed time.
- T17. Step text comes from the API's operation (A16), so the CLI and the
  console show the same stages in the same words.
- T18. Ctrl-C while waiting stops the waiting, not the work, and prints the
  command that resumes the wait.

**Tables and machine-readable output**

- T19. A table has one header row in upper case, using glossary words (`NAME`,
  `STATE`), no borders, at least two spaces between columns, and no trailing
  spaces. The first column is the name. Text is left aligned, numbers and sizes
  right aligned. An empty value is `-`. Rows are sorted by the first column
  unless the command's documentation says otherwise.
- T20. A list's default columns fit in 100 columns when names are 20 characters
  long. Further fields are in the `get` view and in `--output json`.
- T21. An empty list prints no header. It prints one line on standard error that
  says what is empty and where, and how to create one: `No instances in project
  default. Create one with: alloy instance create`. The exit code is 0.
- T22. A `get` view prints one field per line: a label in glossary words, then
  the value, with all values starting in the same column. Nested fields are
  indented two spaces under their parent's label. The name and state come first,
  then the other fields in the API's order.
- T23. `--output json` and `yaml` print the API representation unchanged (ARFC
  0019 clause 3.1.4): never colored, never with warnings or progress, field names
  as the API spells them. A `list` prints the items of every page as one array.
- T24. A list on screen is never silently partial. `list` follows every page by
  default. When the user caps it, a line on standard error says that more exist.
- T25. `--output name` prints one full resource name (A2) per line, for pipes.
- T26. Scripts rely on exit codes (ARFC 0019 clause 3.1.7) and on `--output json`
  or `yaml`, which are stable within an API version. Table layout may change
  between releases.

**Errors**

- T27. An error is, on standard error: the message as one paragraph saying what
  happened and why, wrapped as T3 says, then one `Try:` line with the command
  that fixes it, when one exists (ARFC 0019 clause 3.1.8). Nothing else, except
  for an unexpected failure (code `Internal`), which adds the request id so a
  cloud admin can find the request in the audit log.
- T28. Never on screen: stack traces, Rust `Debug` output, raw gRPC or HTTP status
  text, internal type names, or any secret.
- T29. A usage error (exit 2) names the one wrong flag or argument, suggests the
  nearest valid one when it is a likely typo, and prints the command's usage line,
  not its whole help.
- T30. A destructive command's confirmation (ARFC 0019 clause 3.1.6) says what
  will be deleted, including what goes with it, then asks for the name: `Delete
  instance web and its 2 volumes? Type web to confirm:`.

## Web console

The console in the browser. ARFC 0019 section 3.2 fixes what it can do; these
rules fix how it feels. The console follows Linear's design language as closely
as it can (D-0067): its layout, spacing, type, color, and interactions. Raycast
and Notion are secondary references. Alloy copies no logo, name, icon, image, or
code from any of them.

**Keyboard first**

- C1. Every screen and every action can be reached with the keyboard alone, in a
  visible focus order. The focus is always visible.
- C2. The command palette opens with Ctrl+K, or Cmd+K on macOS, from every
  screen. It lists every action the current screen offers, every resource kind
  to go to, and resources in the current project by name. Each entry shows its
  shortcut when it has one. Typing filters actions and destinations with no
  network request. Enter runs the selected entry and Esc closes the palette.
- C3. Palette actions use the CLI's verbs and nouns (`Create instance`, `Delete
  volume`), and each shows the equivalent CLI command (ARFC 0019 clause 3.2.3).
- C4. `?` lists every shortcut. Esc closes the topmost dialog. Single-character
  shortcuts work only when focus is not in a text field, and the user can turn
  them off (WCAG 2.2 success criterion 2.1.4). No shortcut replaces one that the
  browser or the operating system already uses.
- C5. Every screen has a URL. Filters, sort order, and the selected tab are part
  of it. Back and forward work. A copied URL opens the same view for another
  member of the organization.

**Density**

- C6. Lists are tables, not cards. At 1280 by 800 pixels, with navigation shown,
  a list shows at least 20 rows. A row is at most 36 pixels high.
- C7. Body text is 13 or 14 pixels. There are no illustrations, hero images,
  marketing copy, or tips unrelated to the screen.
- C8. A create form opened and submitted without changes creates a working
  resource (principle 2, ARFC 0019 clause 3.1.3). Fields with defaults are shown
  filled in with them.
- C9. Every identifier, address, and CLI command on screen can be copied with
  one click or one keystroke.

**Speed**

- C10. Lists and detail screens update live from `Watch`. No refresh button is
  needed. Data shown while the connection is down is marked stale with its age
  (ARFC 0019 section 6).
- C11. Moving between screens never reloads the page. A screen whose data is
  already loaded renders in under 100 ms, and every input is acknowledged on
  screen in under 100 ms.
- C12. Nothing is shown as done before the API says so. A change shows as
  pending until it is confirmed, and an operation shows its stages as the CLI
  does (T17).

**Color and typography**

- C13. The interface is neutral grays and one accent color, used for focus,
  selection, links, and the primary action. Green, yellow, and red appear only
  on states and warnings, always next to a word, with the meanings of T9.
- C14. Text contrast is at least 4.5 to 1 and the contrast of controls and focus
  indicators at least 3 to 1 (WCAG 2.2 level AA). There are a light and a dark
  theme, following the operating system's setting by default.
- C15. Two type families: Inter, self-hosted, for text, and the system monospace
  font for identifiers, addresses, commands, and keys. The whole console uses at
  most four text sizes and three weights. Numbers in tables use tabular figures and are right aligned.
- C16. Motion lasts 150 ms or less and only shows where something came from or
  went. There is none when the user's system asks for reduced motion.

**Errors and warnings**

- C17. An error shows the API's message next to the field or action it concerns,
  with the next step as a button or a copyable command. It stays until it is
  fixed or dismissed. No error says only that something went wrong.
- C18. A destructive action asks for the resource's name, as the CLI does (ARFC
  0019 clause 3.1.6).
- C19. An empty list says what is empty, and offers the create action and its CLI
  command (T21).

**Copy**

- C20. Text shows state; it never narrates the interface or repeats an
  instruction. No hints such as "(generated, edit to change)" or "press Enter
  to …" in the body: shortcuts appear as keycaps, explanations in tooltips.
- C21. Every string is the shortest thing that is true and useful at a glance.
  Detail a user did not ask for goes in a tooltip or a documentation link, never
  inline. An action that an icon expresses, such as copy, is an icon button
  with a tooltip, not a word.

## Native API

ARFC 0008 fixes the model. These rules make every kind behave alike, so a user
who has learned one kind knows them all.

**Names**

- A1. A kind is a glossary noun: singular and PascalCase in Protocol Buffers
  (`Instance`, `PublicAddress`), plural, lower case, and hyphenated in resource
  names and HTTP paths (`instances`, `public-addresses`). The CLI noun is the
  singular of the same words (`alloy public-address list`).
- A2. Every resource has one full name: its parent's full name, then the
  collection and the resource's name, as in
  `orgs/acme/projects/web/instances/api-1`. Errors, audit records, console URLs,
  and `--output name` all use it.
- A3. A name the user chooses is a lower case DNS label: a letter, then letters,
  digits, or hyphens, 63 characters at most. It is unique within its parent and
  never changes. Every resource also has an immutable `uid`. A Create without a
  name gets a generated one. Names that a compatibility API defines, such as
  bucket names, follow that API's rules instead.
- A4. Common fields have one name in every kind: `name`, `uid`, `labels`,
  `state`, `create_time`, `update_time`, `generation`. Field names are snake case
  and use glossary words with no abbreviation the glossary lacks. A field's name
  carries its unit: timestamps end in `_time`, sizes are integers in bytes ending
  in `_bytes`, durations end in `_duration`. Booleans name the positive case
  (`enabled`, not `disabled`).
- A5. An enum value maps mechanically to the word the CLI and the console show:
  drop the type prefix, lower case it, and turn `_` into `-`, so
  `MESH_RELAY_ONLY` is `relay-only`. That word is the glossary's word. The zero
  value, `*_UNSPECIFIED`, is never shown to a user.

**Methods**

- A6. Methods are the standard six and `Verb` + `Kind` (ARFC 0008 section 2). A
  custom method exists only for an action that is not a change of fields:
  `StartInstance`, `ApproveNode`. Every method is one CLI command, `alloy <noun>
  <verb>`, and one console action with the same verb (principle 7).
- A7. Create and Update return the resource, Delete returns nothing, each either
  directly or as an operation's result (A15). Get and List never change anything
  and can be called with HTTP GET.

**Pagination and filtering**

- A8. Every List takes `page_size`, `page_token`, `filter`, and `order_by`, and
  returns `next_page_token`, which is empty on the last page (ARFC 0008 clause
  3.4). A page size above the maximum is reduced to it, not refused. A page token
  used with a different filter or order is refused with `InvalidArgument`. The
  default order is by name.
- A9. One filter grammar serves every List, the CLI's `--filter`, and the
  console's filter field, and is documented once. Labels are filtered as
  `labels.<key>`. An unknown field is refused with `InvalidArgument` naming the
  field and its position in the expression.

**Errors**

- A10. Every error has `code`, `message`, `resource`, and `details` (ARFC 0008
  clause 3.6). Each code maps to one HTTP status and one CLI exit code, listed in
  one table in the definition of the codes.
- A11. The message is written for a person and follows principle 5, with no
  internal names. When a call or command fixes the error, `details` carries it
  as a structured next step, the API call and its CLI form, so the CLI's `Try:`
  line and the console's button come from the API and agree, as standing
  warnings do (ARFC 0019 clause 3.2.5).
- A12. A request with invalid input is refused once, listing every invalid field
  with its path and the rule it breaks, not only the first.
- A13. A resource of another organization answers exactly as a missing one:
  `NotFound`, the same message, the same timing (principle 9).

**Idempotency and operations**

- A14. Every mutating method takes `idempotency_key` (ARFC 0008 clause 3.2).
  Generated clients, the CLI, and the console set a new key on every mutating
  call and reuse it on every retry of that call, so a user never has to. A key
  sent again with a different request is refused, and the error says so.
- A15. A method either always returns an operation or never does, and its
  documentation says which. A method that can take more than 1 second returns an
  operation (principle 10).
- A16. An operation (ARFC 0008 clause 3.3) carries `name`, `done`, `error`, and
  `result`, plus its progress: the current stage as one short sentence, the
  stages finished, and, when the size of the work is known, the amount done and
  the total. Operations are resources: they can be read with Get, List, and
  Watch. `--no-wait` prints the operation's name and the command that waits for
  it.

**Clients**

- A17. Every client is generated from the Protocol Buffers definitions (ARFC 0008
  clause 3.10): the Rust client, the console's TypeScript client (R-0007), and
  the OpenAPI document for every other language. No hand-written client or SDK
  exists beside them.
- A18. Every generated client has the same three helpers: iterate a list across
  all its pages, wait for an operation, and retry `Unavailable` and
  `RateLimited` with backoff and the same idempotency key.
- A19. Names in a generated client are the definitions' names in that language's
  case convention. Documentation comes from the definitions' comments (ARFC 0008
  clause 3.11), and each method's documentation has an example request.
- A20. The JSON over HTTP form of every method can be called with `curl` from the
  documentation alone: one documented path per method, the standard Protocol
  Buffers JSON mapping, and the same error body as every other form.

## Compatibility APIs

The EC2 API (ARFC 0016), the S3 API (ARFC 0014), and the OCI registry API (ARFC
0015) extend principle 8. Users arrive with tools they already have. Those tools,
not Alloy, define what correct means.

- X1. A compatibility endpoint behaves as AWS does for every supported call:
  request parsing, parameters and their defaults, response bodies and headers,
  HTTP status codes, error codes and error bodies, pagination tokens, ordering,
  identifier formats, and limits. The OCI registry behaves as the OCI
  Distribution Specification says. Where AWS's documentation and AWS's behavior
  differ, Alloy matches the behavior and lists the case as a difference.
- X2. None of the native rules above apply there. No `Try:` lines, no
  Alloy-only headers, parameters, fields, or error codes, no defaults that differ
  from AWS's. The only Alloy text allowed is the native error code inside the
  message of a mapped error (ARFC 0016 section 6).
- X3. An unsupported action or parameter fails with that API's own error for the
  case (`UnsupportedOperation` for EC2, `NotImplemented` for S3). Nothing
  unsupported is silently ignored.
- X4. Every difference is listed, in one document per API generated from the
  table the translator uses (ARFC 0016 clause 3.7 for EC2). Nothing differs that
  the list does not name.
- X5. Tenant isolation and the security model outrank compatibility. Where exact
  behavior would reveal another organization's resources (principle 9) or weaken
  ARFC 0020, Alloy takes the safer behavior and lists it as a difference.
- X6. Compatibility is measured by the pinned suites (ARFC 0014 clause 3.9, ARFC
  0015 clause 3.2, ARFC 0016 clause 3.6), with the tools unchanged except for
  endpoint and credentials.
- X7. The native surfaces help users cross over without changing the
  compatibility ones: `alloy <noun> get`, `--output json`, and the console show a
  resource's compatibility identifier, such as its EC2 id, and a project's EC2
  account id, so a user can match what the AWS tools print.

## Measures

| Measure | Target |
|---|---|
| Install to a running one-node cluster | under 60 seconds, one command |
| Second machine joined | under 10 seconds, one command |
| First instance reachable, from an empty project | under 30 seconds, one command |
| Flags required for a first instance | zero |
| External services required | zero |
| Terminal: CLI start to first output, no network | under 50 ms (ARFC 0019 section 7) |
| Terminal: a step's line shown after the step starts | under 100 ms |
| Terminal: progress redraws | at most 10 per second |
| Terminal: prose width | 80 columns at most |
| Terminal: default list width, names of 20 characters | 100 columns at most |
| Terminal: colors | 16 ANSI foreground colors, bold, and dim. None off a terminal or with `NO_COLOR` set |
| Console: initial load | under 300 KiB compressed (ARFC 0019 section 7) |
| Console: screen change with data loaded, and input acknowledged | under 100 ms |
| Console: command palette open and filtered | under 100 ms, no network request |
| Console: list rows visible at 1280 by 800 | at least 20 |
| Console: actions reachable by keyboard alone | all |
| Console: contrast | 4.5 to 1 for text, 3 to 1 for controls (WCAG 2.2 AA) |
| Console: font files downloaded | zero |
| Native API: methods without a CLI command and a console action | zero |
| Native API: mutating methods without `idempotency_key` | zero |
| Native API: required fields to create a first instance | zero |
| Native API: hand-written clients | zero |
| Compatibility APIs: pinned suites passing | all |
| Compatibility APIs: Alloy-only headers, parameters, fields, or error codes | zero |
| Compatibility APIs: differences not in the list | zero |

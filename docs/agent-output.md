# AXI and TOON, evaluated against 5w's output

Task #12. No `docs/` convention exists in this repository yet (README.md is the only prose doc,
per CLAUDE.md: "README.md is the user's view"). This evaluation is reasoning about a hypothetical
change, not a description of a shipped one, so it does not belong in README.md's "Keeping context
small" section, which describes what 5w does today. It lives here instead; a reviewer who adopts
any of the proposed changes should fold the relevant bit of this file's reasoning into README.md
at that point, per CLAUDE.md's "reasoning in the README rather than in error text."

Commands were run from this worktree, off a terminal (`std::io::stdout().is_terminal()` is false
under the tool that ran them, so `5w`'s own compact-by-default rule was already in effect without
`FIVEW_AGENT` set). `5w --version` was 0.1.3 at the time.

## AXI — https://axi.md — 10 design principles

### 1. Token-efficient output (use TOON instead of JSON)

Given its own section below, since it's most of the task. Short version: **reject** TOON as a
shipped `--toon` format; **adopt**, cheaply, a trimmed field set for `--json` (see #2).

### 2. Minimal default schemas (3-4 fields per list item, not 10+)

**Already met** for 5w's actual default (what an agent gets running `5w ready` with no flags,
which is compact off a terminal or under `FIVEW_AGENT=1` — `src/tasks.rs` `compact_default()`):

```
#1 !1 @release Attach SHA256SUMS.asc to v0.1.2: ... >restricted
11 ready · 1 >manual · 0 blocked
```

That's id, level, area, title, plus branch/lane/note only when present — already at AXI's 3-4
field target, and it's the format an agent sees by default, not an opt-in.

**Not met** for `--json`, which is the one output mode that ignores this principle: `Q::json()`
in `src/tasks.rs` (~line 262) always emits 11 fields per task — `id, state, level, area, lane,
kind, title, branch, needs, unmet, rework` — even for a bare `ready --json`:

```
{"id":1,"state":"open","level":1,"area":"release","lane":"restricted","kind":"restricted",
"title":"Attach SHA256SUMS.asc to v0.1.2: ...","branch":null,"needs":[],"unmet":[],"rework":null}
```

**Adopt**: trim `--json`'s default row to the fields that matter for a list (id, state, level,
area, title, branch), moving `kind`, `needs`, `unmet`, `rework` behind the existing `--full` flag
(which already exists for the human/compact split and could gate JSON too). Measured effect: on
the 12-task queue, dropping to 4 fields cuts a JSON array of these rows from 783 to 448 tokens
(cl100k_base) — see the TOON section for the full table. This is a same-day change with the tools
5w already has (`--full` already exists); it does not need a new format.

### 3. Content truncation (truncate with a size hint, e.g. `--full` to see the rest)

**Partially met.** Titles over `title_max` (120, `.5w.toml`) are split into title + body at
write time (`5w add`, `5w split`), and a compact row marks a non-empty body with `+`
(`src/tasks.rs` `row()`, ~line 236: `let body = if t.body.is_empty() { "" } else { " +" };`).
`5w show <id>` and `5w delegate <id>` print the body in full. This is functionally a truncation
hint, but the `+` mark's meaning ("this task has a body") is documented only in README.md's
"Keeping context small" section, not in `5w --help`'s own text — an agent that has only read
`5w ready --help` has no way to learn what `+` means from the tool itself.

There is also a second, narrower gap: `doctor` can report titles that are *still* over
`title_max` (`src/tasks.rs` `doctor()`: `"{long} open titles over {} chars — 5w split"`) — this
happens between a manual edit to TASKS.md and the next `5w split`. When that's true, the compact
row's own defensive truncate (`truncate()` in `src/util.rs`: `format!("{}…", t.trim_end())`)
prints a bare `…` with no count and no pointer to the fix, unlike AXI's
`(truncated, 2847 chars total — use --full to see complete body)` example.

**Adopt**: when `row()`'s `truncate()` actually cuts a title (not just when body exists), append
a hint naming the count and the fix, e.g. `…(212 chars — 5w split)`. And add one line to
`5w ready --help`'s usage text (or the row output itself) explaining `+`.

### 4. Pre-computed aggregates (counts, CI-style summaries, inline)

**Already met.** Examples, all from this session:

- `5w ready` (non-JSON): `11 ready · 1 >manual · 0 blocked` — one line, no follow-up count query.
- `5w levels`: `delegable by level: !1 5 · !2 4 · !3 1 · !4 1`, `open by lane: ...`,
  `closed: 0 in queue, 0 archived`.
- `5w doctor`: `ok — 12 queued, 0 archived`.
- `5w review`: prints the diffstat and how far a branch has drifted since submit inline
  (`git diff --shortstat`, "+N commits" since submit), rather than making the agent run `git log`
  itself.

Nothing to change here; this is the AXI principle 5w already leans on hardest.

### 5. Definitive empty states

**Already met.** `(nothing ready)`, `(nothing submitted)`, `(no reports)`,
`no titles over 120 chars` (doctor), `nothing closed to archive` — every read/write path that can
return nothing says so explicitly rather than printing zero bytes. Confirmed by running `5w
review` and `5w report list` on this queue (no submissions, no reports yet in this worktree).

### 6. Structured errors & exit codes

Three separate claims here; verdicts differ.

**Errors on stdout, not stderr — reject.** 5w writes refusals to stderr
(`main.rs`: `eprintln!("5w: {e}"); std::process::exit(1);`), confirmed by redirecting stdout and
stderr separately for `5w show 9999` (`no task #9999` landed only in the stderr file). This is the
opposite of AXI's suggestion, deliberately: CLAUDE.md's own rule is "refusals one line naming the
fix," and keeping them off stdout means `5w ready --json | some-parser` never has to distinguish
a refusal line from a data line — the parser can assume stdout is always well-formed. An agent
harness that runs 5w (including the one used for this task) already sees stdout and stderr
together by default. Mixing errors into stdout would trade that guarantee for nothing.

**Idempotent mutations — reject, with reason.** AXI wants a repeated mutation to be safe to retry.
5w's are deliberately *not* that: `5w accept 12` on an already-closed task refuses
(`#{id} is already closed`) rather than silently no-op-succeeding. This is intentional per
CLAUDE.md ("never accept, close or ship work you were handed") and PROTOCOL.md's review gate —
5w's mutations are gated state transitions, not idempotent upserts, and a silent no-op on a
repeated `accept` would be exactly the kind of masked mistake the queue exists to prevent (e.g. an
agent replaying a stale script against a task that was since re-opened and changed). Keeping this
as a refusal is correct for 5w's model even though it departs from AXI's letter.

**A third exit code for "you asked for something that doesn't exist" — adopt, and there's a real
bug adjacent to it.** 5w has exit 0 (success) and exit 1 (error) only; there's no code 2. More
importantly, unrecognized flags on every read command (`ready`, `next`, `ls`, `blocked`, `all`,
`show`, `review` — anything going through `opts()` in `src/tasks.rs`) are **silently accepted and
ignored**, not refused:

```
$ 5w ready --bogus; echo $?
#1 !1 @release ...
...
11 ready · 1 >manual · 0 blocked
0
```

`opts()` pushes any unknown `--flag` into `o.flags` (used only by `review --checklist`) and never
validates it against what the current command understands. This is precisely the failure mode AXI
principle 6 names — "an agent that invents a flag learns it did nothing instead of trusting
unscoped output" — and 5w currently does the silent-nothing thing. Contrast with `5w add`, which
does refuse unknown fields (`validate_field()` bails with `not a field: ...`). **Adopt**: make
`opts()` reject flags a given command doesn't understand (exit 1 is enough; a dedicated exit 2 is
optional polish, not the fix that matters).

### 7. Ambient context (install into session hooks so state is visible before the agent acts)

**Reject — not applicable to 5w's invocation model.** 5w has no daemon and no session hook; every
read is an explicit `5w ready` / `5w delegate <id>` call. That's by design (PROTOCOL.md, the
review gate) — state changes happen through commits, so "ambient, always-current" state is what a
git worktree already gives an agent for free, and a background-injected hook would fight the
"work goes through the queue" model in CLAUDE.md. The closest available equivalent is already
built: `5w delegate <id>` assembles everything a worker needs in one call — the task, its body,
`refs:` (sections to read, not whole files), `docs:`, `conventions:`, `tier:`, and the exact next
commands — so an agent's *first* call already surfaces the full context, no separate discovery
turn required. That is AXI's goal, achieved through an explicit command rather than a hook.

### 8. Content first (no-args run shows live data, not help)

**Already met.** `main.rs` `dispatch()`: `let cmd = args.first().cloned().unwrap_or_else(||
"ready".into());` — bare `5w` runs `ready`, confirmed: running `5w` with no arguments printed the
same ready list as `5w ready`. Help only appears on `-h`/`--help`/`help`, never by default.

### 9. Contextual disclosure (`help[]`-style next-step hints)

**Partially met — adopt for the common list commands.** `5w next` already does this exactly:

```
#1 [open] !1 @release ...
→ 5w delegate 1
```

`5w doctor`'s notes embed the fix command inline (`` `5w update-files` ``, `` `5w split` ``), and
`5w delegate`'s footer *is* a next-step block (`steps: ...`). But `5w ready` and `5w review` —
the two most-run list commands — stop at their aggregate line with no pointer to the obvious next
action. **Adopt**: append a `→ 5w delegate <id>` hint (the top ready task) to `ready`'s summary
when the list isn't empty and the caller isn't asking for `--json`/`--ids`; similarly point
`review` at `5w accept <id>` / `5w reject <id> <reason>` for its top row.

### 10. Consistent way to get help (concise `--help` per subcommand)

**Partially met — adopt.** `wt`, `ship`, `lint`, `ci`, and `report` each have their own `USAGE`
constant and print it on `--help`. Every other subcommand does not: `main.rs` (~line 73) only
special-cases `ship` vs. the rest, so `5w show --help`, `5w ready --help`, `5w submit --help`,
etc. all print the *entire* global `tasks::USAGE` block — confirmed by running `5w show --help`,
which prints the same ~530-token, 40-line listing as `5w --help`, not a line about `show`. Each
subcommand's one-line summary already exists as text inside that block (e.g. `show <id> [--json]
one task, archived ones included`); it's just never printed alone. Measured: the full usage block
is 532 tokens (cl100k_base); the one relevant line is 20. **Adopt**: extract each subcommand's
existing one-liner and print just that (plus any fuller usage string a command already builds,
e.g. `add`'s `usage:` string) on `<cmd> --help`, falling back to the full listing only for bare
`5w --help`.

## TOON — https://toonformat.dev, spec v4.1.1 (github.com/toon-format/spec)

TOON is an indentation-based, YAML-flavoured encoding of the JSON data model, with a tabular form
(`name[N]{f1,f2,...}:` header, one delimited row per array element) as its main lever against
JSON's per-object brace/quote/key repetition. Quoting is conditional (§7.2: quote empty strings,
numeric-looking strings, `true`/`false`/`null`, strings containing structural characters or the
active delimiter, strings starting with `-`/`#`); keys need no quoting when they match
`^[A-Za-z_][A-Za-z0-9_.]*$`. `[N]` is a declared length the decoder validates against, which is
the format's answer to AXI's "guard against truncation passing undetected."

### Measured comparison (this repo's actual queue, 12 open tasks, `5w all --json`)

| output | fields/row | bytes | tokens (cl100k_base) | vs. current `--json` |
|---|---|---|---|---|
| `5w --json` today (JSON Lines) | 11 | 2734 | 788 | — |
| same data as one JSON array | 11 | 2745 | 783 | ~0% |
| TOON tabular, same 11 fields | 11 | 1524 | 460 | **-41%** |
| JSON array, trimmed to 4 fields | 4 | 1573 | 448 | **-43%** |
| TOON tabular, trimmed to 4 fields | 4 | 1198 | 353 | **-55%** |
| `5w ready` compact (today's actual default) | ~4-6 + aggregate | 1006 | 293 | **-63%** |

(Token counts via `tiktoken`'s `cl100k_base`, a GPT tokenizer, not Claude's own — treat the
percentages as indicative, not exact for every model. Script and raw data are in this session's
scratch dir, not committed, since they're not needed to reproduce: the queue and the code that
prints it are.)

The headline: **TOON's ~41-43% saving over JSON matches its own published number (42.6%) on real
5w data** — but 5w's *existing, shipped, zero-cost* compact format already beats every TOON
variant measured, including the one with fewer fields than TOON's own tabular header would carry
by default. Roughly half of TOON's advantage over 5w's current `--json` is the format (braces →
delimiters); the other half is simply carrying fewer fields — and 5w can capture that half today,
in plain JSON, via the `--full` split proposed in principle #2, with no new format.

### Why a `--toon` writer would cost more than it looks

1. **The schema doesn't tabularize cleanly.** TOON's tabular form requires "every column is
   uniform-primitive or nested-uniform" (spec §9.3) — nested-uniform means nested *objects*, not
   arrays. 5w's task schema has three array-valued fields (`needs`, `unmet`, `body`). The moment
   any row has a non-empty `needs`, a faithful full-fidelity TOON encoding of `Q::json()`'s
   current 11 fields can't stay in the compact tabular form for that column; the honest encoding
   falls back to TOON's bulkier list form (`- key: ...` per object), which gives back most of the
   saving the format exists for. In practice a `--toon` writer would need to either (a) drop
   array fields from the default row — which is just principle #2's field-trim again, now paying
   a new-format tax for it — or (b) implement both tabular and list form plus the logic to detect
   which one a given batch of rows needs, which is real code, not a formatting tweak.
2. **No crates, and the spec has real edge cases.** CLAUDE.md is explicit: "No dependencies... a
   crate is not the fix for a missing feature." A conformant writer needs: the quoting predicate
   (§7.2's numeric-like regex, reserved words, leading `-`/`#`, delimiter-in-content), the escape
   table (§7.1: `\`, `"`, control chars → `\uXXXX`), delimiter selection and the `[N<delim>]`
   header spelling for tab/pipe, and — because tabular form's header must be committed before any
   row prints — buffering all rows before emitting anything, a structural change from `Q::list()`
   today, which prints one line per task as it iterates (`src/tasks.rs` ~line 310). None of this
   is exotic, but it's a genuine chunk of hand-written, spec-tracking code (versioned at v4.1.1,
   with its own conformance suite this repo would have no way to run against a hand-rolled
   writer) for a decoder-side benefit — 5w never needs to *parse* TOON, only emit it, which caps
   the cost but doesn't remove it.
3. **JSON's real advantage is that every consumer already speaks it.** TOON is newer and less
   universally known to models than JSON; an agent given `--toon` output has to either already
   know the format or be taught it in-context (spending back some of the saved tokens). 5w's
   `--json` is consumed by scripts and agents that already have a JSON parser; a second output
   format is a second thing to keep in sync with the schema (`Q::json()` already has one hand-
   written serializer to maintain correctly — see the escaping in `js()` — a second one doubles
   that surface for a format whose ceiling, on this data, is *worse* than what `5w ready` already
   does for free).

### Recommendation

**Reject** a `--toon` (or TOON-by-default) output format for now. It would save tokens over
5w's `--json`, roughly matching TOON's own claimed number — but 5w's existing compact default
already beats it on the numbers above, `--json` is the deliberately full-fidelity/parseable
channel where TOON's array-column limitation bites hardest, and the dependency-free
implementation cost (quoting/escaping to spec, buffered tabular emission, a second serializer to
maintain) is real for a format that isn't yet a safe bet for every consuming agent to already
understand.

**Adopt** the cheap part of the same idea instead: trim `--json`'s default field set (principle
#2 above). That alone captures about half of TOON's measured advantage over today's `--json`,
using code 5w already has (`--full`), with no new format, no spec to track, and no new
serializer.

**Revisit if**: `--json` payloads grow past what the compact default can substitute for — e.g. a
future bulk export/ingest path, or the README's own precedent of a 900-task queue (649 KB →
52 KB via `5w archive`) recurring for `--json` rather than the archived Markdown. At that size,
TOON's array-column limitation matters less (most consumers there want tabular rows, not deep
task bodies) and the token savings compound over a much bigger payload.

## Proposed follow-up tasks (not added to the queue — for the reviewer)

- `--json list rows: trim to id/state/level/area/title/branch by default, --full for the rest @output !2`
- `opts(): refuse a flag a command doesn't understand instead of silently accepting it @output !2`
- `row(): append a size hint when a title is actually truncated, not just a bare … @output !1`
- `per-subcommand --help: print the one-line usage instead of the full global listing @output !1`
- `ready/review: append a → 5w delegate|accept next-step hint to the summary line, like next already does @output !1`

## Sources

- AXI: https://axi.md (10 principles, fetched in full)
- TOON: https://toonformat.dev and https://github.com/toon-format/spec/blob/main/SPEC.md
  (v4.1.1/v4.1 badge; encoding rules §2, §6-§9, §11, §14.1; quoting §7.2-7.3)

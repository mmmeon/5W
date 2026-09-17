# AXI, TOON and clig.dev, evaluated against 5w's output

Task #12 (AXI, TOON) and task #13 (clig.dev), stacked. No `docs/` convention exists in this
repository yet (README.md is the only prose doc, per CLAUDE.md: "README.md is the user's view").
This evaluation is reasoning about a hypothetical change, not a description of a shipped one, so
it does not belong in README.md's "Keeping context small" section, which describes what 5w does
today. It lives here instead; a reviewer who adopts any of the proposed changes should fold the
relevant bit of this file's reasoning into README.md at that point, per CLAUDE.md's "reasoning in
the README rather than in error text."

Commands were run from this worktree, off a terminal (`std::io::stdout().is_terminal()` is false
under the tool that ran them, so `5w`'s own compact-by-default rule was already in effect without
`FIVEW_AGENT` set). `5w --version` was 0.1.3 at the time. Task #13's own commands were re-run
against the same binary, additionally probing an interactive terminal via `script`, `NO_COLOR`,
`--no-color`, unknown flags, stdin `-`, and SIGPIPE — cited inline with their actual output.

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
5w already has (`--full` already exists); it does not need a new format. **Done (#22):** list
rows (`ready`, `ls`, `blocked`, `all`, and `review`, which keeps its `tip`/`moved`/`diff`/`behind`)
carry those six plus `unmet` and `rework`, what the text row shows as "blocked by" and "sent back";
`--full` restores `lane`, `kind` and `needs`. `show` and `next` print one task and stay full.

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
`review` at `5w accept <id>` / `5w reject <id> <reason>` for its top row. **Done (#26):** `ready`'s summary line ends with
`→ 5w delegate <id>`; `review` ends its rows with `→ 5w accept <id>`.

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

## Command Line Interface Guidelines — https://clig.dev, github.com/cli-guidelines/cli-guidelines

clig.dev's foreword names its own turn explicitly: "the command line of the past was
*machine-first*... today's command line is *human-first*," and its Philosophy section builds
everything — prompts, confirmations, "conversation as the norm," help text you're meant to read —
on that premise. 5w's premise is the opposite one, stated in CLAUDE.md: "output is read by
agents: compact off a terminal." That's not a rejection of clig so much as a different answer to
the same question clig itself poses (who is this for), so most of the guide still applies — it
just applies to the *terminal* case, which 5w treats as the secondary one. Where clig's
human-first framing and 5w's agent-first framing pull in different directions on a specific
guideline, it's called out below; everywhere else, "already met" or "adopt" holds for both
readings.

### The Basics

**Argument parsing library — reject, a hard constraint, not a judgment call.** CLAUDE.md: "No
dependencies... a crate is not the fix for a missing feature." 5w hand-writes `opts()`
(`src/tasks.rs` ~line 66) instead of using `clap` or similar. This is exactly why the unknown-flag
bug exists (`opts()` accepts and silently drops any `--flag` it doesn't recognize — see #12's
principle 6, and that section's follow-up task, which is the actual fix here; not re-listed).
Adopting a crate would fix it faster, but the constraint is non-negotiable, so the fix is to the
hand-written parser, not a dependency.

**Zero exit on success, non-zero on failure — already met.** `main.rs`: `std::process::exit(1)`
on any `Err`, falls through to 0 otherwise. Confirmed: `5w show 9999` exits 1, `5w doctor` on a
clean queue exits 0.

**stdout for output, stderr for messaging — already met.** `main.rs`: `eprintln!("5w: {e}");`.
Confirmed by redirecting streams separately for `5w show 9999`: stdout was empty, stderr held
`5w: no task #9999`. (This is also AXI principle 6's "errors on stdout" claim, rejected there for
the same reason.)

### Help

**Extensive help on `-h`/`--help` — met at the top level, gapped per-subcommand.** Same gap #12's
principle 10 already covers (`wt`/`ship`/`lint`/`ci`/`report` have it, the rest fall back to the
full global `USAGE`) — see that section and its follow-up, not re-listed here. Confirmed again
this session: `5w add --help`, `5w submit --help`, `5w accept --help`, `5w reject --help` all print
the same ~40-line global listing, not a line about the subcommand.

**Concise help by default — already met, by a different route than clig describes.** clig's
default is "show a short help blurb when args are missing"; 5w's default (AXI principle 8,
already in this file) is stronger — bare `5w` shows live queue data, not help text at all. This
satisfies clig's own carve-out ("ignore this guideline if your program is interactive/does
something by default") in spirit: an agent's first call already gets a state, not a menu.

**Lead with examples — adopt narrowly; reject in full, an agent-vs-human split.** `tasks::USAGE`
has zero example invocations, only a command list — a real gap for a first-time human reader.
But clig's fuller ask (a short story of chained examples building toward complex use) would cost
tokens on every `--help` call for no benefit to the audience CLAUDE.md optimizes for: `5w delegate
<id>` already hands a worker task-specific, ready-to-run commands (AXI principle 7, already in
this file) — a generic example in global help is redundant with that, for an agent. **Adopt**: one
line, e.g. `example: 5w ready @output` under the usage line. **Reject** a longer worked sequence.
**Done (#34):** the full listing's second line is `example  5w ready level:1`; a command's
`--help` does not repeat it.

**Support path for feedback — already met.** `report <what happened>` saves locally, and
`report send --gh` pre-populates a GitHub issue URL against `mmmeon/5W` (or `$FIVEW_ISSUES`) —
`src/report.rs` lines 29, 32, 275, 283. This is clig's "make it effortless to submit bug reports...
pre-populate as much as possible" almost verbatim, and it's paired with `install_panic_hook()`
(`src/report.rs` lines 102-113), which on a panic records the crash to `last-failure.md` and tells
the user the exact command to run — clig's "provide debug/traceback info and instructions to file
a bug," also already met.

**Link to web docs in help text — adopt.** `tasks::USAGE` never prints a URL. README.md is the
web-readable doc (per CLAUDE.md: "README.md is the user's view") but nothing in `--help` points at
it. Cheap, one line. **Done (#29):** the full listing ends with `docs <repository>#readme`, taken from
`Cargo.toml`'s `repository`; per-command help leaves it out.

**Suggest corrections when the user got it wrong — reject, a stronger fix already exists.**
`validate_field()` (`src/tasks.rs` line 933) doesn't guess at a typo; it states the grammar
directly: `not a field: "xyz" (want @area !n >lane needs:#a,#b branch:x)`. Implementing
fuzzy-match "did you mean" suggestions would be exactly the hand-rolled complexity CLAUDE.md warns
against, for a case the existing refusal already resolves more directly — the fix is named, not
guessed at, which is a stricter reading of clig's own goal (get the user to the right answer) than
clig's own mechanism (guess and ask).

**Display help immediately if stdin is a TTY and input was expected — not applicable.** 5w has no
command that defaults to reading stdin; the only stdin consumer is `add --body -`
(`src/tasks.rs` lines 961-966), gated behind an explicit flag the caller chose. There's no `cat`-
style hang risk to guard against.

### Documentation

**Web-based docs — already met.** README.md and PROTOCOL.md, on GitHub.

**Terminal-based docs — already met.** `5w --help`, `5w doctor`, and especially `5w delegate <id>`,
which assembles `refs:`/`docs:`/`conventions:`/`tier:` for the task at hand (AXI principle 7).

**man pages — reject.** Another documentation artifact to keep in step with the binary, for a tool
that already has "keep the two protocols in step" (CLAUDE.md) as a live source of drift risk. Not
worth it for a single small binary with a `--help`.

### Output

Most of this section is AXI's principles 2-5 and 9, already covered above and not repeated. New
ground clig covers that AXI didn't:

**Color: disable on non-TTY, `NO_COLOR`, `TERM=dumb`, `--no-color` — partially met.**
`Sty::new()` (`src/util.rs` lines 13-16) checks `stdout().is_terminal()` and `NO_COLOR` — both
confirmed this session: piping through `script` (a real pty) shows ANSI codes
(`^[[1m!1^[[0m`); the same command with `NO_COLOR=1` set shows none. **Not met:** `TERM=dumb` is
never checked (`grep -n '"dumb"' src/*.rs` — nothing), and there's no `--no-color` flag — passing
`5w ready --no-color` runs but does nothing, silently absorbed by the same unknown-flag bug #12
already flagged (so even after that fix lands, `--no-color` would need to actually reach `Sty`,
which it doesn't yet). **Adopt** both, cheaply: one more `||` clause in `Sty::new()`, and a flag
read in `opts()`.

**No animations off-terminal — already met, trivially.** 5w has no progress bars or spinners at
all yet (no long-running operation needs one today — everything is local git). Worth remembering
if #9 (self-update, a network fetch) lands: that's the first place this guideline and the
"responsive <100ms" / timeout guidance below would actually bind.

**Symbols/emoji for clarity — reject, an agent-vs-human split.** 5w's compact rows already use
dense ASCII sigils (`#`, `!`, `@`, `>`, `+`, `→`) instead of prose or emoji. Adding emoji per
clig's "yubikey-agent" example would cost bytes/tokens per row for scannability an agent doesn't
need and a human reader can already get from the sigils once they're documented (see #12's
principle 3 adopt, re the undocumented `+` mark). Keep the sigils, skip the emoji.

**Don't leak developer-only output; don't treat stderr like a log file — already met.** Refusals
are one line, `5w: <message>`, no `ERR`/`WARN` labels, no stack traces by default (a panic gets a
one-line pointer to `5w report`, not a raw backtrace on stderr).

**Use a pager for long output — reject.** Piping `5w`'s own output through `less` assumes an
interactive human; it would intercept stdout an agent expects to read directly, and 5w's
compact-by-default output is already short in the case that matters most (off-terminal). Not worth
the added complexity clig itself warns is "error-prone."

**If you change state, tell the user; make state visible — already met.** Every mutation prints
its result: `"  #{id} accepted{}"` and `"  #{id} submitted — {branch} at {sha}"`
(`src/tasks.rs` lines 1322-1325, 1163), `"  added #{}"` on `add`. `5w show`/`5w review` make the
current state (including drift since submit) visible on request.

### Errors

**Catch and rewrite errors for humans — this is the sharpest clig-vs-CLAUDE.md conflict in the
whole document, and it's resolved toward CLAUDE.md.** clig's own example is conversational and
multi-sentence: *"Can't write to file.txt. You might need to make it writable by running `chmod
+w file.txt`."* CLAUDE.md is explicit and opposite in register: "refusals one line naming the
fix." 5w's refusals already name the fix (`not a field: ... (want @area !n >lane ...)`, doctor's
`` `5w split` `` hint) but as a single terse clause, never a sentence with a subject and an
apology. **Reject** clig's prose register; 5w's existing one-liners already satisfy the underlying
goal (get the user/agent to the fix) at a fraction of the token cost — keep the current style.

**Group repeated similar errors under one header — not applicable today.** 5w reports exactly one
failure per invocation (one `Result<(), String>` per command); there's no batch path yet that could
produce a wall of similar errors. Revisit only if a bulk command is ever added.

**Unexpected errors get debug info and a bug-report path — already met** (see Help, above:
`install_panic_hook()`).

**Red for errors, used intentionally — adopt, low value.** `Sty::red()` exists and is used for one
thing today — a drift/rework note in `row()` (`src/tasks.rs` line 257) — but the top-level refusal
line in `main.rs` (`eprintln!("5w: {e}")`) never touches `Sty` at all, so it's never colored even
on an interactive terminal with color otherwise on. Cheap to add, purely cosmetic, no effect on
the off-terminal/agent path (stderr colored only when stderr itself is a TTY and `NO_COLOR` unset).

### Arguments and flags

**Prefer flags to args — reject, an agent-vs-human split, intentional.** 5w leans positional and
uses terse sigils instead of flags for the most common filters (`@area`, `!level`, `>lane` rather
than `--area=x --level=n --lane=x`), and its mutating commands are `<verb> <id> [more]`
(`5w accept 12`, `5w reject 12 <reason>`) rather than flag-qualified. This is clig's own "common,
primary action, brevity is worth memorizing" exception (its `cp <source> <destination>` example),
applied more broadly than clig would by default — deliberately, since every extra `--flag=` is
tokens an agent pays on every invocation of what is, for 5w, the primary action of nearly every
subcommand. **Reject** the letter of "prefer flags" here; keep the sigils and positional ids.

**Full-length flag versions; one-letter flags reserved for common ones — already met.** `-h`
pairs with `--help` (and only that); every other flag is spelled out (`--json`, `--force`,
`--full`, `--ids`, `--limit`, `--body`, `--at`, `--staged`, `--sync`, `--squash`,
`--discard-ignored`, `--pin`, `--checklist`, `--gh`, `--print`, `--expected`, `--no-last`) — no
single-letter flag exists besides `-h` and `-V`, so there's no short-flag namespace to pollute.

**Multiple args for a simple multi-file action — not applicable, by design.** 5w's mutations are
explicitly one task at a time: "each commits itself to the trunk, and only itself"
(`tasks::USAGE`). Batch args would fight that invariant (PROTOCOL.md's one-commit-per-transition
gate), not just be extra parsing work.

**Standard flag names — already met** for the ones 5w has (`--json`, `--force`, `-h`/`--help`).
No `-q`/`--quiet`: not adopted, since compact-by-default already suppresses the noise `-q` exists
to hide for the audience that matters most (agents); a human on a terminal can still redirect.

**Make the default right for most users — already met**, and it's 5w's central design choice:
`compact_default()` (`src/tasks.rs` lines 101-108) picks the terse format unless a human is
actually at a terminal.

**Never require a prompt; support `-` for stdin — already met.** 5w has no interactive prompts
anywhere (confirmed: no `stdin().read_line` in the codebase outside `report.rs`'s own
who-am-I detection); every input is a flag or arg. `add --body -` reads stdin explicitly when
asked (`src/tasks.rs` lines 961-966). This is clig's *minimum* bar for the section ("never
require") met by 5w's actual default (never prompt at all) — not a gap, a stronger position.

**Confirm before anything dangerous — already met via flags, no interactive path needed.**
`accept --force`, `ship --force`, `wt rm --force` gate the moderate/severe cases; since 5w never
prompts (above), there's no interactive confirmation to also implement — the flag-gate is the
whole mechanism, consistent with the rest of the section.

**Order-independent flags/subcommands — not the anti-pattern clig describes.** `5w --json ready`
is refused (`unknown command: --json`) — but 5w doesn't have flags that work in one position and
silently fail in another (clig's actual complaint); it has no true global flags at all, only
pseudo-commands (`--help`, `--version`) that stand alone. Flags *after* the subcommand are
consistently order-independent among themselves (confirmed for `ready`'s filters and output
flags). **Reject** — no fix needed, this isn't the inconsistency being warned about.

**Do not read secrets from flags — not applicable.** No flag in 5w accepts a secret; `report send
--gh` shells out to `gh`, which owns its own credentials.

### Interactivity

**Reject, in full — this is where clig's "conversation as the norm" philosophy and CLAUDE.md's
agent-first design diverge most broadly, and the file already documents the resolution (AXI
principle 7).** clig's Interactivity section (only-prompt-on-a-TTY, `--no-input`, hidden password
input, Ctrl-C escape from a wrapped program) assumes a human iterating through trial and error.
5w has zero prompts of any kind (see Arguments and flags, above), so the section is moot rather
than failed: `--no-input` has nothing to disable, there's no password prompt to hide, and 5w never
wraps another interactive program (no SSH/tmux-style embedding). The one item that does apply —
"let the user escape," i.e. Ctrl-C works — is covered under Signals, below, and is met.

### Subcommands

**Consistent across subcommands — mostly met, one small inconsistency found.** `--json`/`--force`/
`--full` behave the same wherever they appear, parsed by the shared `opts()` scanner. Exception:
`wt rm <branch> [--force]` checks `rest.get(1) == "--force"` positionally (`src/wt.rs` line 55)
rather than scanning args the way `tasks::opts()` does, so `5w wt rm --force <branch>` doesn't work
the way `5w ready --limit 3 --json` (any order) does. Minor; **adopt**, low priority.

**Noun-verb consistency across subcommand groups — already met.** `wt <new|add|ls|path|rm|link|
install|setup>` and `report <list|show|send|rm>` are both noun-first with their own verb sets,
styled identically in `USAGE`.

**No ambiguous or similarly-named commands — already met**, checked against the full command
list (`add set submit accept reject done open archive split ready next ls blocked levels all show
delegate review doctor` plus `wt ship lint ci report hook update-files init`) — no near-duplicate
pairs like clig's "update vs. upgrade" example.

### Robustness

**Validate user input — already met.** `validate_field()`, `parse_id()`, `--limit`'s parse-or-bail,
whitespace/control-char rejection on fields (`src/tasks.rs` lines 902-936).

**Responsive <100ms; show progress on long operations — already met, trivially, today.** Every
current operation is local git; nothing takes long enough to need a spinner. This guideline (and
the timeout guidance below it in clig) is the one to revisit when #9 (self-update, a network
fetch) is implemented — not a new follow-up here, just a forward pointer to an already-queued task.

**Recoverable after a transient failure — already met, by the same mechanism AXI's "idempotent
mutations" section (above) already discusses from the other side.** Every mutation is one atomic
`git commit`; a failed or interrupted command simply hasn't committed, so re-running it is safe.
Re-running it *after* success is deliberately refused (not silently idempotent) — that's the
AXI-section tradeoff, not a Robustness-section gap.

**Crash-only — already met, and `ship.rs` goes a step further than "no cleanup needed."**
`ship`'s squash builds the new commit object first (`git commit-tree`, which touches no ref) and
only moves a ref once, via `merge --ff-only`, with the comment "until it succeeds nothing has
changed" (`src/ship.rs` lines 262-263) — build-then-swap, exactly clig's crash-only pattern. Where
`ship --sync` does need to undo something (a rebase that turned out not to be reviewable), it does
so explicitly: `git reset --hard` back to the pre-rebase sha, with the refusal saying so
(`src/ship.rs` lines 205-211) — a guarded rollback rather than a bare crash, which is stronger than
the guideline asks for.

### Future-proofing

**Warn before a breaking change — not applicable yet.** 5w is pre-1.0 and hasn't deprecated
anything user-facing. The closest existing analog is `doctor`'s version-drift notes (`requires =
"0.1.2"; this is 0.1.3 — ...`), which already serve the same "tell them before it bites them"
function for the queue/tool version mismatch it can detect.

**Don't create a time bomb — already met.** 5w's core (queue read/write) makes zero network calls;
the only thing that reaches the network is the opt-in `report send --gh` against a configurable
repo. The tool works exactly the same in 20 years as today for its main job, regardless of
GitHub's fate.

**No catch-all subcommand; no arbitrary abbreviations — already met.** Confirmed: `5w rea` and
`5w r` both refuse (`unknown command: rea`), not treated as `ready`. Bare `5w` (no args at all)
defaulting to `ready` is a different thing — AXI principle 8's content-first default, not a
catch-all that swallows unrecognized *subcommand names*.

### Signals and control characters

**Ctrl-C exits immediately — already met, and for a stronger reason than "handled."** 5w never
installs a `SIGINT` handler; Rust's default disposition (terminate immediately) is already correct
here, because — per Robustness, above — no mutation has a cleanup phase to interrupt. `SIGPIPE` *is*
handled explicitly, and for the opposite reason: `main.rs` lines 20-31 reset it to `SIG_DFL`
because Rust ignores it by default, which would otherwise turn `5w ready | head` into a panic
instead of a quiet exit. Confirmed this session: `5w all | head -1` exits 0, no panic, one line of
output. "Let the user escape a wrapped program" doesn't apply — 5w never wraps another interactive
program.

### Configuration

**Category-appropriate storage, precedence order — already met.** `.5w.toml` is 5w's "stable
within a project, for all users" config (clig's category 3), version-controlled, exactly matching
clig's own recommendation for that category. `FIVEW_AGENT`/`FIVEW_WT_ROOT`/`FIVEW_TRUNK` are
category-1/2 (varies per invocation or per machine), and precedence is right: a flag beats the
env var where both exist (`--full` overrides `FIVEW_AGENT`'s compact default in `opts()`).

**XDG base directory spec — not applicable.** 5w has no user-level (`~/.config`) config file at
all; everything is either a flag, a `FIVEW_*` env var, or the project's own `.5w.toml`. The
guideline binds only where a user-level config file exists.

**Ask consent before modifying config that isn't yours — already met, trivially.** The only files
5w writes outside `TASKS.md`/the archive are its own: `.5w.toml`, `PROTOCOL.md`, hooks it installed
itself. It never touches an unrelated file like a shell rc or crontab.

### Environment variables

**Naming, single-line values, no collisions — already met.** `FIVEW_AGENT`, `FIVEW_ISSUES`,
`FIVEW_WT_ROOT`, `FIVEW_TRUNK` — all uppercase, underscored, project-prefixed, single-line string
or boolean-presence values.

**Check general-purpose env vars where relevant — partially met.** `NO_COLOR` and `HOME` are read
(`src/util.rs` line 15, `src/wt.rs` line 189). `DEBUG`, `EDITOR`, `PAGER`, `TMPDIR`, `LINES`/
`COLUMNS` are not — **not applicable** for each: no verbose mode to gate behind `DEBUG` yet, no
file-editing prompt to hand to `$EDITOR`, no pager (rejected above), no temp files, no
screen-width-dependent tables.

**`.env` — not applicable.** 5w's project-level config is TOML (`.5w.toml`), not `.env`; nothing
here needs `.env`'s looser, stringly-typed model.

**Do not read secrets from env vars — already met, trivially.** 5w never reads a secret from
anywhere (see Arguments and flags, above).

### Naming

**Already met.** `5w` — lowercase, short without being a taken common-utility name (`cd`/`ls`/
`ps`), typeable with one hand, matching CLAUDE.md's own description: "one static binary `5w`."

### Distribution

**Single static binary — already met**, and it's a repository-level commitment, not just a
default: CLAUDE.md's Releasing section describes a reproducible build, a signed tag whose message
is the `SHA256SUMS`, and a release workflow that only publishes on a sums match.

**Easy to uninstall — adopt, minor.** README.md documents `cargo install --path .` (line 16) but
no corresponding uninstall line. Cheap to add next to it. **Done (#31):** an uninstall note follows the install block.

### Analytics

**Do not phone home without consent — already met, and stated in the tool's own usage text.**
`tasks::USAGE` line 40: `` report <what happened>  save a report locally (last failure attached);
nothing is sent ``. Nothing reaches the network until the user explicitly runs `report send`
(`--gh`, `--print`, or a browser — README.md lines 321-322). There is no background or default-on
telemetry anywhere in the codebase (confirmed: the only network-reaching code path is `report`'s
explicit send).

## Proposed follow-up tasks (not added to the queue — for the reviewer)

- `--json list rows: trim to id/state/level/area/title/branch by default, --full for the rest @output !2` — done, #22
- `opts(): refuse a flag a command doesn't understand instead of silently accepting it @output !2` — done, #23
- `row(): append a size hint when a title is actually truncated, not just a bare … @output !1` — done, #24
- `per-subcommand --help: print the one-line usage instead of the full global listing @output !1` — done, #25
- `ready/review: append a → 5w delegate|accept next-step hint to the summary line, like next already does @output !1` — done, #26
- `Sty::new(): also disable color for TERM=dumb, and wire up an explicit --no-color flag (currently swallowed by the opts() unknown-flag bug) @output !1` — done, #27
- `main.rs: color the "5w: {e}" refusal red when stderr is a terminal and NO_COLOR is unset @output !1`
- `tasks::USAGE: add one line pointing at the web docs (README.md's URL), per clig's "link to web docs in help text" @output !1` — done, #29
- `wt rm: parse --force by scanning args like tasks::opts() does, instead of a positional rest.get(1) check @output !1`
- `README: add a one-line uninstall note (cargo uninstall 5w) near the install instructions @output !1` — done, #31

## Sources

- AXI: https://axi.md (10 principles, fetched in full)
- TOON: https://toonformat.dev and https://github.com/toon-format/spec/blob/main/SPEC.md
  (v4.1.1/v4.1 badge; encoding rules §2, §6-§9, §11, §14.1; quoting §7.2-7.3)
- CLI Guidelines: https://clig.dev and https://github.com/cli-guidelines/cli-guidelines/blob/main/content/_index.md
  (fetched in full — Philosophy through Analytics, plus Further reading)

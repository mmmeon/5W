# Tasks

The queue. One task per line, read and written by `5w`:

```
- [ ] #<id> <what to do>  @<area> !<1-4> ><lane>  needs:#<id>  branch:<area>/<what>
  An optional body: every line indented two spaces under a task belongs to it.
```

`[ ]` open · `[~]` submitted, waiting on review · `[x]` accepted or closed, with `via:` saying how.

Ids are permanent — never renumber, never reuse. Edit this file through `5w`; without it, follow
[PROTOCOL.md](PROTOCOL.md), which `5w lint` and the pre-commit hook check.

## Open

- [ ] #1 Attach SHA256SUMS.asc to v0.1.2: dist/SHA256SUMS.asc is signed, the gh token lacks Contents write @release !1 >restricted
- [ ] #2 Create the report label on mmmeon/5W so 5w report send can apply it @report !1 >restricted
- [ ] #3 Release 0.1.3: queue commits and squashes signed when the repository signs @release !1 >restricted
- [~] #4 CI for 5W itself: cargo fmt, clippy and test on every push and change request, and the 5w ci check @ci !2 branch:ci/task-4 submitted:cb34177c53d1
- [~] #5 Split titles cut at a word boundary end in a misleading …: the rest is in the body, not lost @queue !1 branch:queue/task-5 submitted:63eb9743ad45
- [~] #6 Normalize worktree paths: wt new prints /home/who/r/ara/../ara-wt/… @wt !1 branch:wt/task-6 submitted:db3e29446261
- [~] #7 Test the panic hook: a crash records last-failure.md and says how to report it @report !2 branch:report/task-7 submitted:cf6e3d7307ea
- [ ] #8 Run the aarch64 release binary on real arm64 hardware @release !1 >manual
- [ ] #9 self-update and version --latest: opt-in, the pinned version by default, checksums and signature verified @upkeep !2
- [ ] #10 Forge events as queue transitions: a change request submits, an approving review accepts, from a CI job @ci !3
- [ ] #11 Gate code pushed straight to the trunk: record the landed range at ship so pre-receive can match it to a review @ci !4
- [~] #12 Evaluate AXI (axi.md) and TOON (toonformat.dev) for 5w output read by agents @output !2 branch:output/task-12 submitted:486262689601
  AXI: 10 design principles for agent-ergonomic CLIs — token budget first, minimal fields per list item, aggregates inline, structured errors, help[] next-step hints, combined operations.
  TOON: compact indentation-based encoding of the JSON data model for LLM prompts (~40% fewer tokens than JSON; spec v4.1.1, conformance suite).
  Compare against what 5w already does (compact off a terminal, one-line refusals naming the fix, --json/--ids). Outcome: a written verdict per principle — adopt, already met, or reject with reason — and follow-up tasks for what is adopted. A TOON writer, if any, is hand-written: no dependencies.
- [~] #13 Evaluate the Command Line Interface Guidelines (clig.dev) for 5w, alongside the AXI/TOON verdict @output !2 branch:output/task-13 submitted:2eae6317c418
  Source: https://github.com/cli-guidelines/cli-guidelines (published at clig.dev). Extend docs/agent-output.md from #12 (branch output/task-12; stack on it) with a section per guideline area: already met, adopt, or reject with reason — noting where clig (human-first) and AXI (agent-first) conflict and which 5w follows. Proposed follow-ups listed in the file, not queued.
- [ ] #14 Benchmark the cost of 5w output: bytes, estimated tokens and time per command on generated queues @bench !3
  Deterministic, no agents, no network, no dependencies. Generate scratch queues of 10, 100 and 1000 tasks (with bodies, needs, archive) and run each read command (ready, next, ls, all, show, delegate, review, doctor, lint) off a terminal, with and without --json, plus common refusals.
  Record per command: stdout+stderr bytes, estimated tokens (a documented heuristic; an optional external tokenizer command via env for exact counts), wall time.
  Commit a baseline; a check compares against it with a tolerance and fails on regressions, so output growth is a deliberate choice. Runnable locally and from CI (#4). Numbers back the output follow-ups from #12/#13.

## Done

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
- [~] #14 Benchmark the cost of 5w output: bytes, estimated tokens and time per command on generated queues @bench !3 branch:bench/task-14 submitted:fb66c3b6e86b
  Deterministic, no agents, no network, no dependencies. Generate scratch queues of 10, 100 and 1000 tasks (with bodies, needs, archive) and run each read command (ready, next, ls, all, show, delegate, review, doctor, lint) off a terminal, with and without --json, plus common refusals.
  Record per command: stdout+stderr bytes, estimated tokens (a documented heuristic; an optional external tokenizer command via env for exact counts), wall time.
  Commit a baseline; a check compares against it with a tolerance and fails on regressions, so output growth is a deliberate choice. Runnable locally and from CI (#4). Numbers back the output follow-ups from #12/#13.
- [~] #15 audit: a report on how a repository uses 5W, from its queue history and 5w records @audit !3 branch:audit/task-15 submitted:45992570a11d
  Tool-neutral, read-only, works on any 5W repository (e.g. ara). Sources only what 5W already has: TASKS.md and archive history in git, .git/5w/last-failure.md and saved reports, doctor and lint findings.
  Report: tasks by lane/level/area; submit→accept time; reject and rework cycles with reasons; tasks reopened; blocked time on needs; branches shipped without a task (when require_task is off); queue edits made outside 5w (lint over history); recorded failures and refusals; context cost of delegate briefs (bytes/estimated tokens, largest first — same estimator as the benchmark).
  Compact by default, --json, --since <rev|date>. Refusal-free on a repo with no history. Design the report sections in the README before building.
- [ ] #16 Move git calls behind named operations in src/git.rs: no raw git argv outside it, output unchanged @vcs !3
  About 50 call sites in 10 modules pass raw git arguments (ship 12, wt 11, lint 5, queue 4, store 3). Move each into a named function in src/git.rs. No behaviour change: tests/flow.rs and the #14 bench baseline prove it. Groundwork for the admin commands and a version-control shim.
- [ ] #17 wt prune: remove worktrees and branches with no commits past their parent, no changes, and no task naming them @wt !2
  Lists what it would remove; --yes removes. Found in the 2026-09-16 supervisor round: test/branch, test/check-normalization, test/check-without-fix, test/normalize-paths needed raw git worktree remove and git branch -D. wt rm keeps the branch. Admin chores belong in 5w, not raw git.
- [ ] #18 doctor: name a trunk checkout holding an exact copy of a branch's diff @doctor !1
  with a command that restores those files only on an exact match
  Found 2026-09-16: main held uncommitted src/wt.rs and tests/flow.rs identical to wt/task-6's diff, which blocks ship. Restoring must refuse unless the working changes equal the branch diff byte for byte.
- [ ] #19 Batch accept (accept 4 5 6) and ship --accepted: land every accepted branch bottom of stack first @queue !2
  stop at the first refusal
  A supervisor round of 8 accepted tasks meant 8 accepts and 8 ships with the stack order (#12 before #13, #14 before #15) worked out by hand. Each accept keeps its own checks; ship --accepted keeps every ship refusal.

## Done

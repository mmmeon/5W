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
- [ ] #6 Normalize worktree paths: wt new prints /home/who/r/ara/../ara-wt/… @wt !1
- [~] #7 Test the panic hook: a crash records last-failure.md and says how to report it @report !2 branch:report/task-7 submitted:cf6e3d7307ea
- [ ] #8 Run the aarch64 release binary on real arm64 hardware @release !1 >manual
- [ ] #9 self-update and version --latest: opt-in, the pinned version by default, checksums and signature verified @upkeep !2
- [ ] #10 Forge events as queue transitions: a change request submits, an approving review accepts, from a CI job @ci !3
- [ ] #11 Gate code pushed straight to the trunk: record the landed range at ship so pre-receive can match it to a review @ci !4
- [ ] #12 Evaluate AXI (axi.md) and TOON (toonformat.dev) for 5w output read by agents @output !2
  AXI: 10 design principles for agent-ergonomic CLIs — token budget first, minimal fields per list item, aggregates inline, structured errors, help[] next-step hints, combined operations.
  TOON: compact indentation-based encoding of the JSON data model for LLM prompts (~40% fewer tokens than JSON; spec v4.1.1, conformance suite).
  Compare against what 5w already does (compact off a terminal, one-line refusals naming the fix, --json/--ids). Outcome: a written verdict per principle — adopt, already met, or reject with reason — and follow-up tasks for what is adopted. A TOON writer, if any, is hand-written: no dependencies.

## Done

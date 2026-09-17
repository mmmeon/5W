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

- [ ] #2 Create the report label on mmmeon/5W so 5w report send can apply it @report !1 >restricted
- [ ] #3 Release 0.1.3: queue commits and squashes signed when the repository signs @release !1 >restricted
- [ ] #8 Run the aarch64 release binary on real arm64 hardware @release !1 >manual
- [ ] #10 Forge events as queue transitions: a change request submits, an approving review accepts, from a CI job @ci !3
- [ ] #11 Gate code pushed straight to the trunk: record the landed range at ship so pre-receive can match it to a review @ci !4
- [ ] #16 Move git calls behind named operations in src/git.rs: no raw git argv outside it, output unchanged @vcs !3
  About 50 call sites in 10 modules pass raw git arguments (ship 12, wt 11, lint 5, queue 4, store 3). Move each into a named function in src/git.rs. No behaviour change: tests/flow.rs and the #14 bench baseline prove it. Groundwork for the admin commands and a version-control shim.
- [ ] #21 Decide on a version-control shim over the git operations from #16 @vcs !4 >owner needs:#16
  Whether to put an interface over #16's named operations (trunk tip, queue commit + CAS ref update, worktree add/rm/ls, rebase onto, diff fingerprint, parent record, dirty, ref validation, signing), which system is second (jj the obvious candidate), and whether git-town's config keys stay the parent record.
- [ ] #36 Decide: close a task in the same commit that lands its change on the trunk @queue !3 >owner
  Owner wish (2026-09-16): in git history the closure and the merge should be one commit. Today it cannot be: PROTOCOL.md says a queue edit is its own commit touching only TASKS.md/DONE.md, and lint --staged, the pre-commit hook and pre-receive refuse a commit that mixes the queue with code; accept must also precede ship, which checks it. Options: (a) ship --squash folds the accept row into the landing commit, and the protocol allows exactly that shape (one row going [~] to [x] via:review with reviewed: equal to the squashed tree's source); (b) keep separate commits but have ship write the landing sha into the row (landed:<sha>) so history links them; (c) leave as is. (a) weakens the queue-commit-alone guarantee that keeps peer rows and code apart; (b) keeps it. Skipped for the current queue run; commits there stay separate.
- [ ] #50 Decide: a signing subkey used only for releases, separate from the commit-signing subkey @release !3 >owner
  Found reviewing #9: subkey 3491A839212CC7DB signs every commit and tag and SHA256SUMS. Any text an agent gets signed (a commit message) is a valid signature by the release key; #9 mitigates with strict SHA256SUMS parsing, but a release-only subkey (and self-update pinning it) removes the class. Needs key generation, SIGNING_KEY.asc update, ci/sign-release.sh and install-5w.sh changes.
- [ ] #51 Record full commit shas for submitted: and reviewed: @queue !2
  and clear inherited GIT_DIR/GIT_INDEX_FILE/GIT_OBJECT_DIRECTORY-style env in git::raw
  Found reviewing #19. (1) submitted:/reviewed: hold 12-hex short shas; a colliding prefix makes rev-parse ambiguous (refusal) and could stand in for a garbage-collected reviewed commit (~2^48 work). Record full shas going forward; keep reading short ones in existing rows; check lint and PROTOCOL.md's field description. (2) git::raw inherits GIT_DIR, GIT_WORK_TREE, GIT_INDEX_FILE, GIT_OBJECT_DIRECTORY, GIT_ALTERNATE_OBJECT_DIRECTORIES from the caller (or from git when a hook runs 5w); decide which 5w must clear so its checks read the repository it resolved, and test a hook-invoked 5w still works.
- [~] #54 lint: a single-edit queue subject must name the one row the commit changes @queue !1 rework:"the new no-op check swallows real edits: a set leaves a working-copy hand edit on disk while saying 'already so', and no longer moves a misplaced row into its section; no-op only when committed rows, the row's section and the working/staged copies are all unchanged" branch:queue/task-54 submitted:0b309f8ccd84
  Found reviewing #20: a hand commit 'chore(tasks): set #100 level 1' that also changes #4 passes lint and audit counts it as 5w-made. #20 checks batch subjects against changed rows; do the same for single-edit subjects (the id named equals the only changed row; archive moves and the tool's own multi-row commits, if any, must keep passing — check this repo's whole history lints). Also print a line when a batch is all no-ops ('nothing to commit').
- [~] #55 add mints ids from the working copy, so a later set of an uncommitted hand row commits an id lint calls reused @queue !1 branch:queue/task-55 submitted:8fa990600dd9
  Found reviewing #54: with an uncommitted hand row #50 in TASKS.md, 5w add mints #51 (reading the working copy); a later 5w set 50 ... pulls #50 into a commit and lint reports 'new row reuses id 50 (highest before was 51)' on a 5w-made commit. Decide: mint from the committed queue plus working-copy ids (never collide, never reuse), or refuse to pull an uncommitted row into a commit whose id is below the committed maximum; test it.

## Done

- [x] #1 Attach SHA256SUMS.asc to v0.1.2: dist/SHA256SUMS.asc is signed, the gh token lacks Contents write @release !1 >restricted via:self
- [x] #4 CI for 5W itself: cargo fmt, clippy and test on every push and change request, and the 5w ci check @ci !2 branch:ci/task-4 submitted:cb34177c53d1 via:review reviewed:cb34177c53d1
- [x] #5 Split titles cut at a word boundary end in a misleading …: the rest is in the body, not lost @queue !1 branch:queue/task-5 submitted:63eb9743ad45 via:review reviewed:63eb9743ad45
- [x] #6 Normalize worktree paths: wt new prints /home/who/r/ara/../ara-wt/… @wt !1 branch:wt/task-6 submitted:db3e29446261 via:review reviewed:db3e29446261
- [x] #7 Test the panic hook: a crash records last-failure.md and says how to report it @report !2 branch:report/task-7 submitted:cf6e3d7307ea via:review reviewed:cf6e3d7307ea
- [x] #12 Evaluate AXI (axi.md) and TOON (toonformat.dev) for 5w output read by agents @output !2 branch:output/task-12 submitted:486262689601 via:review reviewed:486262689601
  AXI: 10 design principles for agent-ergonomic CLIs — token budget first, minimal fields per list item, aggregates inline, structured errors, help[] next-step hints, combined operations.
  TOON: compact indentation-based encoding of the JSON data model for LLM prompts (~40% fewer tokens than JSON; spec v4.1.1, conformance suite).
  Compare against what 5w already does (compact off a terminal, one-line refusals naming the fix, --json/--ids). Outcome: a written verdict per principle — adopt, already met, or reject with reason — and follow-up tasks for what is adopted. A TOON writer, if any, is hand-written: no dependencies.
- [x] #14 Benchmark the cost of 5w output: bytes, estimated tokens and time per command on generated queues @bench !3 branch:bench/task-14 submitted:fb66c3b6e86b via:review reviewed:fb66c3b6e86b
  Deterministic, no agents, no network, no dependencies. Generate scratch queues of 10, 100 and 1000 tasks (with bodies, needs, archive) and run each read command (ready, next, ls, all, show, delegate, review, doctor, lint) off a terminal, with and without --json, plus common refusals.
  Record per command: stdout+stderr bytes, estimated tokens (a documented heuristic; an optional external tokenizer command via env for exact counts), wall time.
  Commit a baseline; a check compares against it with a tolerance and fails on regressions, so output growth is a deliberate choice. Runnable locally and from CI (#4). Numbers back the output follow-ups from #12/#13.
- [x] #13 Evaluate the Command Line Interface Guidelines (clig.dev) for 5w, alongside the AXI/TOON verdict @output !2 branch:output/task-13 submitted:1ba4fcdbfc4a via:review reviewed:1ba4fcdbfc4a
  Source: https://github.com/cli-guidelines/cli-guidelines (published at clig.dev). Extend docs/agent-output.md from #12 (branch output/task-12; stack on it) with a section per guideline area: already met, adopt, or reject with reason — noting where clig (human-first) and AXI (agent-first) conflict and which 5w follows. Proposed follow-ups listed in the file, not queued.
- [x] #15 audit: a report on how a repository uses 5W, from its queue history and 5w records @audit !3 branch:audit/task-15 submitted:28d2bbbdd136 via:review reviewed:28d2bbbdd136
  Tool-neutral, read-only, works on any 5W repository (e.g. ara). Sources only what 5W already has: TASKS.md and archive history in git, .git/5w/last-failure.md and saved reports, doctor and lint findings.
  Report: tasks by lane/level/area; submit→accept time; reject and rework cycles with reasons; tasks reopened; blocked time on needs; branches shipped without a task (when require_task is off); queue edits made outside 5w (lint over history); recorded failures and refusals; context cost of delegate briefs (bytes/estimated tokens, largest first — same estimator as the benchmark).
  Compact by default, --json, --since <rev|date>. Refusal-free on a repo with no history. Design the report sections in the README before building.
- [x] #30 wt rm: find --force anywhere in the arguments, not only at position 2 @wt !1 needs:#13 branch:wt/task-30 submitted:532ba21d60cd via:review reviewed:532ba21d60cd
  From the AXI/clig.dev evaluation in docs/agent-output.md (#12, #13); see that section for the reasoning. wt rm reads rest.get(1); scan args like tasks::opts() does.
- [x] #32 review --json prints the same text as review: give it JSON output or refuse the flag @output !1 branch:output/task-32 submitted:899619cb8312 via:review reviewed:899619cb8312
  Found by the #14 benchmark: review --json output is byte-identical to review at every queue size.
- [x] #33 wt: resolve .. in the worktree root once, so refusals and the --install line print clean paths too @wt !1 needs:#6 branch:wt/task-33 submitted:7eae06317748 via:review reviewed:7eae06317748
  #6 canonicalizes only the printed 'cd' line. The 'already exists' refusals in wt new/add and 'wt: <cmd> (in <dir>)' still show '..'. Normalize lexically in Repo::wt_root() instead; fs::canonicalize also follows symlinks (/tmp to /private/tmp on macOS).
- [x] #26 ready and review: end the summary line with a next-step hint (5w delegate / 5w accept), as next does @output !1 needs:#13 branch:output/task-26 submitted:86a8037ecb11 via:review reviewed:86a8037ecb11
  From the AXI/clig.dev evaluation in docs/agent-output.md (#12, #13); see that section for the reasoning. AXI principle 9.
- [x] #24 row(): append a size hint when a title is truncated, not a bare ellipsis @output !1 needs:#13 branch:output/task-24 submitted:c80a4613f6c4 via:review reviewed:c80a4613f6c4
  From the AXI/clig.dev evaluation in docs/agent-output.md (#12, #13); see that section for the reasoning. AXI principle 3.
- [x] #25 Per-subcommand --help prints that command's one-line usage, not the full global listing @output !1 needs:#13 branch:output/task-25 submitted:d32230be5716 via:review reviewed:d32230be5716
  From the AXI/clig.dev evaluation in docs/agent-output.md (#12, #13); see that section for the reasoning. add/submit/accept/reject --help print the ~40-line USAGE; wt/ship/lint/ci/report already have their own.
- [x] #38 reject accepts a task that was never submitted: PROTOCOL.md allows reject only from [~] @queue !1 branch:queue/task-38 submitted:cfd866abdd3e via:review reviewed:cfd866abdd3e
  Found reviewing #25: in a scratch repo, 5w reject 1 <reason> on an open [ ] task succeeded. PROTOCOL.md's transition table has Reject as [~] to [ ] only. Refuse in one line naming the state (e.g. '#1 is open, not submitted: nothing to reject'); check lint's transition rules agree; test it.
- [x] #18 doctor: name a trunk checkout holding an exact copy of a branch's diff @doctor !1 branch:doctor/task-18 submitted:1ac01d00671a via:review reviewed:1ac01d00671a
  with a command that restores those files only on an exact match
  Found 2026-09-16: main held uncommitted src/wt.rs and tests/flow.rs identical to wt/task-6's diff, which blocks ship. Restoring must refuse unless the working changes equal the branch diff byte for byte.
- [x] #40 lint: flag a row that gains rework: without going [~] to [ ] (hand-made reject of unsubmitted work) @queue !1 needs:#38 branch:queue/task-40 submitted:71719793b876 via:review reviewed:71719793b876
  Found reviewing #38: src/lint.rs flags a missing rework: only on [~] to [ ]. An open row that stays [ ] and gains rework:, or a closed row reopened with rework: added, passes lint and pre-receive though 5w reject refuses it after #38. Flag only no-rework to rework outside [~] to [ ], so fixing a typo in an existing reason still passes.
- [x] #17 wt prune: remove worktrees and branches with no commits past their parent, no changes, and no task naming them @wt !2 branch:wt/task-17 submitted:e828a5bdc848 via:review reviewed:e828a5bdc848
  Lists what it would remove; --yes removes. Found in the 2026-09-16 supervisor round: test/branch, test/check-normalization, test/check-without-fix, test/normalize-paths needed raw git worktree remove and git branch -D. wt rm keeps the branch. Admin chores belong in 5w, not raw git.
- [x] #39 USAGE hook line omits [pre-commit | pre-receive], so hook --help under-describes it @output !1 branch:output/task-39 submitted:75af304a7fa0 via:review reviewed:75af304a7fa0
  Found reviewing #25: tasks::USAGE has 'hook install | uninstall' while src/lint.rs refusals accept 'install | uninstall [pre-commit | pre-receive]'. Make USAGE match; command_usage() then prints it for 5w hook --help.
- [x] #37 FIVEW_WT_ROOT relative: resolved against the cwd when checked but against the primary checkout by git worktree add @wt !1 branch:wt/task-37 submitted:522b7ef9f714 via:review reviewed:522b7ef9f714
  Found reviewing #33 (pre-existing). A relative FIVEW_WT_ROOT is checked relative to the current directory, then handed to git worktree add run from the primary checkout, so the two can name different directories when 5w runs from a subdirectory or another worktree. Resolve it once against one base (the primary checkout, as worktrees.root is) in Repo::wt_root(); test by running wt new from inside another worktree with a relative FIVEW_WT_ROOT.
- [x] #42 FIVEW_WT_ROOT set but empty puts worktrees inside the primary checkout: treat empty as unset @wt !1 branch:wt/task-42 submitted:07cfc9286a90 via:review reviewed:07cfc9286a90
  Found reviewing #37: FIVEW_WT_ROOT="" counts as set, so Repo::wt_root() joins '' onto the primary and worktrees land at <primary>/<slug>, inside the repository. Treat an empty value as unset (fall back to worktrees.root / the default); test it.
- [x] #41 wt prune: keep any <area>/task-<id> branch of an open task, not only the currently suggested name @wt !1 branch:wt/task-41 submitted:0d13d58f3b0a via:review reviewed:0d13d58f3b0a
  Found reviewing #17: after 5w set <id> area <new>, the untouched <old>/task-<id> worktree is listed for removal because the suggestion changed. Keep any branch whose last component is task-<id> for a task not closed; test with set area after wt new.
- [x] #29 USAGE: one line linking the README on the web @output !1 needs:#13 branch:output/task-29 submitted:d456c3922d26 via:review reviewed:d456c3922d26
  From the AXI/clig.dev evaluation in docs/agent-output.md (#12, #13); see that section for the reasoning. clig: link to web docs in help text.
- [x] #34 USAGE: one example invocation under the usage line, e.g. 5w ready area:output @output !1 needs:#13 branch:output/task-34 submitted:13f4d86feea5 via:review reviewed:13f4d86feea5
  From the AXI/clig.dev evaluation in docs/agent-output.md (#12, #13); see that section for the reasoning. clig 'lead with examples', adopted narrowly; missing from the doc's own follow-up list. No longer worked sequence.
- [x] #23 opts(): refuse a flag a command does not understand instead of silently accepting it @output !2 needs:#13 branch:output/task-23 submitted:eaace9e5fe2e via:review reviewed:eaace9e5fe2e
  From the AXI/clig.dev evaluation in docs/agent-output.md (#12, #13); see that section for the reasoning. 5w ready --bogus exits 0 because opts() pushes unknown flags into o.flags. Refusal one line naming the fix.
- [x] #31 README: one-line uninstall note (cargo uninstall 5w) next to the install line @output !1 needs:#13 branch:output/task-31 submitted:5ea392f19198 via:review reviewed:5ea392f19198
  From the AXI/clig.dev evaluation in docs/agent-output.md (#12, #13); see that section for the reasoning.
- [x] #27 Color: disable for TERM=dumb and add an explicit --no-color flag @output !1 needs:#13 branch:output/task-27 submitted:3f91c12af32e via:review reviewed:3f91c12af32e
  From the AXI/clig.dev evaluation in docs/agent-output.md (#12, #13); see that section for the reasoning. Sty::new() checks only isatty and NO_COLOR; --no-color is swallowed today by the opts() unknown-flag bug.
- [x] #22 JSON list rows: trim to id, state, level, area, title, branch by default; --full for the rest @output !2 needs:#13 branch:output/task-22 submitted:3ec5e7834a6d via:review reviewed:3ec5e7834a6d
  From the AXI/clig.dev evaluation in docs/agent-output.md (#12, #13); see that section for the reasoning. AXI principle 2: rows carry 11 fields today.
- [x] #28 Color the '5w: <error>' refusal red when stderr is a terminal and NO_COLOR is unset @output !1 needs:#13 branch:output/task-28 submitted:5fb513e93e92 via:review reviewed:5fb513e93e92
  From the AXI/clig.dev evaluation in docs/agent-output.md (#12, #13); see that section for the reasoning. main.rs prints refusals with eprintln! uncolored.
- [x] #43 Unknown flags outside tasks.rs: wt ls, wt setup, report list and init accept them; lint reads --bogus as a revision @output !1 needs:#23 branch:output/task-43 submitted:b29d7315e48c via:review reviewed:b29d7315e48c
  Found reviewing #23 (which fixed the queue commands): wt ls --bogus, wt setup --bogus, report list --bogus and init --bogus exit 0 ignoring the flag; lint --bogus fails with a raw 'git rev-parse --verify --bogus^{commit}' error. Refuse in one line like #23 does (unknown flag --x for <cmd> (5w <cmd> --help)); test each.
- [x] #45 lint: refuse single-dash tokens and extra arguments instead of a raw rev-parse error or silently ignoring them @output !1 branch:output/task-45 submitted:40564aa78d6d via:review reviewed:40564aa78d6d
  Found reviewing #43: lint -x reaches git rev-parse as a raw error (only -- flags are checked); lint HEAD --staged and lint HEAD extra ignore everything after the first argument. Refuse both in one line naming the usage. Also docs/agent-output.md (~line 502) still says wt rm --force <branch> does not work; #30 fixed that — update the note.
- [x] #46 lint ^HEAD: a negated revision is taken as a commit and misreported as a branch queue edit @queue !1 branch:queue/task-46 submitted:3dd8ea9bdb7b via:review reviewed:3dd8ea9bdb7b
  Found reviewing #45: 5w lint ^HEAD prints '^<sha>: queue edits go on main, not a branch' because rev-parse --verify ^HEAD^{commit} returns a ^-prefixed sha used as a commit. Refuse a revision starting with ^ (or any rev-parse output that is not a plain sha) in one line; test it.
- [x] #47 ci --head/--base accept a negated revision: ci --head ^HEAD checks nothing and passes @ci !2 branch:ci/task-47 submitted:d1ee52a35ad8 via:review reviewed:d1ee52a35ad8
  Found by #46's worker: git::rev returns ^<sha> for ^HEAD. 5w ci --head ^HEAD with no base yields an empty commit list and passes without checking anything; a ^ in --base fails with a raw git error. pre-receive passes plain shas so is unaffected, but a CI job templated from user input could be. Make git::rev (or ci's resolution) refuse anything that is not a plain sha, in one line; test ci --head ^HEAD refuses.
- [x] #48 ci --trunk is never validated: a bad value reads an empty queue and, with require_task off, passes an unreviewed branch @ci !2 branch:ci/task-48 submitted:d1c205cb75d0 via:review reviewed:d1c205cb75d0
  Found reviewing #47: ci --trunk ^HEAD or a missing ref goes straight into merge-base and git show <trunk>:TASKS.md. With --branch, ship_check reads an empty queue; with require_task off it prints 'no task names X — unreviewed change' and passes even if a task names the branch unaccepted. Resolve --trunk through git::rev (plain sha) and refuse in one line; test ci --branch x --trunk nope refuses.
- [x] #49 ci/pre-receive: branch deletion in a SHA-256 repository is rejected (only 40 zeros treated as deletion) @ci !1 branch:ci/task-49 submitted:0ac527241bf7 via:review reviewed:0ac527241bf7
  Found reviewing #47: ci/pre-receive skips a deletion only when the new sha is 40 zeros; SHA-256 uses 64, so a delete goes on to 5w ci --head 000…0, which refuses and rejects the push. Treat an all-zero sha of either length as null (hook script and ci's --base zero stripping); test with git init --object-format=sha256.
- [x] #19 Batch accept (accept 4 5 6) and ship --accepted: land every accepted branch bottom of stack first @queue !2 branch:queue/task-19 submitted:cb26ea1932e4 via:review reviewed:cb26ea1932e4
  stop at the first refusal
  A supervisor round of 8 accepted tasks meant 8 accepts and 8 ships with the stack order (#12 before #13, #14 before #15) worked out by hand. Each accept keeps its own checks; ship --accepted keeps every ship refusal.
- [x] #9 self-update and version --latest: opt-in, the pinned version by default, checksums and signature verified @upkeep !2 branch:upkeep/task-9 submitted:c02d6dee354b via:review reviewed:c02d6dee354b
- [x] #44 Refusals that append usage text after a newline: make each one line naming the fix @output !1 branch:output/task-44 submitted:e8576b6d3bc3 via:review reviewed:e8576b6d3bc3
  Found reviewing #28: ship --bogus, ci 'unexpected', update-files, and tasks.rs 'text first' / 'unexpected' refusals print the error then a usage block on following lines (now all red on a colour terminal). CLAUDE.md: refusals one line naming the fix. Replace the block with a pointer such as '(5w <cmd> --help)', which #25 made print only that command's usage; test each.
- [x] #52 ship prints 'shipping unreviewed' before its checks run, so a later refusal leaves a false first line @output !1 branch:output/task-52 submitted:0872f8f5fc7d via:review reviewed:0872f8f5fc7d
  Found reviewing #44: src/ship.rs (~219) prints 'ship: no task references <branch> — shipping unreviewed' to stderr before the ship checks; if one then refuses (e.g. behind trunk), stderr has two lines and the first wrongly says it is shipping. Print the notice only once every check has passed (just before landing), or fold it into the success output; test a behind-trunk unreviewed branch gives exactly one refusal line.
- [x] #20 Several queue edits in one signed commit: one signature per supervisor round @queue !2 branch:queue/task-20 submitted:868cb997995d via:review reviewed:868cb997995d
  Each queue edit is its own signed commit; with a hardware key that is a touch per edit. Design how a batch (accepts, adds) commits once while keeping the private-index, compare-and-swap update-ref guarantee and one-line commit subjects that still name each edit.
- [x] #35 Stacked branches recorded with the trunk as parent: ship re-review and --sync conflicts on work already landed @wt !3 branch:wt/task-35 submitted:b1f5afd7b28b via:review reviewed:b1f5afd7b28b
  Found 2026-09-16 shipping #12-#15. output/task-13 held #12's commit but recorded parent main: once #12 landed as a new commit, ship refused #13 (reviewed diff included #12) and needed open/submit/accept by hand. audit/task-15 (parent bench/task-14, correct) still hit a --sync conflict in tests/flow.rs replaying #14's commit. Fix both: (1) wt new records the branch actually stood on, and doctor/wt ls flag a branch whose commits include another unshipped branch's tip while its recorded parent is the trunk; (2) ship --sync rebases only the branch's own commits (git rebase --onto trunk <old parent tip>), and the gate compares the branch's own diff, so a landed parent does not force a re-review. Test: stack B on A, ship A --sync after trunk moves, ship B --sync lands without re-review.
- [x] #53 Bench: normalise commit shas in measured output so an unrelated template or history change doesn't move token counts @bench !1 branch:bench/task-53 submitted:a2caeea2d5be via:review reviewed:a2caeea2d5be
  Found reviewing #20: changing templates/PROTOCOL.md changes every later commit sha in the bench's generated repo; review --json's tip sha then estimates to more tokens and the baseline fails. Replace hex shas in measured output with a fixed-length placeholder before counting tokens (bytes unchanged), or fix commit content so shas are stable; regenerate the baseline once.

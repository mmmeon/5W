# 5W

A task queue that lives in a markdown file on your trunk, a review gate between "finished" and
"landed", one worktree per stacked branch, and a ship command that refuses to land anything other
than the change that was reviewed. One static binary, no dependencies, no daemon, no database.

Built for a supervisor handing work to agents (or people) in parallel: the queue says what is ready
and for whom, `delegate` prints the brief, workers `submit`, the supervisor `accept`s or `reject`s
with a reason that travels into the next brief, and `ship` fast-forwards the trunk.

Extracted from the `bin/tasks`, `bin/wt` and `bin/ship` scripts of a research repo, where it ran
several hundred tasks across forty-branch sessions. Every rule here was bought by a failure there;
the source comments say which.

```
cargo install --path .        # installs `5w`
cd your-repo && 5w init       # writes .5w.toml and TASKS.md, commits them
```

## The file

```
## Open

- [ ] #14 decode the header  @faces !3 >agent needs:#12,#13
  Optional body. Every line indented two spaces under a task belongs to it
  and moves with it — for briefs too long for one line.

## Done

- [x] #12 capture samples  @faces !1 >restricted via:self
- [x] #13 read the spec  @faces !2 branch:faces/spec via:review reviewed:3f9c2a1b7d04
```

| Field                    | Meaning                                                                                       |
| ------------------------ | --------------------------------------------------------------------------------------------- |
| `@area`                  | groups the backlog; `delegate` lists the docs that exist under that directory                 |
| `!1`–`!4`                | complexity — the _decision content_, not the size. Picks which model can be trusted with it   |
| `>lane`                  | who can execute it at all: a configured name with a kind — `agent`, `restricted`, `manual`, `decision` (defaults: `>agent` `>restricted` `>manual` `>owner`) |
| `needs:#a,#b`            | blocked until every one is `[x]`                                                              |
| `branch:`                | the branch doing the work; `ship` keys the gate on it                                         |
| `rework:"…"`             | why the last attempt was rejected; `delegate` opens with it                                   |
| `via:`                   | how it closed — `review`, or the lane's close word (`self`, `decided`)                        |
| `submitted:` `reviewed:` | the commits submit and accept saw; `accept` and `ship` check them                             |

`[ ]` open · `[~]` submitted · `[x]` closed. Lines inside ```fences are never tasks. Ids are
permanent. Fields are order-free, and the *last* token of a kind is the field — an earlier`>manual` in the prose stays prose.

### Lanes

A lane says who can execute a task at all. Its name is the project's own word; its **kind** is what
the work needs, and 5W's behaviour follows the kind:

| Kind | The work needs | Delegable | Closes without review |
|---|---|---|---|
| `agent` | nothing but the repository | yes | `done --self` |
| `restricted` | access an agent may not have: a machine, an account, a secret | yes | `done --self` |
| `manual` | a person, by hand, outside the repository | no | `done --self` |
| `decision` | the owner's call | no | `done --decided` |

The defaults are `>agent`, `>restricted`, `>manual` and `>owner`. A project renames freely —
`[lanes.game]` with `kind = "manual"` is a game-client capture lane. `delegate` refuses a manual or
decision task; `submit` and `accept` warn on one, since nothing can check who did the work.

## The loop

```bash
5w ready                        # delegable, unblocked, grouped by complexity
5w delegate 14                  # the brief: body, rework note, docs, branch, what to do when done
5w wt new faces/header          # branch + worktree, child of whatever you stand on
cd "$(5w wt path faces/header)"
# … work, commit …
5w submit 14                    # worker: hands it back; records the tip

5w review                       # supervisor: diffstat, drift since submit, the checklist
5w accept 14                    # records the reviewed commit — or:
5w reject 14 the parser drops the | case    # back to open, reason attached
5w ship faces/header --sync     # rebase, verify, fast-forward, clean up
```

Ids need no quoting: `14` and `'#14'` are the same. Filters and fields have shell-safe spellings —
`lane:owner` for `'>owner'`, `level:3` for `'!3'`, `area:x` for `@x` — because an unquoted `>agent`
is a redirect that silently creates a file called `agent`.

## What the gate guarantees

**Every queue change is its own commit on the trunk, and holds nothing else.** The edit is applied to
the trunk's committed file and committed with plumbing — a private index, `commit-tree`, and a
compare-and-swap `update-ref` — then mirrored into the trunk's checkout, wherever that is: its
working file, and its index entry, on top of anything already staged there. A peer's uncommitted
row stays uncommitted; it is never swept into your commit, never makes your command decline, and
the next ordinary `git commit` in that checkout does not revert the queue. Writers take a lock, so
parallel `add`s mint distinct ids, and state checks read the committed queue under that lock — a
hand-edited `[~]` in a working copy does not make a task acceptable.

**Closing says how.** `accept` records `via:review`. `done` refuses without the flag its lane
names — `--decided` on a decision lane, `--self` on the rest — so closing an owner decision is never
a reflex. Nothing can check who is typing; the flag makes it a deliberate, recorded act.

**Accept is of what was submitted.** A branch that gained commits after `submit` is not accepted
until you say which commit you reviewed (`--at`). A task whose branch does not exist is not
accepted at all without `--force`, and a closed task's fields cannot be changed without reopening
it — otherwise a closure could be pointed at a branch it never saw. Field values are validated
(branch names by `git check-ref-format`, no whitespace or control characters), so no value can
write a second line into the file.

**Ship lands what was accepted.** `ship` compares the exact diff the branch adds now with the one it
added at the reviewed commit — whitespace, modes and binary content included, only blob ids and
hunk line numbers normalised away. Not `git patch-id`, which ignores whitespace and would pass an
indentation change made after review. A clean rebase passes. A commit added after review, or a
rebase that changed the lines next to the change, does not, and a refused `--sync` puts the branch
back where it was. The check runs before anything rewrites the branch, and again after the rebase.

**Ship refuses before it acts**, naming the fix: trunk or perennial branch; not accepted; stacked on
an unshipped parent (ship the bottom first; children are reparented after); behind the trunk (use
`--sync`); a dirty branch worktree; tracked changes in the trunk's checkout (uncommitted queue rows
excepted). Untracked strays do not block it.

**Removing a worktree deletes its gitignored files**, so ship lists them and refuses — an
extraction's output or a local database dies silently otherwise. Symlinks `wt` made and anything
named in `worktrees.disposable` (default `node_modules`, `target`, `.next`) are exempt;
`--discard-ignored` accepts the loss.

**The trunk moves first.** The fast-forward happens before the worktree is removed or the branch
touched, so a fast-forward git refuses leaves everything exactly as it was.

**`--squash` cannot revert the trunk.** The squash commit is built from the branch's own tree and
parented on the trunk _sha_ the branch was just verified against — never on the trunk by name, which
is how a `reset --soft main` after main moved once deleted a row from the queue.

`--force` overrides the review gate only, never a safety check.

## Without the tool

[PROTOCOL.md](PROTOCOL.md) is the spec the binary implements, written as instructions: the row format,
each edit and what it must carry, and the commit rules. `5w init` copies it into the repo, so a
contributor or agent without `5w` edits `TASKS.md` by hand and gets the same result.

Hand edits are checked, not trusted:

- **`5w lint`** compares the queue before and after — `--staged`, one commit, or a range — and judges
  every row that changed by its transition: `[ ]→[~]` carries `branch:` and `submitted:`,
  `[~]→[x]` carries `via:review` and `reviewed:`, a close carries its lane's `via:`, a reject its
  `rework:`, and nothing else gains one. Closed rows are immutable except to reopen, reflow (`split`)
  or archive; no row is deleted and no id reused; a queue edit is its own commit on the trunk. Every
  commit `5w` itself makes passes it — a `reject` commit may gain `rework:` from any state, as
  releases through 0.1.3 rejected unsubmitted tasks — and the test suite lints its own history.
- **`5w hook install`** (also run by `5w wt setup`) adds a pre-commit hook running `5w lint --staged`.
  Where `5w` is not installed the hook lets the commit through with a warning to follow
  PROTOCOL.md; `5w lint <range>` catches what that let through, later.

A hook is a convenience, not a gate: `--no-verify` skips it. When adopting 5W on an existing queue,
lint from the adoption commit onward — earlier rows predate `submitted:` and `reviewed:`.

## CI and servers

One command holds every check, and takes everything as arguments — no forge's variables are read, so
the same command runs under any CI, in a server hook, and by hand:

```
5w ci --base <old> --head <new> --ref refs/heads/<name>   # a push
5w ci --base <old> --head <new> --branch <name>           # a change request into the trunk
```

- **A push to the trunk:** every commit in the range is linted as landing on the trunk.
- **A push to any other branch:** its commits carry no queue edits.
- **A change request:** no queue edits, plus the ship check — an accepted task names the branch, and
  what the branch adds is exactly what was reviewed (a clean rebase passes). The check is red until
  the task is accepted; re-run it after `5w accept`.

A missing or all-zero `--base` means the merge-base with the trunk. The checkout needs full history.

| Where                                                                           | How                                                                                                                                                                                                                                  |
| ------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Any git server you run — bare repo over SSH, Gitea, Forgejo, self-hosted GitLab | `5w hook install pre-receive` in the bare repo ([ci/pre-receive](ci/pre-receive)). A real gate: bad pushes are refused. Without `5w` on the server it refuses rather than waves through (`git config 5w.allowMissing true` to relax) |
| GitHub, Forgejo, Gitea Actions                                                  | [ci/github-actions.yml](ci/github-actions.yml)                                                                                                                                                                                       |
| GitLab CI                                                                       | [ci/gitlab-ci.yml](ci/gitlab-ci.yml)                                                                                                                                                                                                 |
| Anything else                                                                   | map its before/after SHAs, ref and change-request branch onto the flags                                                                                                                                                              |

The wrappers install exactly the version the project pins (see *Staying current*), verified —
[ci/install-5w.sh](ci/install-5w.sh), inlined.

This repository's own [.github/workflows/ci.yml](.github/workflows/ci.yml) runs `cargo fmt`, `cargo
clippy` and `cargo test` on every push and change request, then the same `5w ci` check against 5W's
own history — built from source rather than installed, since the commit under test is 5w itself.

### Release binaries

```
ci/release.sh        # dist/5w-<version>-{x86_64,aarch64}-unknown-linux-musl + dist/SHA256SUMS
```

Static musl executables, about 1 MB, runnable on any Linux. The build runs in a pinned
`rust:<version>-alpine` container (podman or docker), so the host needs no Rust toolchain, and it is
reproducible: the same commit gives the same bytes on any machine (compiler pinned, paths remapped,
`--locked`, commit timestamp). Before writing `SHA256SUMS` it runs the full test suite against the
artifact the host can execute (`FIVEW_TEST_BIN`), so what ships is what was tested.
`5w --version` names the commit.

Releasing ties the published binaries to the release key without the key leaving the maintainer's
machine:

1. `ci/tag-release.sh` builds, then makes a **signed tag whose message is `SHA256SUMS`**.
2. Pushing it runs [the release workflow](.github/workflows/release.yml): it checks the tag is signed
   by the key in [SIGNING_KEY.asc](SIGNING_KEY.asc) (fingerprint pinned in the workflow), rebuilds,
   tests, and **refuses to publish unless its sums equal the signed ones** — possible because the
   build is reproducible.
3. `ci/sign-release.sh v<version>` checks the published sums against a local build and attaches
   `SHA256SUMS.asc`, which installers verify.

**What is and is not gated.** Queue commits land directly on the trunk, and hosted CI runs after a
push is accepted: there a bad queue edit turns the build red rather than being refused. For a hard
gate on a hosted forge, protect the trunk, require change requests for people, and let only the
account that runs `5w` push queue commits. The ship check runs on change requests; a push of
unreviewed _code_ straight to the trunk is linted for queue edits but not matched to a review — keep
the trunk protected so code arrives by change request.

## Keeping context small

The queue is read by agents, so every read is priced in tokens.

- **`5w archive`** moves closed tasks to `DONE.md` in one commit. On a 900-task queue that took
  `TASKS.md` from 649 KB to 52 KB. Archived ids stay taken, still satisfy `needs:`, still show under
  `5w show`, and ship still reads their `branch:` and `reviewed:`.
- **Titles are short; detail is body.** Text over `title_max` (120) is split at the first sentence
  into a title and an indented body, by `add` and, for existing rows, by `5w split`. Lists print
  titles; `show`, `next` and `delegate` print bodies.
- **Output is compact off a terminal**, or with `FIVEW_AGENT=1`: one line per task
  (`#14 !3 @faces title + [branch] >lane — note`, `+` marking a body; a title still over `title_max`
  ends `…(+53 chars: 5w show 14)`), no headers, one-line
  refusals naming the fix. `FIVEW_AGENT=0` or `--full` gives the human layout. `ready`'s summary
  line ends with the next step (`→ 5w delegate 14`) and `review` closes with `→ 5w accept 14`, as
  `next` does; `--json` and `--ids` carry no hint.
- **`--json`**, **`--ids`**, **`--limit N`** on every list; **`5w next [filters]`** returns just the
  first ready task with its body. `review --json` adds each submitted task's `tip`, `moved` (commits
  since submit), `diff` (shortstat against the trunk) and `behind`.
- **The brief is the worker's only document.** `delegate` prints the task, the rework note, the
  steps and the rules, and names the sections the text cites (`refs: client/FINDINGS.md #779`) so the
  worker reads those rather than whole files. `review` prints its checklist once, on
  `--checklist`.
- **Help is per command.** `5w <command> --help` prints that command's entry from the listing (and
  the `filters`, `out` or `ids` line it refers to), not the whole listing; bare `5w --help` prints
  all of it. `wt`, `ship`, `lint`, `ci`, `report` and `audit` print their own usage.

### Measuring output

`tests/bench.rs` prices what every read costs. It generates queues of 10, 100 and 1000 tasks — open,
submitted and blocked tasks, bodies, rework notes, every lane kind, half as many again archived —
runs `ready`, `next`, `ls`, `all`, `review` (each with and without `--json`), `show`, `delegate`,
`doctor`, `lint` and four common refusals off a terminal, and records stdout+stderr bytes, estimated
tokens and wall time:

```
tasks  case                          bytes  ~tokens  exact     ms
 1000  ready                         40884    10642      -   10.5
 1000  ready --json                 114758    31613      -   10.9
 1000  review                        16834     5078      -  384.4
 1000  delegate <id>                   488      143      -   11.2
 1000  refuse: done <id> (no flag)     118       40      -   11.3
```

Bytes and tokens are compared with [tests/bench.baseline](tests/bench.baseline); a row that grows
more than 2% (and more than 2 bytes or tokens) fails `cargo test`, as does a case added or removed
without the baseline. So output grows only as a deliberate change: rerun with
`FIVEW_BENCH_UPDATE=1 cargo test --test bench` and the baseline's diff shows the cost in review. A
row that shrank passes with a note, to be locked in the same way. Time is printed, never checked:
it depends on the machine and the load, and a flaky check teaches people to ignore it. Every byte
is deterministic — fixed identity and commit dates, so shas repeat, and the scratch path replaced by
a fixed one.

It is a test rather than a `5w bench` subcommand so that it ships nothing: the binary stays the tool,
and the measurement runs wherever the tests do — `cargo test` locally and in CI, with no extra step,
and against a release artifact with `FIVEW_TEST_BIN`. To see the table:
`cargo test --release --test bench -- --nocapture`. `FIVEW_BENCH_TOLERANCE=<percent>` loosens the
check.

**Tokens are estimated** by [src/tokens.rs](src/tokens.rs), since no tokenizer can ship without a
dependency and every model family has its own vocabulary. It prices the pieces a byte-pair encoder
cuts text into: a run of letters 1 token per 7, digits 1 per 3, punctuation 1 per 3, a line break
or a run of indentation 1, any other character 1, and a single space joins the piece after it.
Against o200k_base and cl100k_base it is within 10% on both the benchmark's output and this
repository's prose — good for comparing one output with another, not for billing. For exact
counts, `FIVEW_TOKENIZER` names a command that reads text on stdin and prints a count; it fills the
`exact` column and is never checked against the baseline:

```
FIVEW_TOKENIZER='uvx -q --with tiktoken python -c "import sys, tiktoken; print(len(tiktoken.get_encoding(\"o200k_base\").encode(sys.stdin.read())))"' \
  cargo test --release --test bench -- --nocapture
```

## Auditing how a repository uses 5W

```
5w audit [--since <rev|YYYY-MM-DD>] [--json] [--full]
```

`audit` reports how a repository has used 5W, from what 5W already records: nothing new is logged,
no transcripts are read, and nothing is written. The queue's history is the record. Every state
change is a commit on the trunk, so replaying `TASKS.md` and `DONE.md` commit by commit gives each
task's transitions and their dates. One `git log -p` over the two files, along the trunk's
first-parent line, rebuilds both files at every commit; rows are compared by id, so an archive is a
move, and a hand edit counts the same as one `5w` made. Git runs a handful of times, not once per
commit: on a queue of 815 tasks with 1,705 queue commits, 348 of them by hand, `audit` takes 1.5 s,
most of it linting those 348.

`--since` narrows every section to what happened after a commit (the commits in `<rev>..<trunk>`)
or a date (UTC); the state the window starts from is still read from the whole history. A
repository with no queue history gets the same sections, empty. Compact off a terminal with capped
lists; `--full` lists more, `--json` everything.

| Section | Shows | Why |
|---|---|---|
| `tasks` | tasks by state, closure (`via:`), lane, level and area | where the work is, and whether lanes and levels are used as configured |
| `review` | submit→accept time per accepted attempt: median, p90, the slowest | how long finished work waits on a reviewer, the bottleneck once the queue grows |
| `rework` | rejections, how many tasks were sent back once, twice, three or more times, and the reasons | a reason that repeats points at a brief, a convention or a level that is wrong |
| `reopened` | closed tasks opened again, and how often | work that was closed too early |
| `blocked` | time open tasks waited on `needs:`, the longest waits and what held them | dependencies that stall delegable work |
| `outside` | queue commits 5W does not make — a message it never writes, or other files in the same commit — with lint's findings on them | hand edits, each judged against PROTOCOL.md. 5W's own commits pass lint by construction and are not linted again |
| `doctor` | `5w doctor`'s findings on the queue as it is now | a problem in the file now, beside the history that made it |
| `failures` | the last failure 5W recorded (`.git/5w/last-failure.md`) and the saved reports, sent or not | refusals and crashes agents met. Only the last failure is kept, so this is a pointer, not a rate |
| `briefs` | the `delegate` brief of every open delegable task, in bytes and estimated tokens, largest first | a worker's context starts with its brief; the estimator is the benchmark's |

**Cut: branches shipped without a task.** Ship fast-forwards the trunk and deletes the branch, so a
shipped branch's name survives nowhere 5W records — not in the commits, and in the reflog only
locally and until it expires. Matching landed commits to `reviewed:` shas instead fails on every
ship that rebased. A section built on either would be wrong in both directions, so there is none;
`require_task = true` is the enforcement.

Durations are between commit times (the committer date): a task submitted a day after the work was
finished is measured from the submit. Blocked time still running is measured to now.

## Worktrees

```bash
5w wt new <branch> [--from <parent>] [--install]
5w wt add <branch>        # worktree for an existing branch
5w wt ls | path | rm | link | install | setup
5w wt discard-copy <branch>   # drop the trunk checkout's uncommitted copy of <branch>'s diff
5w wt prune [--yes]       # list, then remove, branches and worktrees left with nothing in them
```

Worktrees go in `worktrees.root` (default `../<repo>-wt`, or `$FIVEW_WT_ROOT`); a relative root is
read from the primary checkout, and its `..` is resolved as written (symlinks are kept) so every path
5w prints is clean. `.worktree-links`
lists gitignored paths or globs to symlink in from the primary — `.env`, a dev database, large
samples. **Links are shared, not copied**; a branch that must change one copies it. Globs expand
file by file so a directory with tracked content is never shadowed. `node_modules` is never linked;
set `worktrees.install` to run a real install per worktree.

Stacking records the parent in git-town's own config keys, so `git town sync` and friends work on
these branches when git-town is installed, and nothing requires it when it is not. `5w wt setup`
configures both.

`5w wt prune` lists the branches safe to drop, with their worktrees: no commits past the recorded
parent (the trunk when none is recorded), no task in the queue or archive naming the branch — nor
suggesting it: a task not closed keeps `<area>/task-<id>`, which a worker creates long before submit
records it — and a worktree, if there is one, under `worktrees.root` that is clean, unlocked and
holds no gitignored files ship would refuse to delete (`worktrees.disposable` exempt). The trunk,
perennial branches and the primary checkout are never touched, nor the worktree you stand in, a
branch a rebase or bisect is working on, or a branch another kept branch is stacked on. A branch
that qualifies but for one of these is listed as kept, with the reason. `--yes` removes them — all
checked before the first goes, each checked again just before it goes (its tip, a rebase or bisect,
changes and ignored files; any difference stops the run), children before parents — with their
git-town config keys.

A branch's change copied into the trunk checkout and left uncommitted — a worker that edited the
wrong directory, a patch applied to try it — blocks shipping that branch. `5w doctor` names each
branch whose whole diff (`main...branch`) the trunk checkout's uncommitted changes equal exactly,
untracked new files included, and `5w wt discard-copy <branch>` discards them. Files compare as git
would stage them (so a line-ending conversion is not a difference), each path's before and after and
its mode. It refuses on any other change in the checkout, a partly staged file, a path that becomes
a directory or back, anything on disk (an ignored file, a directory) where a deleted file would
come back or in place of a parent directory — restoring would take it along — a submodule, or
anything it cannot hash — checking all of that before it touches a file — then restores those files from
`HEAD` and deletes the ones the branch adds. Doctor stays quiet about what it cannot compare, and
narrows hundreds of branches to the possible copies in one git call.

## Configuration

`.5w.toml` at the repo root, read from the trunk. Every key is optional; see
[templates/5w.toml](templates/5w.toml) for the defaults and
[examples/ara.toml](examples/ara.toml) for a full configuration with an extra non-delegable lane
that keeps its own section, a custom brief footer and a review checklist.

| Key                                         |                                                                                       |
| ------------------------------------------- | ------------------------------------------------------------------------------------- |
| `trunk`, `file`, `archive`, `commit_prefix` | where the queue and its archive live, how commits read                                |
| `title_max`                                 | longest task line text before it is split into title and body (0: off)                |
| `perennial`                                 | branches never shipped, rebased or deleted (git-town's list is honoured too)          |
| `require_task`                              | refuse to ship a branch no task names                                                 |
| `[sections]`                                | `open`, `done` headings; created if missing                                           |
| `[levels]`                                  | the tier text per complexity                                                          |
| `[lanes.<name>]`                            | `kind`, `section`; `delegable`, `close`, `note`, `refuse` override the kind            |
| `[worktrees]`                               | `root`, `links_file`, `install`, `install_marker`, `disposable`                       |
| `[delegate]`                                | `context`, `area_docs`, `conventions`, `footer` (`{tasks} {ship} {wt} {id} {branch}`) |
| `[review]`                                  | `checklist`, printed under `5w review`                                                |
| `[commands]`                                | how briefs spell the tools: `tasks`, `wt`, `ship`                                     |

Symlink the binary as `tasks`, `wt` or `ship` and it behaves as that tool, so a repo can keep its
existing spellings.

## Adopting it in a repo that used the shell scripts

The file format is a superset: existing `TASKS.md` files parse unchanged (checked against an
810-task queue — the only differences are words the shell version wrongly stripped out of task
text). Differences in behaviour to know about:

- `add` commits. It used to leave the row uncommitted.
- `accept` requires the task to be submitted (`--force` to override), and refuses when the branch
  moved after submit or no longer exists.
- `set` refuses closed tasks; `open` them first.
- `ship` refuses when the worktree holds gitignored files it would delete (`--discard-ignored`).
- A fenced example's id counts toward the next id, so no two lines ever share a number.
- `ship` needs no cwd, needs no git-town, and can `--sync` and `--squash` itself.
- `reject` requires the task to be submitted, takes the reason as the remaining arguments and accepts
  any character in it.
- The branch scans that read rework notes, submissions and ids off every branch are gone: they
  existed for a layout where every branch carried its own queue, which the scripts had already
  abandoned.
- Reads take milliseconds; the shell version took ten seconds on the same queue.

## Staying current

A project using 5W drifts in three places, and each is covered:

- **The binary.** `.5w.toml` pins the oldest 5w the project works with: `requires = "0.1.2"` (`init`
  writes it). An older binary refuses with the version to get and where, before reading anything
  else in the config — so a key it does not know cannot hide the real problem. Newer binaries run:
  releases only add config keys.
- **CI.** [ci/install-5w.sh](ci/install-5w.sh), inlined in both wrappers, reads that same pin,
  downloads that release, checks it against `SHA256SUMS`, and — when the release carries
  `SHA256SUMS.asc` — that the sums are signed by the release key, whose fingerprint the script pins.
  `FIVEW_REQUIRE_SIGNATURE=1` makes an unsigned release fatal. Upgrading a project, locally and in
  CI, is one reviewed line.
- **Files 5W installed.** PROTOCOL.md and the hooks carry the version that wrote them.
  `5w doctor` notes a stale or hand-edited copy and a pin older than the running binary;
  `5w update-files` rewrites them in the worktree you stand in (hooks in place), and `--pin` raises
  `requires` — left uncommitted, to review like any change.

Nothing checks for new releases in the background: agents often run offline, and a private
repository should not call out on its own.

## Reporting problems with 5W

Most people using 5W are agents, so the path is built for them:

```
5w report "accept refused a task I had just submitted" --expected "it accepts"
5w report list | show <n>
5w report send <n> --print     # the prefilled issue URL; or --gh to create it, or no flag for a browser
```

`report` saves a Markdown report under `.git/5w/reports/` with the environment (5w version and commit,
OS, git) and the **last failure 5w recorded** — the exact command and message — so the reporter does not
reconstruct it. A crash is recorded the same way and says how to report it. **Nothing is sent** until
`send`: a report can quote task text or branch names from a private repository, so read it first.
Briefs from `delegate` tell workers to use `report` when a refusal itself looks wrong.

Issues go to `mmmeon/5W`; `FIVEW_ISSUES=owner/repo` points them at a fork.

## Development

```
cargo test        # unit tests + end-to-end tests against scratch git repos, and the output benchmark
```

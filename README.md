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
  `rework:`. Closed rows are immutable except to reopen, reflow (`split`) or archive; no row is
  deleted and no id reused; a queue edit is its own commit on the trunk. Every commit `5w` itself
  makes passes it — the test suite lints its own history.
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
  (`#14 !3 @faces title + [branch] >lane — note`, `+` marking a body), no headers, one-line
  refusals naming the fix. `FIVEW_AGENT=0` or `--full` gives the human layout.
- **`--json`**, **`--ids`**, **`--limit N`** on every list; **`5w next [filters]`** returns just the
  first ready task with its body.
- **The brief is the worker's only document.** `delegate` prints the task, the rework note, the
  steps and the rules, and names the sections the text cites (`refs: client/FINDINGS.md #779`) so the
  worker reads those rather than whole files. `review` prints its checklist once, on
  `--checklist`.

## Worktrees

```bash
5w wt new <branch> [--from <parent>] [--install]
5w wt add <branch>        # worktree for an existing branch
5w wt ls | path | rm | link | install | setup
```

Worktrees go in `worktrees.root` (default `../<repo>-wt`, or `$FIVEW_WT_ROOT`). `.worktree-links`
lists gitignored paths or globs to symlink in from the primary — `.env`, a dev database, large
samples. **Links are shared, not copied**; a branch that must change one copies it. Globs expand
file by file so a directory with tracked content is never shadowed. `node_modules` is never linked;
set `worktrees.install` to run a real install per worktree.

Stacking records the parent in git-town's own config keys, so `git town sync` and friends work on
these branches when git-town is installed, and nothing requires it when it is not. `5w wt setup`
configures both.

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
- `reject` takes the reason as the remaining arguments and accepts any character in it.
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
cargo test        # unit tests + end-to-end tests against scratch git repos
```

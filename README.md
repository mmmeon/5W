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

- [x] #12 capture samples  @faces !1 >local via:self
- [x] #13 read the spec  @faces !2 branch:faces/spec via:review reviewed:3f9c2a1b7d04
```

| Field | Meaning |
|---|---|
| `@area` | groups the backlog; `delegate` lists the docs that exist under that directory |
| `!1`–`!4` | complexity — the *decision content*, not the size. Picks which model can be trusted with it |
| `>lane` | who can execute it at all. Lanes are configured; the default set is `agent`, `local`, `owner` |
| `needs:#a,#b` | blocked until every one is `[x]` |
| `branch:` | the branch doing the work; `ship` keys the gate on it |
| `rework:"…"` | why the last attempt was rejected; `delegate` opens with it |
| `via:` | how it closed — `review`, or the lane's close word (`self`, `decided`) |
| `submitted:` `reviewed:` | the commits submit and accept saw; `accept` and `ship` check them |

`[ ]` open · `[~]` submitted · `[x]` closed. Lines inside ``` fences are never tasks. Ids are
permanent. Fields are order-free, and the *last* token of a kind is the field — an earlier
`>game` in the prose stays prose.

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
compare-and-swap `update-ref` — then mirrored into the primary checkout. A peer's uncommitted row
in the same file stays uncommitted; it is never swept into your commit, and it never makes your
command decline. Writers take a lock, so parallel `add`s mint distinct ids.

**Closing says how.** `accept` records `via:review`. `done` refuses without the flag its lane
names — `--self` on `agent`/`local`, `--decided` on `owner` — so closing an owner decision is never
a reflex. Nothing can check who is typing; the flag makes it a deliberate, recorded act.

**Accept is of what was submitted.** A branch that gained commits after `submit` is not accepted
until you say which commit you reviewed (`--at`).

**Ship lands what was accepted.** `ship` compares the patch-id of what the branch adds now with what
it added at the reviewed commit. A clean rebase passes. A commit added after review, or a conflict
resolved differently, does not. This check runs before anything rewrites the branch.

**Ship refuses before it acts**, naming the fix: trunk or perennial branch; not accepted; stacked on
an unshipped parent (ship the bottom first; children are reparented after); behind the trunk (use
`--sync`); a dirty branch worktree; tracked changes in the trunk's checkout. Untracked strays in
the primary do not block it. If the fast-forward fails, the removed worktree is put back.

**`--squash` cannot revert the trunk.** The squash commit is built from the branch's own tree and
parented on the trunk *sha* the branch was just verified against — never on the trunk by name, which
is how a `reset --soft main` after main moved once deleted a row from the queue.

`--force` overrides the review gate only, never a safety check.

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

`.5w.toml` at the repo root. Every key is optional; see
[templates/5w.toml](templates/5w.toml) for the defaults and
[examples/ara.toml](examples/ara.toml) for a full configuration with an extra non-delegable lane
that keeps its own section, a custom brief footer and a review checklist.

| Key | |
|---|---|
| `trunk`, `file`, `commit_prefix` | where the queue lives and how its commits read |
| `perennial` | branches never shipped, rebased or deleted (git-town's list is honoured too) |
| `require_task` | refuse to ship a branch no task names |
| `[sections]` | `open`, `done` headings; created if missing |
| `[levels]` | the tier text per complexity |
| `[lanes.<name>]` | `delegable`, `close`, `section`, `note`, `refuse` |
| `[worktrees]` | `root`, `links_file`, `install`, `install_marker` |
| `[delegate]` | `context`, `area_docs`, `conventions`, `footer` (`{tasks} {ship} {wt} {id} {branch}`) |
| `[review]` | `checklist`, printed under `5w review` |
| `[commands]` | how briefs spell the tools: `tasks`, `wt`, `ship` |

Symlink the binary as `tasks`, `wt` or `ship` and it behaves as that tool, so a repo can keep its
existing spellings.

## Adopting it in a repo that used the shell scripts

The file format is a superset: existing `TASKS.md` files parse unchanged (checked against an
810-task queue — the only differences are words the shell version wrongly stripped out of task
text). Differences in behaviour to know about:

- `add` commits. It used to leave the row uncommitted.
- `accept` requires the task to be submitted (`--force` to override), and refuses when the branch
  moved after submit.
- `ship` needs no cwd, needs no git-town, and can `--sync` and `--squash` itself.
- `reject` takes the reason as the remaining arguments and accepts any character in it.
- The branch scans that read rework notes, submissions and ids off every branch are gone: they
  existed for a layout where every branch carried its own queue, which the scripts had already
  abandoned.
- Reads take milliseconds; the shell version took ten seconds on the same queue.

## Development

```
cargo test        # unit tests + end-to-end tests against scratch git repos
```

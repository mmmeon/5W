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
cd your-repo && 5w init       # writes .5w.toml, TASKS.md, PROTOCOL.md; commits them
```

`init` commits on the trunk and writes its name as `.5w.toml`'s `trunk`: `FIVEW_TRUNK` or `git config
5w.trunk` when set, else the branch the primary worktree has checked out, else the one `origin/HEAD`
names (then `git-town.main-branch`, then `main`) when it is detached. `origin/HEAD` and
`git-town.main-branch` count only when the branch they name exists (a remote's rename leaves
`origin/HEAD` stale). On a branch they contradict it refuses, naming `git switch <trunk>` — the queue
does not live on a feature branch — unless that trunk is checked out in another worktree, where it
commits. A trunk other than `main` is also recorded as `git config 5w.trunk <trunk>` (init prints
it): commands find `.5w.toml` on `main` or in the primary checkout. The pin is local, so where
neither has the file (a clone whose primary checkout is on another branch) they read `trunk` from the
`.5w.toml` committed on the branch `origin/HEAD` names, then on `main` or `master`, local then
`origin/`, counting only a `trunk` that names an existing branch. Where the trunk exists only as
`origin/<trunk>` (no local branch), reads show the queue committed there and writes, `submit` and `ship` refuse, naming
`git branch <trunk> origin/<trunk>`.

To uninstall: `cargo uninstall fivew` (the package is `fivew`, the binary `5w`), or delete the release
binary from wherever you put it. A repository keeps its committed `.5w.toml`, `TASKS.md`,
`PROTOCOL.md` (and `DONE.md` once archived); `5w hook uninstall` (`5w hook uninstall pre-receive` on
a server) removes the hooks. Also left behind: worktrees under `worktrees.root` (default
`../<repo>-wt`), and git config from `5w wt setup` (`rebase.updateRefs`, `git-town.*` when git-town
is present) and per branch (`git-town-branch.<b>.parent` / `.branchtype`).

## The file

```
## Open

- [ ] #14 decode the header  @faces !3 >agent needs:#12,#13
  Optional body. Every line indented two spaces under a task belongs to it
  and moves with it — for briefs too long for one line.

## Done

- [x] #12 capture samples  @faces !1 >restricted via:self
- [x] #13 read the spec  @faces !2 branch:faces/spec via:review reviewed:3f9c2a1b7d04e5a6b8c9d0e1f2a3b4c5d6e7f8a9
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
| `submitted:` `reviewed:` | the commits submit and accept saw, as full shas (text output shows 12 digits, `--json` all); `accept` and `ship` check them |

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

A round of reviews batches: `5w accept 14 15 16` accepts each in turn — every check, one commit
each — and stops at the first refusal, naming what was accepted and what was not tried (`--at`
names one commit, so it takes one id). `5w ship --accepted --sync` then ships every branch an
accepted task names that still exists, parents before their children (from the recorded stack),
each exactly as `ship <branch>` would, and stops at the first refusal with the branch it stopped at
and what already landed. It takes `--sync`, `--squash` and `--discard-ignored` for every branch;
not `-m` or `--force`, which belong to one branch.

Where every commit is signed with a hardware key, a commit per edit is a touch per edit. `5w batch`
reads write commands from stdin, one per line as they would follow `5w`, and commits them as one:

```bash
5w batch <<'EOF'
accept 14
accept 15
reject 16 'the parser drops the | case'
add "follow-up: cover the | case" area:faces
EOF
```

Each line runs with every check it has alone, under one lock held for the whole batch, against the
queue as the trunk and the lines before it leave it; the first refusal names its line and nothing is
committed or written. `add`, `set`, `submit`, `accept`, `reject`, `done` and `open` are taken, and
each row once per batch — a commit is linted row by row, and submit-then-accept in one commit would
read as accepted, never submitted. The subject names every edit (`chore(tasks): accept #14, accept
#15, reject #16, add #17`), the body holds each edit's own message, and `lint` and `audit` read it
as that many edits; `lint` finds a batch subject that names a row the commit leaves alone, or misses
one it changes. An edit that changes nothing (`set` to the value a row has) is left out. A batch of
one commits under its edit's own message, and a batch whose every edit is a no-op commits nothing
and says so.

Ids need no quoting: `14` and `'#14'` are the same. Filters and fields have shell-safe spellings —
`lane:owner` for `'>owner'`, `level:3` for `'!3'`, `area:x` for `@x` — because an unquoted `>agent`
is a redirect that silently creates a file called `agent`.

## What the gate guarantees

**Every queue change is its own commit on the trunk, and holds nothing else.** The edit is applied to
the trunk's committed file and committed with plumbing — a private index, `commit-tree`, and a
compare-and-swap `update-ref` — then mirrored into the trunk's checkout, wherever that is: its
working file, and its index entry, on top of anything already staged there. A peer's uncommitted
row stays uncommitted; it is never swept into your commit, never makes your command decline, and
the next ordinary `git commit` in that checkout does not revert the queue. An edit whose committed
row already reads so commits nothing but still mirrors into the checkout, fixing a stale working or
staged row (`checkout fixed: #1 matches main`), or moving a peer's staged row in both files
(`checkout updated: #3`). Both working files are written aside (in the git dir, or beside the file
where a rename from there is refused, as across bind mounts) before the commit and renamed into place after it, the archive first, so a moved row is never in neither: one that
cannot be written (a directory in its place) refuses with nothing committed, and an archive in a
directory the checkout lacks creates it. A queue or archive the trunk tracks as a symlink refuses
queue writes, naming the `git rm` and copy that put the file in its place (or, when the link names a
file that is not a queue, a checkout of `TASKS.md` alone from before the first-parent commit that set the link, so a merge
that took the link from a side branch restores the trunk's own rows; by hand when that commit has no
parent), and lint flags a commit
that makes either a symlink or points one elsewhere: a retargeted link would aim
queue writes at another file. A commit that turns such a link back into a file is linted against the
file as the first-parent history last held it, not the link's empty reading, so a restore that drops
rows is flagged. A link only the trunk checkout holds refuses too, naming where it points
(rows edited through it live there) and the `rm` and `git checkout` that restore the file. Every
such fix is `git -C <checkout>` with absolute paths, so typed in a worktree it still lands in the
trunk checkout; and `5w ci --branch` reports a queue or archive that is a
symlink on the trunk rather than reading the link as an empty queue. A temporary file or private index a killed run left in the
git dir (`5w-write-<pid>-<n>-<file>`, `5w-index-<pid>`) is removed by the next write, under the lock. If the checkout cannot take a change the trunk already has
(its `index.lock` held), the refusal names the rows committed and a fix that applies that commit's
own diff to the working files and index, keeping anything else in them (`--3way` where a hand
edit is in the way); both index entries change in one `update-index`, so a failure leaves neither
moved and that diff applies. Left so, the next write usually catches the checkout up first (`checkout caught up:
#2`): where its index entries are a recent trunk commit's files exactly, the index takes the
trunk's, and each working row changed since takes the trunk's version unless edited there by
hand. Not after a repo's first archive: the checkout's queue still holds the rows the trunk's new
archive has, so every command refuses on duplicate ids before a write can run, and that refusal
names the same diff to apply. Its index lacks the archive too, so an ordinary `git commit` there
would take the archive off the trunk (after a later archive, its new rows): `5w doctor` names that
diff as well, and `lint` (the pre-commit hook, a push) refuses the closed rows it would move back into
the queue. The hook cannot see the commit's subject yet, and a missed archive stages exactly what a
deliberate unarchive does, so a commit the checkout's index missed leaves a marker,
`5w-missed-<trunk>` in the git dir, holding the trunk's tip before and after: the hook reads an
archived row back in the queue as the missed commit, naming the diff, only while the marker stands
for the checkout — while a row those commits changed or moved reads, in its staged queue and
archive, otherwise than the trunk has it. Other staged edits do not matter: a row added by hand
over a missed archive is still refused. Once the checkout is fixed by hand (that diff applied, or
the files restored), the marker is left over and a deliberate unarchive staged after passes; its
limit is a row the missed commits archived themselves, which reads as the miss while the marker
stands (`5w lint --staged` before staging that unarchive clears it). A write that catches the checkout up or updates its index removes it, and so does the hook
(`5w lint --staged`) once it no longer stands; `5w doctor` names the diff while it stands and notes
one left over, removing nothing. While the queue or archive is in conflict in the trunk checkout
(unmerged, mid-merge), writes refuse until it is resolved and `git add`ed: one would collapse the
conflict to a single entry and leave the markers in the file. `5w doctor` names such a conflict, a git conflict
marker line in either file (outside ``` fences), and an id repeated within the archive as well as
within the queue — duplicates in the archive stop reads just as the queue's do. Writers take a lock, so
parallel `add`s mint distinct ids, and state checks read the committed queue under that lock — a
hand-edited `[~]` in a working copy does not make a task acceptable. A command that names an
uncommitted row commits it; `add` mints past uncommitted rows, so when a higher id is committed
by then, that command refuses rather than commit a reused id, and names the id to renumber it to.

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
added at the reviewed commit — whitespace, modes and bytes included, only blob ids and hunk line
numbers normalised away, and read with plumbing and every diff setting pinned, so a textconv
driver, a `-diff` attribute, `diff.context` or `diff.ignoreSubmodules` cannot hide a change. Not
`git patch-id`, which ignores whitespace and would pass an indentation change made after review. A
line number gives way to a position among identical copies: how many copies of the hunk's lines (its
context and removed lines) start above it in the file it applies to. In a file that holds the same
lines twice, the reviewed hunk moved to the other copy is a different change, while a rebase that
only shifts lines keeps it. The count is taken in each diff's own pre-image, so it pins the copy's
number, not the block: when the trunk has since added a copy above, the change in the same-numbered
copy — which may be the new one, where `git rebase` can put it — still reads as the reviewed one.
A clean rebase passes. A commit added after review, or a rebase that changed the lines next to the
change, does not, and a refused `--sync` puts the branch
back where it was. The check runs before anything rewrites the branch, and again after the rebase. A
stacked branch was reviewed on top of its parent; once the parent has landed, the change compared is
what the branch added on top of the parent's reviewed commit — allowed only when that parent's
branch is gone and the trunk holds the parent's version of every path the parent changed.

**With `gate_trunk`, code reaches the trunk only as a recorded landing of an accepted change.** What
it proves is that each landing has an accepted row in the queue and matches that row's reviewed
change on the server — not that anyone but the pusher reviewed it: whoever may push queue accepts
can accept their own change. Ship's check runs where
ship runs; a plain `git push origin main` of commits nobody shipped skips it. So with `gate_trunk =
true`, ship follows each fast-forward with a *landing record*, an empty commit on the trunk:

```
chore(tasks): land #11

Landed: <trunk before>..<what landed>
Change: <the change id ship compared>
```

and `5w ci --ref refs/heads/<trunk>` — the pre-receive hook, a push job — reports every commit the
push brings to the trunk that no record in the push covers. The record is where to look, not
evidence: the server recomputes everything from the pushed objects, and `Change:` is for a reader.
A record covers every commit in its `Landed:` range only when its one parent is the range's end and
its tree that parent's (it adds nothing), the range's start is an ancestor of its end, every commit
in the range is new in this push (a start reaching back over what the trunk already has could make
"the trunk plus #1" out of taking a later landing back out), the queue at
the record holds its task `[x] via:review` with a `reviewed:` commit the server has, and the range
adds exactly what that commit added over the range's start — or, for a stacked branch, over a
landed parent's reviewed commit, by the rule above (the parent's branch may still be on the
server). So a record naming another task, a wider or narrower range, or a change no accepted row
records covers nothing; a hand-written one (`git commit --allow-empty`), for a landing made without ship,
passes on the same terms. `--sync` and `--squash` change nothing here: the range is what landed,
compared with the review as ship compared it, and several ships pushed at once are several records.
Rebasing the trunk over a record (`git pull --rebase`) breaks it, since its range names the old
commits; merge instead, or ship again.

- **Needs no record:** a commit that changes only the queue and archive, or nothing; a two-parent
  merge whose tree is the clean merge of its parents (`git merge-tree`) outside those files. The
  commits a merge brings in are judged themselves, under the merge's setting.
- **Which commits:** every one a push brings when the trunk it moves has `gate_trunk = true`, and
  in a push that enables it, those whose first parent's `.5w.toml` has it (a config that does not
  parse is read as the newest one on its first-parent line that does, and as on when none does; one
  the config rejects counts as on unless it says `gate_trunk = false`). History from before the
  enabling commit, and that commit, pass as they are;
  a branch from before it is still new to the trunk, and turning the gate off is a gated change.
- **The reviewed commit must be on the server.** Shipped as reviewed, it is what landed. After
  `--sync` or `--squash` it is not on the trunk: keep its branch pushed, or push it with the trunk
  (`git push origin main <sha>:refs/5w/reviewed/11`) — ship names it.
- **Only a `via:review` row lands under the gate.** `--force`, or a task closed without review,
  records no landing, and the push is refused.
- **The trunk cannot be deleted or rewound** under the gate: a push re-creating it would have no
  trunk to be judged against, and a force push to an older commit judges nothing, drops landings the
  server has, and can reset to before the gate was on. A trunk update whose old tip is not below the
  new one is refused when the old tip has `gate_trunk` on (read as above). Turn
  the gate off through a shipped change first.
- **What it does not check.** Who reviewed: the accept may arrive in the same push as the landing,
  and who may push one is the queue's own trust (see *Forge events*). A forge's merge button records no landing; its
  push job goes red after the fact, which only a server hook prevents. Coverage is of the range's
  net change, as ship's check is, and one accepted change may land more than once.

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

**`--sync` replays only the branch's own commits.** A child of a parent that landed by `--squash`,
or was rebased as it shipped, still holds the parent's old commits, which are not on the trunk;
replaying them onto the trunk that has their change conflicts with itself. So when an accepted
parent has landed (by the rule above: its branch gone, the trunk holding its version of every path
it changed), `--sync` runs `git rebase --onto <trunk> <parent's reviewed commit>`. The reviewed
commit is the parent's last known tip that the queue itself records, and the one the gate compares
the child against; with no such parent the rebase is a plain `git rebase <trunk>`. The gate runs
after the rebase as before, so cutting away a parent can never land less, or more, than was reviewed.

`--force` overrides the review gate only, never a safety check.

## Without the tool

[PROTOCOL.md](PROTOCOL.md) is the spec the binary implements, written as instructions: the row format,
each edit and what it must carry, and the commit rules. `5w init` copies it into the repo, so a
contributor or agent without `5w` edits `TASKS.md` by hand and gets the same result.

Hand edits are checked, not trusted:

- **`5w lint`** compares the queue before and after — `--staged`, one commit, or a range — and judges
  every row that changed by its transition: `[ ]→[~]` carries `branch:` and `submitted:`,
  `[~]→[x]` carries `via:review` and `reviewed:`, a close carries its lane's `via:`, a reject its
  `rework:`, and nothing else gains one. A `submitted:` or `reviewed:` written or changed is a full sha
  (a 12-digit prefix passes only in a submit or accept commit, as releases through 0.1.3 wrote them);
  a prefix is read only while it names exactly one commit, and one under 7 digits never. Closed rows are immutable except to reopen, reflow (`split`)
  or archive, and an archived row moves back to the queue only unchanged, in a commit whose subject
  names each (`chore(tasks): unarchive #4`, or several joined by `, `) and that changes no row — how
  an archived row is reopened, which is what `reopen` of one names; no row is deleted, no id reused and none in both files; a queue edit is its own commit on the trunk. A
  merge is judged by the tree it makes against its first parent (the trunk's side). A row it holds as
  another parent does passes when the first parent left that row as at one of their merge bases (all
  of them: a criss-cross history has several) — a feature merged into the trunk carries the trunk's
  queue, the trunk merged into a feature brings rows the feature never touched. A row both sides
  changed is a resolved conflict, merged field by field against a merge base: a field neither side
  changed keeps the base's value, one a single side changed takes that side's, one both changed takes
  either's. The result is then judged as a change from the parent whose state it keeps (the first
  parent's when both), so a closed row gains no field and an open one no `via:`. Taking a row whole
  from the side discards the trunk's edit and is refused — except a close, which wins over edits the
  other side made while the row was open. The first parent's row whole always passes: it is no change
  to the trunk, as `lint --staged` sees it. A merge
  trusts what its sides' own commits did: a range lint judges those inside it, and a hand edit on a
  side outside the linted range goes unseen. A row no parent holds is an add, judged as one — two clones that both added an id renumber one past
  both sides' highest. Any other change (a row dropped, a row neither side changed edited in the
  resolution) is judged against the first parent and flagged as a queue edit inside a merge.
  `lint --staged` judges a merge being committed the same way, so the
  pre-commit hook and a range lint in CI agree. A
  single edit's subject (`chore(tasks): set #4 level 1`) names the only row its commit may change, and
  a batch's names exactly the rows it changes; a line rewritten to read the same is no change. Every
  commit `5w` itself makes passes it — a `reject` commit may gain `rework:` from any state, as
  releases through 0.1.3 rejected unsubmitted tasks, and a `set` to the value a row has (which now
  commits nothing, and says so, unless it moves the row to its lane's section or fixes the checkout's copy) may rewrite its row's line — and the test suite lints its own history.
- **`5w hook install`** (also run by `5w wt setup`) adds a pre-commit hook running `5w lint --staged`.
  Where `5w` is not installed the hook lets the commit through with a warning to follow
  PROTOCOL.md; `5w lint <range>` catches what that let through, later.
- **A hook's environment.** git runs a hook with `GIT_DIR` set in a linked worktree and
  `GIT_INDEX_FILE` set for every commit — for `git commit -a` or `git commit <path>` a temporary
  index holding what is being committed. `lint --staged` reads that index. Every other git call 5w
  makes names its directory and clears `GIT_DIR`, `GIT_WORK_TREE`, `GIT_COMMON_DIR`,
  `GIT_INDEX_FILE` and `GIT_NAMESPACE`, so a call in another worktree reads that worktree, not the
  hook's; `GIT_OBJECT_DIRECTORY` and `GIT_ALTERNATE_OBJECT_DIRECTORIES` are kept only inside a
  pre-receive quarantine, where the pushed objects are. A `GIT_DIR`, `GIT_WORK_TREE` or
  `GIT_COMMON_DIR` naming another repository than the one 5w runs in is refused.

A hook is a convenience, not a gate: `--no-verify` skips it. When adopting 5W on an existing queue,
lint from the adoption commit onward — earlier rows predate `submitted:` and `reviewed:`.

## CI and servers

One command holds every check, and takes everything as arguments — no forge's variables are read, so
the same command runs under any CI, in a server hook, and by hand:

```
5w ci --base <old> --head <new> --ref refs/heads/<name>   # a push
5w ci --base <old> --head <new> --branch <name>           # a change request into the trunk
```

- **A push to the trunk:** every commit in the range is linted as landing on the trunk, and under
  `gate_trunk` each one that adds code is covered by a landing record (*What the gate guarantees*).
  A new tip whose `.5w.toml` the config rejects is refused, naming its line and error (a `requires`
  newer than the checking 5w: upgrade that 5w): landed, it would make every later push fail.
- **A push to any other branch:** its commits carry no queue edits. Commits the trunk already holds —
  brought in by `git merge <trunk>` — are the trunk's and are not judged again. The trunk they are
  judged against is the server's before the push: a hook cannot know whether git will apply a trunk
  update in the same push (a non-fast-forward, an `update` hook), so push the trunk first, then a
  branch that merged its new tip. That holds for a new server too: `git push origin main feat` into
  an empty repository judges `feat` against no trunk, and git can still refuse the trunk's creation
  after the hook (an `update` hook), so the trunk's queue commits read as
  `feat`'s and the push is refused, naming the fix — push `main`, then the branches.
- **A change request:** no queue edits, plus the ship check — an accepted task names the branch, and
  what the branch adds is exactly what was reviewed (a clean rebase passes). The check is red until
  the task is accepted; re-run it after `5w accept`.
- **A plain range** (neither `--ref` nor `--branch`): every commit in it carries no queue edits,
  whether or not the trunk holds it.

A missing or all-zero `--base` means the merge-base with the trunk. The checkout needs full history.
`--head`, `--base` and `--trunk` must each name one commit: a negation such as `^HEAD` or a range is
refused, not read as an empty range that passes having checked nothing, or as a trunk with an empty
queue that passes an unreviewed branch (`accept --at` and `audit --since` refuse it too). Without
`--trunk`, the trunk is `refs/heads/<trunk>`, else `refs/remotes/origin/<trunk>`.

| Where                                                                           | How                                                                                                                                                                                                                                  |
| ------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Any git server you run — bare repo over SSH, Gitea, Forgejo, self-hosted GitLab | `5w hook install pre-receive` in the bare repo ([ci/pre-receive](ci/pre-receive)). A real gate: bad pushes are refused. Without `5w` on the server it refuses rather than waves through (`git config 5w.allowMissing true` to relax) |
| GitHub, Forgejo, Gitea Actions                                                  | [ci/github-actions.yml](ci/github-actions.yml)                                                                                                                                                                                       |
| GitLab CI                                                                       | [ci/gitlab-ci.yml](ci/gitlab-ci.yml)                                                                                                                                                                                                 |
| Anything else                                                                   | map its before/after SHAs, ref and change-request branch onto the flags                                                                                                                                                              |

**Which branch is the trunk on a server.** `FIVEW_TRUNK`, else `git config 5w.trunk`, else the
branch the bare repository's `HEAD` names (then `git-town.main-branch`, then `main`); `.5w.toml`'s
`trunk` read from that branch has the last word, except over a pin (`FIVEW_TRUNK` or `5w.trunk`):
a committed rename must not move the gate off the branch the server guards. A push to the pinned
trunk whose `.5w.toml` names another trunk is refused, naming both and the pin to change
(`FIVEW_TRUNK` when that is the pin, since `git config` cannot outrank it) — renaming the trunk on a
server is its admin's step. `5w hook install pre-receive` records `5w.trunk` from `HEAD` when it is unset —
set it by hand if the trunk is renamed. `HEAD` itself is no pin (a stale one would refuse every push):
while `gate_trunk` is on and nothing pins the trunk, every push warns, naming `git config 5w.trunk
<HEAD's branch>`, and a push to `HEAD`'s branch whose `.5w.toml` renames the trunk away from it is
refused, landed or not. A server with branches but no
branch of that name refuses every push to another branch, naming `git config 5w.trunk <name>`: a
wrong guess would judge no push as landing on the trunk. A pushed ref that is a symbolic ref (an
alias left by a rename) is judged as the branch it points at.

**A trunk whose `.5w.toml` broke** (landed with the hook off, or from before this check) does not
lock the server: `5w ci` refuses every push naming the error and the fix — push a commit that fixes
`.5w.toml` to the trunk, or upgrade the server's 5w when the config `requires` a newer one. A push to
the trunk whose new tip commits a config that parses (or none) is the repair, judged under that
config — except the queue file, archive and commit prefix, which the gate tells queue edits and
landings by: those come from the broken config where it says them, so a repair cannot call its
code the queue. Broken past parsing, it says nothing: they and `gate_trunk` come from the newest
config on the trunk's first-parent line that parses (defaults, gated, when none does), so a trunk
gated before the break stays gated. A config the parser reads but rejects counts as gated unless it
says `gate_trunk = false`. Under the gate the repair needs a landing record like any code. A
broken config naming a trunk an unpinned server has no branch for names the pin instead:
`git config 5w.trunk <HEAD's branch>`, then the repair. A server cloned after the break still installs
the hook: `5w hook install pre-receive` notes the broken config and that only the repair will be
accepted (`5w hook install` in a checkout still refuses — fix the file there). A checkout's
pre-commit hook takes the repair commit the same way: while the trunk's `.5w.toml` does not parse,
`5w lint --staged` refuses a commit whose staged `.5w.toml` does not parse either, naming the fix,
and judges one whose staged config parses (or is absent) under it — so the repair can be committed on
a branch in its worktree. It must keep the queue file, archive and commit prefix the broken config
gives (rename in a later commit), and its queue edits are checked as any.

A forge's check is judged the same way: `5w ci --branch` on a change request whose head commits a
config that parses (or none) runs the ship check under that config, with the trunk's queue file,
archive and commit prefix as above, and `require_task` read from the trunk like `gate_trunk` (on,
unless the trunk's config says `false`) — so the change request that repairs the config can pass
its required check, and one that renames the queue or drops `require_task` gains nothing. Any
other `--branch` is refused naming the fix: fix `.5w.toml` on the branch, or on the trunk. `--event
submit|accept` stays refused until the trunk is repaired: an event writes the queue with every
setting but those few at its default, and a server whose `5w ci` guards the trunk refuses the queue
commit anyway, as a push that does not repair it.

The wrappers install exactly the version the project pins (see *Staying current*), verified —
[ci/install-5w.sh](ci/install-5w.sh), inlined.

### Forge events

A change request can drive the queue itself: opening it submits its task, an approval accepts it.
A job maps its forge's events onto two generic transitions:

```
5w ci --event submit --branch <name> --head <tip>                 # opened, or pushed to
5w ci --event accept --branch <name> --head <tip> --at <reviewed> # approved
```

The task is the one unclosed row naming the branch; `--task <id>` names one when no row does, and is
refused when that row names another branch. Each event is the ordinary `submit` or `accept`, with
every check, committed to `refs/heads/<trunk>` — so the job fetches the trunk as a local branch
first (`git fetch origin +main:main`) and **pushes it back itself**: 5w never pushes.

- **What is accepted** is `--at`, the approved commit, and only when it is `--head`, the change
  request's tip now, and contains the submitted commit. A push after the approval needs its own. The
  ship check (`5w ci --branch`) then passes on that change and nothing else.
- **Re-runs are no-ops.** Submitting at the recorded commit, accepting at the recorded reviewed
  commit, or either on a closed task prints one line and exits 0. A push after submit leaves the row
  as it is; review reads the drift, and the approval names the commit. A re-run at a head that was
  rejected does not resubmit it; a new commit does. A branch no task names is a note, not a failure
  — the change request's check fails it under `require_task`.
- **Races.** Events in one clone are serialised by the queue lock and a compare-and-swap; jobs in
  separate clones race at the push, and the second is refused as non-fast-forward. Fetch the trunk
  again, dropping the local queue commit, and re-run the events; the wrappers retry three times.
- **Signing.** A queue commit is signed when the runner's git config asks for it (`commit.gpgsign`,
  `user.signingkey`). A runner usually has no key, so event commits are unsigned; where the trunk
  requires signed commits, give the job a key or leave events to people.

**Trust rules.** 5w cannot tell who approved or who runs it; the wiring around it must.

1. **The job that edits the queue never runs code from the change request.** A forge runs a change
   request's pipeline from the change request's own copy of the CI file, which its author can edit to
   accept anything. So the check on the change request stays read-only, and submit and accept run
   in a separate job defined on the default branch. That job checks out nothing from the change
   request and reads none of its files — not its `.5w.toml`, not its `requires`: its 5w version is
   pinned in the job (`FIVEW_VERSION`, which `ci/install-5w.sh` also honours) and must be a release
   that has `ci --event` — the templates name 0.1.4, the first one planned to; check before enabling.
   The change request's commits are fetched as objects only.
2. **Whatever wakes the job is not evidence.** It reads the change request again from the forge's
   API: open, into the trunk, from the repository itself (not a fork), and its current head. An
   approval counts only if it is of that head, and from someone with write access by the API's
   permission check (not a display field such as `author_association`) who neither opened the change
   request nor authored or committed any of its commits. Any outstanding "changes requested" blocks
   it.
3. **The push credential belongs to that job alone.** A token allowed to push the trunk lives in an
   environment only that job uses, restricted to the default branch. Never give the forge's general
   job token (`GITHUB_TOKEN`, `CI_JOB_TOKEN`) push to the trunk: every writer's pipeline holds it.
   Each token reaches only the git command that needs it, through its environment — never its
   arguments, and never stored in the clone. Nobody but owners may set pipeline variables: one that
   overrides the API or server address would send the token elsewhere.
4. **Forks are done by hand**, and a fork's pipeline must never run with the parent's secrets.

The wiring follows them:

- **GitHub:** [ci/github-actions.yml](ci/github-actions.yml) is the read-only check
  (`contents: read`). [ci/github-queue-events.yml](ci/github-queue-events.yml) runs from the default
  branch on `workflow_run` of it (the change request into the default branch, when the run lists
  several), reads the reviews, the commits and the approver's permission from the API, fetches with
  the read-only `GITHUB_TOKEN` (so a private repository works), pushes with `FIVEW_PUSH_TOKEN` (a
  GitHub App or fine-grained token) from the `5w-queue` environment, and re-runs the check after an
  accept. Its author and committer check cannot see past two limits, so it does not accept then
  (accept by hand): a commit whose author or committer is no GitHub account has no login to exclude,
  and the API lists at most 250 commits of a change request.
- **GitLab:** the `5w` job in [ci/gitlab-ci.yml](ci/gitlab-ci.yml) is the read-only check. The
  `5w:queue-events` job runs on a schedule on the default branch, reads merge requests and approvals
  from the API, and pushes with `FIVEW_QUEUE_TOKEN` from the protected `5w-queue` environment. GitLab
  records who approved but not which commit, so it refuses while approvals survive a push, and counts
  an approval only if given after the current head last arrived (the merge request's latest version
  with that head, so a force-push back to an earlier head does not revive an older approval). It
  also reads each reviewer's state, since `detailed_merge_status` names only the first failing
  check: a reviewer's changes requested, or reviewers it cannot read, stop an accept. Set
  "Minimum role to use pipeline variables" to Owner or no one: a pipeline or schedule variable
  overriding `CI_API_V4_URL` or `CI_SERVER_URL` would redirect the token. Commit authors are matched
  by the emails GitLab shows the token, so that check is only as good as those; an approver whose
  user record cannot be read, or who has no email the token can see, is not counted. A project
  access token sees only public emails: approvers need one, or the job an administrator's token.
  Every list the job reads is read to its last page, and a failed page, a page that is not a JSON
  array or a list longer than 100 pages fails the job.

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

- **`5w archive`** moves closed tasks to `DONE.md` in one commit — the tasks closed on the trunk; a
  row closed only in the checkout moves there uncommitted, in the staged copy too (a `DONE.md` the
  trunk lacks is staged new with just those rows; an untracked one with more than archive's header
  is refused). A row closed on the trunk but open in the checkout (reopened by hand) is not
  archived, and a note names it; one the checkout deleted by hand still lands in its `DONE.md` as the
  trunk has it. On a 900-task queue that took `TASKS.md` from 649 KB to 52 KB.
  Archived ids stay taken, still satisfy `needs:`, still show under `5w show`, and ship still reads
  their `branch:` and `reviewed:`. `5w` edits no archived row — not even where the checkout's
  `TASKS.md` still holds it; a reopen or edit of one says to move its block back by hand.
- **Titles are short; detail is body.** Text over `title_max` (120) is split at the first sentence
  into a title and an indented body, by `add` and, for existing rows, by `5w split`. Lists print
  titles; `show`, `next` and `delegate` print bodies.
- **Output is compact off a terminal**, or with `FIVEW_AGENT=1`: one line per task
  (`#14 !3 @faces title + [branch] >lane — note`, `+` marking a body; a title still over `title_max`
  ends `…(+53 chars: 5w show 14)`), no headers, one-line
  refusals naming the fix (a usage mistake points at `5w <cmd> --help` rather than printing the
  usage; git's own stderr is folded onto the line). `FIVEW_AGENT=0` or `--full` gives the human layout. `ready`'s summary
  line ends with the next step (`→ 5w delegate 14`) and `review` closes with `→ 5w accept 14`, as
  `next` does; `--json` and `--ids` carry no hint.
- **`--json`**, **`--ids`**, **`--limit N`** on every list; **`5w next [filters]`** returns just the
  first ready task with its body. A list's `--json` prints one object per line with what its text
  row shows — `id`, `state`, `level`, `area`, `title`, `branch`, `unmet` (what blocks it) and
  `rework` (null when absent); `--full` adds `lane`, `kind` and `needs`. Changed after 0.1.3: the
  default row carried every field, so a reader of `lane`, `kind` or `needs` now passes `--full`.
  `show --json` and `next --json` print one task, so always every field, with `body`, `via`,
  `submitted`, `reviewed` and `archived`. `review --json` adds each submitted task's
  `tip`, `moved` (commits since submit), `diff` (shortstat against the trunk) and `behind` to the
  short row, and to every field with `--full`.
- **A flag a command does not take is refused** (`unknown flag --jsn for ready (5w ready --help)`),
  not ignored: an agent that invents a flag learns so, instead of trusting output the flag never
  shaped. So are `wt`, `report`, `init` and `lint` (`unknown flag --x for wt ls (5w wt ls --help)`;
  `lint` also refuses `-x`, a second argument and a revision that is not a commit, such as `^HEAD`,
  since it takes one of `--staged`, `<rev>` or a range;
  `wt` and `report` subcommands print their usage for `--help`). `done`'s flags are the lanes' close words; text (`reject`'s reason, `--body`) may still
  start with `--`.
- **Colour only for a person:** styling is off when stdout is not a terminal, under `NO_COLOR` or
  `TERM=dumb`, and with `--no-color`. That flag is global — any command, any position — except where
  it is text: a `--body`, `--expected` or `-m` value, `reject`'s reason and `report`'s text once
  they begin, and anything after `--`.
  A refusal (`5w: <error>`) is red by the same rules, judged on stderr: coloured only when stderr is
  a terminal, byte-identical otherwise.
- **The brief is the worker's only document.** `delegate` prints the task, the rework note, the
  steps and the rules, and names the sections the text cites (`refs: client/FINDINGS.md #779`) so the
  worker reads those rather than whole files. `review` prints its checklist once, on
  `--checklist`.
- **Help is per command.** `5w <command> --help` prints that command's entry from the listing (and
  the `filters`, `out` or `ids` line it refers to), not the whole listing; bare `5w --help` prints
  all of it, ending with a `docs` line linking this README on the web (the `repository` field in
  `Cargo.toml`, so a fork's build links the fork). `wt`, `ship`, `lint`, `ci`, `report` and `audit` print their own usage.

### Measuring output

`tests/bench.rs` prices what every read costs. It generates queues of 10, 100 and 1000 tasks — open,
submitted and blocked tasks, bodies, rework notes, every lane kind, half as many again archived —
runs `ready`, `next`, `ls`, `all`, `review` (each with and without `--json`), `show`, `delegate`,
`doctor`, `lint` and four common refusals off a terminal, and records stdout+stderr bytes, estimated
tokens and wall time:

```
tasks  case                          bytes  ~tokens  exact     ms
 1000  ready                         40884    10642      -   10.5
 1000  ready --json                  89611    24897      -   10.9
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
a fixed one. Shas are still priced as a fixed stand-in of the same length: any unrelated commit (a
change to the protocol template `init` commits) moves every later sha, and a sha's estimated tokens
depend on its digits.

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
| `outside` | queue commits 5W does not make — a message it never writes, a subject naming other rows than it changes, or other files in the same commit — with lint's findings on them | hand edits, each judged against PROTOCOL.md. 5W's own commits pass lint by construction and are not linted again |
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

Worktrees go in `worktrees.root` (default `../<repo>-wt`, or `$FIVEW_WT_ROOT`, ignored when empty); a relative root,
from either, is read from the primary checkout whatever directory 5w runs in (a subdirectory, another
worktree), and its `..` is resolved as written (symlinks are kept) so every path 5w prints is clean. `.worktree-links`
lists gitignored paths or globs to symlink in from the primary — `.env`, a dev database, large
samples. **Links are shared, not copied**; a branch that must change one copies it. Globs expand
file by file so a directory with tracked content is never shadowed. `node_modules` is never linked;
set `worktrees.install` to run a real install per worktree.

Stacking records the parent in git-town's own config keys, so `git town sync` and friends work on
these branches when git-town is installed, and nothing requires it when it is not. `5w wt setup`
configures both. `wt new` records the branch checked out where you run it, so stack from inside the
parent's worktree (or name it with `--from`). A branch made from the trunk and then moved onto
another branch's work by hand still records the trunk, and ship would take it for the bottom of its
stack: `5w wt ls` and `5w doctor` note each branch whose commits include another unshipped branch's
tip while its recorded parent is the trunk, with the `git config git-town-branch.<b>.parent` that
fixes it. Only branches with a recorded parent count on either side, so a backup made with
`git branch` is never named, and a branch recorded as stacked on the flagged one is its child.

`5w wt prune` lists the branches safe to drop, with their worktrees: no commits past the recorded
parent (the trunk when none is recorded), no task in the queue or archive naming the branch — nor
suggesting it: a task not closed keeps any `<area>/task-<id>`, which a worker creates long before
submit records it, even after `5w set <id> area` changed the suggestion — and a worktree, if there is one, under `worktrees.root` that is clean, unlocked and
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
that keeps its own section, a custom brief footer and a review checklist. A key given twice in the
same table reads as its last, everywhere: the config, the version check, the gate, `self-update`
and the CI install script; `update-files --pin` rewrites that last `requires` and drops the others.

| Key                                         |                                                                                       |
| ------------------------------------------- | ------------------------------------------------------------------------------------- |
| `trunk`, `file`, `archive`, `commit_prefix` | where the queue and its archive live, how commits read                                |
| `title_max`                                 | longest task line text before it is split into title and body (0: off)                |
| `perennial`                                 | branches never shipped, rebased or deleted (git-town's list is honoured too)          |
| `require_task`                              | refuse to ship a branch no task names                                                 |
| `gate_trunk`                                | ship records each landing; a trunk push's code needs a record matching a review       |
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
repository should not call out on its own. Asking is explicit:

- **`5w version --latest`** prints this binary's version and the newest release's, and changes
  nothing (`5w version` alone is `5w --version`).
- **`5w self-update`** installs the version the project pins (`requires` in the `.5w.toml` of the
  worktree you stand in) when this binary is older — it runs even where that pin makes every other
  command refuse. `--latest` takes the newest release instead. It downloads the binary for this
  machine, `SHA256SUMS` and `SHA256SUMS.asc`, and installs only if all of these hold:
  - `SHA256SUMS.asc` is exactly one good signature, not expired or revoked, over `SHA256SUMS`, by
    the release key built into the binary ([SIGNING_KEY.asc](SIGNING_KEY.asc)): `gpg` checks it in
    a keyring holding nothing else, and the signing subkey's fingerprint must be
    `1125DC32ECA09CA21A1810DE3491A839212CC7DB`, as [ci/install-5w.sh](ci/install-5w.sh) pins. A
    release not signed yet is not installed.
  - `SHA256SUMS` is exactly what `ci/release.sh` writes — every line
    `<64 hex>  5w-<version>-<arch>-unknown-linux-musl`, newline-terminated, one version (the one
    asked for), no name twice — or the whole file is refused. The same key signs the maintainer's
    commits, so a signature alone does not make a text a release: a signed commit carrying a sums
    line in its message is refused here.
  - the binary's SHA-256 matches its line.

  Only then is the running binary replaced: written beside it, synced, and renamed over it, so an
  interrupted update leaves the old binary whole; any failed check writes nothing. Downloads are
  https only (redirects too), size-capped and time-limited.

  It needs `curl` and `gpg` on PATH and refuses in one line naming the one missing. Releases are
  read from the repository's `/releases` (`Cargo.toml`'s `repository`); `FIVEW_RELEASES_URL` points
  at a mirror laid out the same way (`latest/download/SHA256SUMS`, `download/v<version>/…`).
  `FIVEW_RELEASE_KEY` names another armored public key to trust (its primary key must make the
  signature), and is honoured only when `FIVEW_RELEASES_URL` is a `file://` directory — a local
  mirror or a test — so the environment cannot redirect trust for a network download.

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

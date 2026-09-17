//! ship — land a reviewed branch on the trunk by fast-forward.
//!
//! Every refusal happens before anything irreversible, and names the command
//! that fixes it. Beyond the shell version:
//!
//! - **The reviewed change is the change that lands.** `accept` records the
//!   commit it reviewed; ship compares the patch-id of what the branch adds now
//!   against what it added then. A clean rebase passes; a commit added after the
//!   review does not.
//! - **`--sync` rebases and `--squash` squashes**, the squash built with
//!   `commit-tree` on the branch's own tree, parented on the trunk sha it was just
//!   verified against — so it can never silently revert what the trunk gained.
//! - **No git-town needed, and no cwd rule.** The fast-forward happens in
//!   whichever worktree holds the trunk, or on the ref when none does.

use crate::bail;
use crate::git;
use crate::queue::{self, State};
use crate::store::Repo;
use crate::util::{Res, short};
use crate::wt;
use std::collections::HashSet;

pub const USAGE: &str = "\
usage: 5w ship [<branch> | --accepted] [--sync] [--squash] [-m <message>] [--discard-ignored] [--force]

  --accepted every accepted task's branch, bottom of each stack first; stops at the first refusal
  --sync     rebase the branch's own commits onto the trunk first, in its worktree
  --squash   land one commit (message from -m, or composed from the branch's commits)
  --discard-ignored   delete gitignored files in the branch's worktree with it
  --force    override the review gate only — never the safety checks";

struct Opts {
    sync: bool,
    squash: bool,
    force: bool,
    discard_ignored: bool,
    message: Option<String>,
}

pub fn run(repo: &Repo, args: &[String]) -> Res<()> {
    let mut branch = None;
    let mut accepted = false;
    let mut o = Opts {
        sync: false,
        squash: false,
        force: false,
        discard_ignored: false,
        message: None,
    };
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--sync" => o.sync = true,
            "--squash" => o.squash = true,
            "--force" => o.force = true,
            "--discard-ignored" => o.discard_ignored = true,
            "--accepted" => accepted = true,
            "-m" | "--message" => {
                o.message = Some(args.get(i + 1).ok_or("-m needs a message")?.clone());
                i += 1;
            }
            "-h" | "--help" | "help" => {
                println!("{USAGE}");
                return Ok(());
            }
            a if a.starts_with('-') => return Err(crate::tasks::unknown_flag(repo, "ship", a)),
            a => {
                if branch.is_some() {
                    bail!("one branch at a time (every accepted one: --accepted)");
                }
                branch = Some(a.to_string());
            }
        }
        i += 1;
    }
    if o.message.is_some() && !o.squash {
        bail!("-m only applies with --squash");
    }
    if accepted {
        if branch.is_some() {
            bail!(
                "--accepted ships every accepted branch; drop the branch name, or drop --accepted"
            );
        }
        if o.message.is_some() {
            bail!("-m would give every branch one message; drop it (--squash composes each)");
        }
        if o.force {
            bail!("--accepted ships only accepted work; --force one branch by name");
        }
        return ship_accepted(repo, &o);
    }
    let branch = match branch.or_else(|| git::current_branch(&repo.cwd)) {
        Some(b) => b,
        None => bail!(
            "not on a branch: name the one to ship ({} ship --help)",
            repo.cfg.cmd_tasks
        ),
    };
    ship(repo, &branch, &o)
}

/// Every branch an accepted task names that still exists, parents before their
/// children, each shipped as `ship <branch>` would; the first refusal stops the run.
fn ship_accepted(repo: &Repo, o: &Opts) -> Res<()> {
    let p = &repo.primary;
    let committed = repo.committed()?.unwrap_or_default();
    let archived = repo.committed_file(&repo.cfg.archive)?.unwrap_or_default();
    let mut branches: Vec<String> = Vec::new();
    for t in queue::parse(&committed)
        .into_iter()
        .chain(queue::parse(&archived))
    {
        if t.state != State::Done || t.via.as_deref() != Some("review") {
            continue;
        }
        let Some(b) = t.branch else { continue };
        if b != repo.trunk
            && !branches.contains(&b)
            && !repo.is_perennial(&b)
            && git::branch_exists(p, &b)
        {
            branches.push(b);
        }
    }
    // Depth in the recorded stack: a branch ships after every parent it has.
    let limit = branches.len();
    let depth = |b: &String| {
        let mut n = 0;
        let mut cur = b.clone();
        while let Some(parent) = git::parent_of(p, &cur) {
            if parent == repo.trunk || !git::branch_exists(p, &parent) || n > limit {
                break;
            }
            n += 1;
            cur = parent;
        }
        n
    };
    let mut order: Vec<(usize, String)> = branches.into_iter().map(|b| (depth(&b), b)).collect();
    order.sort_by_key(|(d, _)| *d);
    if order.is_empty() {
        println!("ship: no accepted branch left to ship");
        return Ok(());
    }
    let mut shipped: Vec<String> = Vec::new();
    for (_, b) in &order {
        if let Err(e) = ship(repo, b, o) {
            let done = match shipped.is_empty() {
                true => "none shipped".to_string(),
                false => format!("shipped {}", shipped.join(" ")),
            };
            bail!("stopped at {b}: {e} ({done})");
        }
        shipped.push(b.clone());
    }
    Ok(())
}

fn ship(repo: &Repo, branch: &str, o: &Opts) -> Res<()> {
    let branch = branch.to_string();
    let Opts {
        sync,
        squash,
        force,
        discard_ignored,
        ref message,
    } = *o;
    let p = &repo.primary;
    let trunk = &repo.trunk;
    let tasks = &repo.cfg.cmd_tasks;

    // --- eligibility -----------------------------------------------------------------
    if &branch == trunk {
        bail!("{trunk} is the trunk, not shippable");
    }
    if repo.is_perennial(&branch) {
        bail!("{branch} is perennial — never shipped, rebased or deleted");
    }
    if !git::branch_exists(p, &branch) {
        bail!("no such branch: {branch}");
    }
    if let Some(parent) = git::parent_of(p, &branch)
        && &parent != trunk
        && git::branch_exists(p, &parent)
    {
        bail!(
            "{branch} is stacked on {parent}; ship that first: `{} {parent}`",
            repo.cfg.cmd_ship
        );
    }

    // --- the review gate, read from the trunk's committed queue ------------------------
    let committed = repo.committed()?.ok_or_else(|| {
        format!(
            "{trunk} has no {} — refusing to guess what is reviewed",
            repo.cfg.file
        )
    })?;
    // Archived rows count: an accepted task moved out by `archive` still
    // authorises its branch, and still records what was reviewed.
    let archived = repo.committed_file(&repo.cfg.archive)?.unwrap_or_default();
    let all: Vec<_> = queue::parse(&committed)
        .into_iter()
        .chain(queue::parse(&archived))
        .collect();
    let rows: Vec<_> = all
        .iter()
        .filter(|t| t.branch.as_deref() == Some(branch.as_str()))
        .cloned()
        .collect();
    let mut notices = Vec::new();
    if rows.is_empty() {
        if repo.cfg.require_task && !force {
            bail!(
                "no task names branch:{branch} and require_task is on (--force ships unreviewed)"
            );
        }
        notices.push(format!(
            "ship: no task references {branch} — shipping unreviewed"
        ));
    }
    let unaccepted: Vec<_> = rows.iter().filter(|t| t.state != State::Done).collect();
    if !unaccepted.is_empty() {
        if !force {
            let list: Vec<String> = unaccepted
                .iter()
                .map(|t| format!("#{} [{}]", t.id, t.state.mark()))
                .collect();
            bail!(
                "not accepted: {} — review `git diff {trunk}...{branch}`, then `{tasks} accept <id>` or `reject`",
                list.join("; ")
            );
        }
        notices.push(format!(
            "ship: --force — shipping {} unaccepted task(s)",
            unaccepted.len()
        ));
    }

    // --- preflight ------------------------------------------------------------------
    let branch_wt = git::worktree_of(p, &branch)?;
    if let Some(w) = &branch_wt {
        if git::dirty(w)? {
            bail!(
                "{branch} has uncommitted files in {}; commit or clean them",
                w.display()
            );
        }
        // Removing the worktree deletes its gitignored files too: an extraction's
        // output, a local database. Name them while refusing is still free.
        let doomed = ignored_files(repo, w)?;
        if !doomed.is_empty() && !discard_ignored {
            let more = if doomed.len() > 5 {
                format!(" (+{} more)", doomed.len() - 5)
            } else {
                String::new()
            };
            bail!(
                "removing {} would delete ignored files: {}{more} — move them, add to worktrees.disposable, or --discard-ignored",
                w.display(),
                doomed
                    .iter()
                    .take(5)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    }
    let trunk_wt = git::worktree_of(p, trunk)?;
    if let Some(w) = &trunk_wt {
        // Uncommitted queue rows are an expected state there, not a blocker; git
        // itself refuses the fast-forward if the branch touches that file.
        let dirt = git::git(w, &["status", "--porcelain", "--untracked-files=no"])?;
        if dirt
            .lines()
            .any(|l| l.get(3..) != Some(repo.cfg.file.as_str()))
        {
            bail!(
                "{} has uncommitted tracked changes; commit or stash before {trunk} can fast-forward",
                w.display()
            );
        }
    }

    // Before anything rewrites the branch, and again after a rebase has.
    let behind = !git::ok(p, &["merge-base", "--is-ancestor", trunk, &branch]);
    let mut authorised = verify_reviewed(
        repo,
        &all,
        &rows,
        &branch,
        force,
        behind && sync,
        &mut notices,
    )?;

    if behind {
        if !sync {
            bail!(
                "{branch} is behind {trunk}: `{} {branch} --sync`",
                repo.cfg.cmd_ship
            );
        }
        let Some(w) = &branch_wt else {
            bail!(
                "--sync needs a worktree for {branch}: `{} add {branch}`",
                repo.cfg.cmd_wt
            )
        };
        let before = git::rev(p, &format!("refs/heads/{branch}")).ok_or("cannot resolve branch")?;
        let o = match sync_upstream(repo, &all, &before)? {
            Some(up) => {
                println!(
                    "ship: rebasing {branch}'s own commits ({}..) onto {trunk}",
                    short(&up)
                );
                git::raw(w, &["rebase", "--onto", trunk, &up], &[], None)?
            }
            None => {
                println!("ship: rebasing {branch} onto {trunk}");
                git::raw(w, &["rebase", trunk], &[], None)?
            }
        };
        if !o.ok {
            let _ = git::raw(w, &["rebase", "--abort"], &[], None);
            bail!(
                "rebase onto {trunk} conflicts (aborted); rebase by hand in {}, then re-review",
                w.display()
            );
        }
        match verify_reviewed(repo, &all, &rows, &branch, force, false, &mut notices) {
            Ok(a) => authorised = a,
            Err(e) => {
                // The rebase bought nothing; do not leave the branch rewritten.
                let _ = git::raw(w, &["reset", "--hard", "--quiet", &before], &[], None);
                bail!(
                    "{e}; {branch} is back at {} as it was before the rebase",
                    short(&before)
                );
            }
        }
    }

    let tip = git::rev(p, &format!("refs/heads/{branch}")).ok_or("cannot resolve branch")?;
    let trunk_sha = git::rev(p, &format!("refs/heads/{trunk}")).ok_or("cannot resolve trunk")?;

    // --- squash: build the commit, move nothing yet --------------------------------------
    let mut land = tip.clone();
    if squash {
        let count: usize = git::git(p, &["rev-list", "--count", &format!("{trunk_sha}..{tip}")])?
            .parse()
            .unwrap_or(0);
        if count > 1 {
            // Parent the squash on the exact trunk commit the branch was verified
            // against, by sha. Squashing onto `main` by name after main moved is
            // how a squash silently reverts what main gained.
            if git::git(p, &["merge-base", &trunk_sha, &tip])? != trunk_sha {
                bail!("{trunk} moved during the ship; run it again");
            }
            let msg = match &message {
                Some(m) => m.clone(),
                None => compose(p, &trunk_sha, &tip)?,
            };
            let first = git::git(
                p,
                &["rev-list", "--reverse", &format!("{trunk_sha}..{tip}")],
            )?;
            let first = first.lines().next().unwrap_or(&tip).to_string();
            let who = git::git(p, &["log", "-1", "--format=%an%n%ae%n%aI", &first])?;
            let mut who = who.lines();
            let (an, ae, ad) = (
                who.next().unwrap_or(""),
                who.next().unwrap_or(""),
                who.next().unwrap_or(""),
            );
            let tree = git::git(p, &["rev-parse", &format!("{tip}^{{tree}}")])?;
            land = git::commit_tree(
                p,
                &tree,
                &trunk_sha,
                &msg,
                &[
                    ("GIT_AUTHOR_NAME", an),
                    ("GIT_AUTHOR_EMAIL", ae),
                    ("GIT_AUTHOR_DATE", ad),
                ],
            )?;
        }
    }

    // --- fast-forward first: until it succeeds nothing has changed ------------------------
    let ff = match &trunk_wt {
        Some(w) => {
            git::raw(w, &["merge", "--ff-only", "--quiet", &land], &[], None).and_then(|o| {
                if o.ok {
                    Ok(())
                } else {
                    Err(o.stderr.trim().to_string())
                }
            })
        }
        None => git::git(
            p,
            &[
                "update-ref",
                "-m",
                &format!("5w ship {branch}"),
                &format!("refs/heads/{trunk}"),
                &land,
                &trunk_sha,
            ],
        )
        .map(|_| ()),
    };
    if let Err(e) = ff {
        bail!("fast-forward of {trunk} to {branch} failed; nothing was changed: {e}");
    }
    // Said once it has landed: a refusal before this must stand alone.
    for n in &notices {
        eprintln!("{n}");
    }
    if land != tip {
        println!("ship: squashed into {}", short(&land));
    }
    let mut landed = land.clone();
    if repo.cfg.gate_trunk {
        match &authorised {
            Some((id, reviewed)) => match record_landing(repo, *id, &trunk_sha, &land) {
                Ok(r) => {
                    landed = r;
                    println!("ship: landing of #{id} recorded ({})", short(&landed));
                    if !git::ok(p, &["merge-base", "--is-ancestor", reviewed, &land]) {
                        println!(
                            "ship: the server needs #{id}'s reviewed commit: keep its branch pushed, or `git push <remote> {trunk} {reviewed}:refs/5w/reviewed/{id}`"
                        );
                    }
                }
                Err(e) => eprintln!(
                    "ship: {branch} landed, but its landing record did not commit ({e}); a push of {trunk} is refused until one does (README: gate_trunk)"
                ),
            },
            None => eprintln!(
                "ship: no review authorised {branch}, so no landing is recorded; gate_trunk refuses a push of {trunk}"
            ),
        }
    }

    // --- landed. Now tidy: the worktree, then the branch. ---------------------------------
    if branch_wt.is_some()
        && let Err(e) = wt::remove(repo, &branch, false)
    {
        bail!(
            "{branch} landed on {trunk}, but its worktree could not be removed ({e}); `{} rm {branch}` then `git branch -D {branch}`",
            repo.cfg.cmd_wt
        );
    }

    // Children of the shipped branch now stack on the trunk.
    if let Some(list) = git::opt(
        p,
        &["config", "--get-regexp", r"^git-town-branch\..*\.parent$"],
    ) {
        for line in list.lines() {
            if let Some((key, val)) = line.split_once(' ')
                && val == branch
            {
                git::git(p, &["config", key, trunk])?;
                let child = key
                    .trim_start_matches("git-town-branch.")
                    .trim_end_matches(".parent");
                println!("ship: {child} now stacks on {trunk}");
            }
        }
    }
    git::git(p, &["branch", "-D", &branch])?;
    let _ = git::raw(
        p,
        &[
            "config",
            "--remove-section",
            &format!("git-town-branch.{branch}"),
        ],
        &[],
        None,
    );
    println!(
        "\nship: {branch} is on {trunk} ({}..{})",
        short(&trunk_sha),
        short(&landed)
    );
    Ok(())
}

/// The landing record `gate_trunk` asks for: an empty commit on the landed tip
/// naming the task, the range and its change id, moved onto the trunk only if
/// the trunk is still at `land`.
fn record_landing(repo: &Repo, id: u64, base: &str, land: &str) -> Res<String> {
    let p = &repo.primary;
    let change = git::change_id(p, base, land)?;
    let tree = git::git(p, &["rev-parse", &format!("{land}^{{tree}}")])?;
    let msg = format!(
        "{}: land #{id}\n\nLanded: {base}..{land}\nChange: {change}",
        repo.cfg.commit_prefix
    );
    let r = git::commit_tree(p, &tree, land, &msg, &[])?;
    git::git(
        p,
        &[
            "update-ref",
            "-m",
            &format!("5w ship: land #{id}"),
            &format!("refs/heads/{}", repo.trunk),
            &r,
            land,
        ],
    )?;
    Ok(r)
}

fn compose(p: &std::path::Path, base: &str, tip: &str) -> Res<String> {
    let log = git::git(
        p,
        &["log", "--reverse", "--format=%H", &format!("{base}..{tip}")],
    )?;
    let shas: Vec<&str> = log.lines().collect();
    let first = git::git(p, &["log", "-1", "--format=%B", shas[0]])?;
    let mut msg = first.trim_end().to_string();
    let rest: Vec<String> = shas[1..]
        .iter()
        .filter_map(|s| git::opt(p, &["log", "-1", "--format=%s", s]))
        .collect();
    if !rest.is_empty() {
        msg += "\n\nSquashed:\n";
        for s in rest {
            msg += &format!("- {s}\n");
        }
    }
    Ok(msg)
}

/// Gitignored files in a worktree that removing it would delete: not the
/// symlinks `wt` made, not anything under a `worktrees.disposable` name.
pub fn ignored_files(repo: &Repo, w: &std::path::Path) -> Res<Vec<String>> {
    let out = git::git(
        w,
        &[
            "status",
            "--porcelain",
            "--ignored",
            "--untracked-files=all",
        ],
    )?;
    Ok(out
        .lines()
        .filter_map(|l| l.strip_prefix("!! "))
        .filter(|f| {
            let first = f.trim_end_matches('/').split('/').next().unwrap_or("");
            let last = f.trim_end_matches('/').rsplit('/').next().unwrap_or("");
            !repo.cfg.disposable.iter().any(|d| d == first || d == last)
        })
        .filter(|f| !w.join(f.trim_end_matches('/')).is_symlink())
        .map(String::from)
        .collect())
}

/// The reviewed change is the change that lands: compare what the branch adds
/// now with what it added at the reviewed commit (`git::change_id`), so a clean
/// rebase passes and anything added or altered after the review does not.
/// Returns the task and reviewed commit a review authorised the branch by, if any.
fn verify_reviewed(
    repo: &Repo,
    all: &[queue::Task],
    rows: &[queue::Task],
    branch: &str,
    force: bool,
    quiet: bool,
    notices: &mut Vec<String>,
) -> Res<Option<(u64, String)>> {
    let p = &repo.primary;
    let trunk = &repo.trunk;
    let tasks = &repo.cfg.cmd_tasks;
    let tip = git::rev(p, &format!("refs/heads/{branch}")).ok_or("cannot resolve branch")?;
    macro_rules! say { ($($t:tt)*) => { if !quiet { println!($($t)*) } } }
    // A --force notice waits for the ship to land, and is said once across the
    // checks before and after a rebase.
    let mut authorised = None;
    let mut note = |n: String| {
        if !notices.contains(&n) {
            notices.push(n);
        }
    };
    for t in rows.iter().filter(|t| t.state == State::Done) {
        match &t.reviewed {
            None => say!(
                "ship: authorised by #{} via:{} (no reviewed commit recorded)",
                t.id,
                t.via.as_deref().unwrap_or("unrecorded")
            ),
            Some(r) => {
                let why = match git::recorded(p, r) {
                    git::Recorded::Commit(c) if c == tip => {
                        say!("ship: authorised by #{} via:review at {}", t.id, short(r));
                        authorised.get_or_insert((t.id, c));
                        continue;
                    }
                    git::Recorded::Commit(c) => Ok(c),
                    git::Recorded::Missing => Err("no longer exists"),
                    git::Recorded::Ambiguous => {
                        Err("is ambiguous: more than one commit starts with it")
                    }
                    git::Recorded::Invalid => Err("is not a commit name (7 or more hex digits)"),
                };
                let reviewed = match why {
                    Ok(c) => c,
                    Err(why) if force => {
                        note(format!(
                            "ship: #{}'s reviewed:{} {why}; --force",
                            t.id,
                            short(r)
                        ));
                        continue;
                    }
                    Err(why) => bail!(
                        "#{} was reviewed at {}, which {why}; re-review, then `{tasks} accept {} --force --at {branch}`",
                        t.id,
                        short(r),
                        t.id
                    ),
                };
                let now = git::change_id(p, trunk, &tip)?;
                let then = git::change_id(p, trunk, &reviewed)?;
                if now == then {
                    authorised.get_or_insert((t.id, reviewed.clone()));
                    say!(
                        "ship: authorised by #{} via:review at {} (rebased since; same change)",
                        t.id,
                        short(r)
                    );
                } else if let Some(under) = landed_under(repo, all, &reviewed, &now, trunk, true)? {
                    authorised.get_or_insert((t.id, reviewed.clone()));
                    say!(
                        "ship: authorised by #{} via:review at {} (on #{under}, which landed; same change)",
                        t.id,
                        short(r)
                    );
                } else if force {
                    note(format!(
                        "ship: #{}'s branch changed since review at {}; --force",
                        t.id,
                        short(r)
                    ));
                } else {
                    bail!(
                        "{branch} is not the change #{} accepted at {}: `git range-diff {trunk}...{r} {trunk}...{branch}`; re-review with `{tasks} open {id}`, submit, accept",
                        t.id,
                        short(r),
                        id = t.id
                    );
                }
            }
        }
    }

    Ok(authorised)
}

/// A stacked branch was reviewed on top of its parent, so what it added then
/// includes the parent's change; once the parent has landed (rebased, or
/// squashed) that no longer compares. The parent's accepted row records the
/// commit reviewed under it: when that commit is below the reviewed one, its
/// branch is gone, and the trunk holds that commit's version of every path the
/// parent changed, the change to compare is what the branch added on top of it.
/// Returns the parent task's id when that change is `now`. `trunk` is where the
/// parent must have landed; `gone`: its branch must be gone too (a server that
/// judges a push keeps branches, and relies on the paths alone).
pub fn landed_under(
    repo: &Repo,
    all: &[queue::Task],
    reviewed: &str,
    now: &str,
    trunk: &str,
    gone: bool,
) -> Res<Option<u64>> {
    for (id, base) in landed_parents(repo, all, reviewed, trunk, gone)? {
        if git::change_id(&repo.primary, &base, reviewed)? == now {
            return Ok(Some(id));
        }
    }
    Ok(None)
}

/// Accepted parents of `commit` that have landed: (task id, reviewed commit) for
/// each accepted row whose branch is gone, whose reviewed commit is below
/// `commit` but not on the trunk, and whose every changed path the trunk holds
/// at that commit's version.
fn landed_parents(
    repo: &Repo,
    all: &[queue::Task],
    commit: &str,
    trunk: &str,
    gone: bool,
) -> Res<Vec<(u64, String)>> {
    let p = &repo.primary;
    let mut found = Vec::new();
    for t in all.iter().filter(|t| t.state == State::Done) {
        let (Some(r), Some(b)) = (&t.reviewed, &t.branch) else {
            continue;
        };
        if gone && git::branch_exists(p, b) {
            continue;
        }
        let git::Recorded::Commit(base) = git::recorded(p, r) else {
            continue;
        };
        if base == commit
            || !git::ok(p, &["merge-base", "--is-ancestor", &base, commit])
            || git::ok(p, &["merge-base", "--is-ancestor", &base, trunk])
        {
            continue;
        }
        let Some(mb) = git::opt(p, &["merge-base", trunk, &base]) else {
            continue;
        };
        // Names are compared here, never handed back to git: as pathspecs a
        // quoted non-ASCII name or a `:`-magic name would match nothing and pass.
        // Submodules are listed whatever diff.ignoreSubmodules says.
        let names = |from: &str, to: &str| -> Res<HashSet<String>> {
            let o = git::git(p, &git::pinned_diff(&["--name-only", "-z", from, to]))?;
            Ok(o.split('\0')
                .filter(|n| !n.is_empty())
                .map(String::from)
                .collect())
        };
        let changed = names(&mb, &base)?;
        if !changed.is_empty() && !names(&base, trunk)?.is_disjoint(&changed) {
            continue;
        }
        found.push((t.id, base));
    }
    Ok(found)
}

/// Where `--sync` rebases `branch` from: the reviewed commit of the nearest
/// accepted parent that has landed (`landed_parents`), so only the branch's own
/// commits are replayed — a parent landed by `--squash`, or rebased as it
/// shipped, is not on the trunk as the commits the branch holds, and replaying
/// those conflicts with itself. None: rebase as plain `git rebase <trunk>` does.
///
/// The reviewed commit, not a remembered tip or a merge-base: it is in the
/// committed queue rather than local config, it is the very commit the gate
/// compares the branch's change against, and a merge-base with a branch that is
/// gone cannot be computed. A parent that did not land is never cut away here.
fn sync_upstream(repo: &Repo, all: &[queue::Task], tip: &str) -> Res<Option<String>> {
    let p = &repo.primary;
    let bases: Vec<String> = landed_parents(repo, all, tip, &repo.trunk, true)?
        .into_iter()
        .map(|(_, b)| b)
        .collect();
    // The nearest: no other landed parent sits above it.
    Ok(bases
        .iter()
        .find(|b| {
            !bases
                .iter()
                .any(|o| o != *b && git::ok(p, &["merge-base", "--is-ancestor", b.as_str(), o]))
        })
        .cloned())
}

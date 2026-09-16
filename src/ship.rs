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

pub const USAGE: &str = "\
usage: 5w ship [<branch>] [--sync] [--squash] [-m <message>] [--force]

  --sync     rebase the branch onto the trunk first, in its worktree
  --squash   land one commit (message from -m, or composed from the branch's commits)
  --force    override the review gate only — never the safety checks";

pub fn run(repo: &Repo, args: &[String]) -> Res<()> {
    let mut branch = None;
    let (mut sync, mut squash, mut force) = (false, false, false);
    let mut message = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--sync" => sync = true,
            "--squash" => squash = true,
            "--force" => force = true,
            "-m" | "--message" => {
                message = Some(args.get(i + 1).ok_or("-m needs a message")?.clone());
                i += 1;
            }
            "-h" | "--help" | "help" => {
                println!("{USAGE}");
                return Ok(());
            }
            a if a.starts_with('-') => bail!("unknown flag {a}\n{USAGE}"),
            a => {
                if branch.is_some() {
                    bail!("one branch at a time");
                }
                branch = Some(a.to_string());
            }
        }
        i += 1;
    }
    if message.is_some() && !squash {
        bail!("-m only applies with --squash");
    }
    let branch = match branch.or_else(|| git::current_branch(&repo.cwd)) {
        Some(b) => b,
        None => bail!("{USAGE}"),
    };
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
            "{branch} is stacked on {parent}. Ship the bottom of the stack first:\n    {} {parent}",
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
    let rows: Vec<_> = queue::parse(&committed)
        .into_iter()
        .filter(|t| t.branch.as_deref() == Some(branch.as_str()))
        .collect();
    if rows.is_empty() {
        if repo.cfg.require_task && !force {
            bail!(
                "no task names branch:{branch}, and this repo requires one (require_task). --force to ship unreviewed."
            );
        }
        eprintln!("ship: no task references {branch} — shipping unreviewed");
    }
    let unaccepted: Vec<_> = rows.iter().filter(|t| t.state != State::Done).collect();
    if !unaccepted.is_empty() {
        if !force {
            let list: Vec<String> = unaccepted
                .iter()
                .map(|t| format!("  #{} [{}] {}", t.id, t.state.mark(), t.text))
                .collect();
            bail!(
                "not accepted yet:\n{}\n\n  Review it:\n    git diff {trunk}...{branch}\n  then:\n    {tasks} accept <id>    # or: {tasks} reject <id> <reason>",
                list.join("\n")
            );
        }
        eprintln!(
            "ship: --force — shipping {} unaccepted task(s)",
            unaccepted.len()
        );
    }

    // --- preflight ------------------------------------------------------------------
    let branch_wt = git::worktree_of(p, &branch)?;
    if let Some(w) = &branch_wt
        && git::dirty(w)?
    {
        bail!(
            "{branch} has uncommitted or untracked files in {} — commit or clean them",
            w.display()
        );
    }
    let trunk_wt = git::worktree_of(p, trunk)?;
    if let Some(w) = &trunk_wt
        && git::dirty_tracked(w)?
    {
        bail!(
            "{} has uncommitted changes to tracked files; a fast-forward of {trunk} wants them committed or stashed",
            w.display()
        );
    }

    // Before anything rewrites the branch, and again after a rebase has.
    let behind = !git::ok(p, &["merge-base", "--is-ancestor", trunk, &branch]);
    verify_reviewed(repo, &rows, &branch, force, behind && sync)?;

    if behind {
        if !sync {
            bail!(
                "{branch} is behind {trunk}. Rebase it first, or let ship do it:\n    {} {branch} --sync",
                repo.cfg.cmd_ship
            );
        }
        let Some(w) = &branch_wt else {
            bail!(
                "--sync rebases in the branch's worktree, and {branch} has none:\n    {} add {branch}",
                repo.cfg.cmd_wt
            )
        };
        println!("ship: rebasing {branch} onto {trunk}");
        let o = git::raw(w, &["rebase", trunk], &[], None)?;
        if !o.ok {
            let _ = git::raw(w, &["rebase", "--abort"], &[], None);
            bail!(
                "rebase of {branch} onto {trunk} stopped on conflicts and was aborted:\n{}\n  Resolve it by hand in {}, then ship again. A resolved conflict changes the patch, so it will need a fresh accept.",
                o.stderr.trim(),
                w.display()
            );
        }
        verify_reviewed(repo, &rows, &branch, force, false)?;
    }

    let tip = git::rev(p, &format!("refs/heads/{branch}")).ok_or("cannot resolve branch")?;

    // --- squash ----------------------------------------------------------------------
    let mut land = tip.clone();
    let trunk_sha = git::rev(p, &format!("refs/heads/{trunk}")).ok_or("cannot resolve trunk")?;
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
            let o = git::raw(
                p,
                &["commit-tree", &tree, "-p", &trunk_sha, "-F", "-"],
                &[
                    ("GIT_AUTHOR_NAME", an),
                    ("GIT_AUTHOR_EMAIL", ae),
                    ("GIT_AUTHOR_DATE", ad),
                ],
                Some(&format!("{}\n", msg.trim_end())),
            )?;
            if !o.ok {
                bail!("git commit-tree: {}", o.stderr.trim());
            }
            land = o.stdout.trim().to_string();
            // Same tree, so the branch's worktree stays clean when its ref moves.
            git::git(
                p,
                &[
                    "update-ref",
                    "-m",
                    "5w ship --squash",
                    &format!("refs/heads/{branch}"),
                    &land,
                    &tip,
                ],
            )?;
            println!("ship: squashed {count} commits into {}", short(&land));
        }
    }

    // --- worktree off, fast-forward, clean up ----------------------------------------------
    let removed = match &branch_wt {
        Some(_) => {
            wt::remove(repo, &branch, false)?;
            true
        }
        None => false,
    };
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
        if removed {
            eprintln!("ship: fast-forward failed — restoring the worktree");
            if let Err(e2) = wt::add_worktree(repo, &branch, false) {
                eprintln!(
                    "ship: could not restore it ({e2}); {} add {branch}",
                    repo.cfg.cmd_wt
                );
            }
        }
        bail!(
            "fast-forward of {trunk} to {branch} failed: {e}\n  Check `git log {trunk}` before retrying."
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
        short(&land)
    );
    Ok(())
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

/// The reviewed change is the change that lands: compare what the branch adds
/// now with what it added at the reviewed commit, by patch-id, so a clean rebase
/// passes and anything added or altered after the review does not.
fn verify_reviewed(
    repo: &Repo,
    rows: &[queue::Task],
    branch: &str,
    force: bool,
    quiet: bool,
) -> Res<()> {
    let p = &repo.primary;
    let trunk = &repo.trunk;
    let tasks = &repo.cfg.cmd_tasks;
    let tip = git::rev(p, &format!("refs/heads/{branch}")).ok_or("cannot resolve branch")?;
    macro_rules! say { ($($t:tt)*) => { if !quiet { println!($($t)*) } } }
    for t in rows.iter().filter(|t| t.state == State::Done) {
        match &t.reviewed {
            None => say!(
                "ship: authorised by #{} via:{} (no reviewed commit recorded)",
                t.id,
                t.via.as_deref().unwrap_or("unrecorded")
            ),
            Some(r) if tip.starts_with(r.as_str()) => {
                say!("ship: authorised by #{} via:review at {r}", t.id)
            }
            Some(r) => {
                let Some(reviewed) = git::rev(p, r) else {
                    if force {
                        eprintln!("ship: reviewed commit {r} is gone; --force");
                        continue;
                    }
                    bail!(
                        "#{} was reviewed at {r}, which no longer resolves. Re-review and `{tasks} accept {} --force --at {branch}`.",
                        t.id,
                        t.id
                    )
                };
                let now = git::change_id(p, trunk, &tip)?;
                let then = git::change_id(p, trunk, &reviewed)?;
                if now == then {
                    say!(
                        "ship: authorised by #{} via:review at {r} (rebased since; same change)",
                        t.id
                    );
                } else if force {
                    eprintln!(
                        "ship: #{}'s branch changed since review at {r}; --force",
                        t.id
                    );
                } else {
                    bail!(
                        "{branch} is not the change #{} accepted at {r} — something was added or altered after the review.\n  \
                         What changed:\n    git range-diff {trunk}...{r} {trunk}...{branch}\n  \
                         Re-review, then:\n    {tasks} open {id} && {tasks} submit {id} && {tasks} accept {id}",
                        t.id,
                        id = t.id
                    );
                }
            }
        }
    }

    Ok(())
}

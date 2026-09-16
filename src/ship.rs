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
usage: 5w ship [<branch>] [--sync] [--squash] [-m <message>] [--discard-ignored] [--force]

  --sync     rebase the branch onto the trunk first, in its worktree
  --squash   land one commit (message from -m, or composed from the branch's commits)
  --discard-ignored   delete gitignored files in the branch's worktree with it
  --force    override the review gate only — never the safety checks";

pub fn run(repo: &Repo, args: &[String]) -> Res<()> {
    let mut branch = None;
    let (mut sync, mut squash, mut force, mut discard_ignored) = (false, false, false, false);
    let mut message = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--sync" => sync = true,
            "--squash" => squash = true,
            "--force" => force = true,
            "--discard-ignored" => discard_ignored = true,
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
    let rows: Vec<_> = queue::parse(&committed)
        .into_iter()
        .chain(queue::parse(&archived))
        .filter(|t| t.branch.as_deref() == Some(branch.as_str()))
        .collect();
    if rows.is_empty() {
        if repo.cfg.require_task && !force {
            bail!(
                "no task names branch:{branch} and require_task is on (--force ships unreviewed)"
            );
        }
        eprintln!("ship: no task references {branch} — shipping unreviewed");
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
        eprintln!(
            "ship: --force — shipping {} unaccepted task(s)",
            unaccepted.len()
        );
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
    verify_reviewed(repo, &rows, &branch, force, behind && sync)?;

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
        println!("ship: rebasing {branch} onto {trunk}");
        let o = git::raw(w, &["rebase", trunk], &[], None)?;
        if !o.ok {
            let _ = git::raw(w, &["rebase", "--abort"], &[], None);
            bail!(
                "rebase onto {trunk} conflicts (aborted); rebase by hand in {}, then re-review",
                w.display()
            );
        }
        if let Err(e) = verify_reviewed(repo, &rows, &branch, force, false) {
            // The rebase bought nothing; do not leave the branch rewritten.
            let _ = git::raw(w, &["reset", "--hard", "--quiet", &before], &[], None);
            bail!(
                "{e}\n  ({branch} is back at {} as it was before the rebase)",
                short(&before)
            );
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
        bail!("fast-forward of {trunk} to {branch} failed; nothing was changed:\n{e}");
    }
    if land != tip {
        println!("ship: squashed into {}", short(&land));
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

/// Gitignored files in a worktree that removing it would delete: not the
/// symlinks `wt` made, not anything under a `worktrees.disposable` name.
fn ignored_files(repo: &Repo, w: &std::path::Path) -> Res<Vec<String>> {
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
                        "#{} was reviewed at {r}, which no longer exists; re-review, then `{tasks} accept {} --force --at {branch}`",
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
                        "{branch} is not the change #{} accepted at {r}: `git range-diff {trunk}...{r} {trunk}...{branch}`; re-review with `{tasks} open {id}`, submit, accept",
                        t.id,
                        id = t.id
                    );
                }
            }
        }
    }

    Ok(())
}

//! `5w ci` — the checks a forge or a git server runs, with no forge in them.
//!
//! Everything arrives as arguments; a wrapper of a few lines maps its forge's
//! variables onto them. Nothing here reads a CI environment variable, so the
//! same command runs under any CI, in a `pre-receive` hook, and by hand.

use crate::bail;
use crate::git;
use crate::lint;
use crate::queue::{self, State};
use crate::store::Repo;
use crate::util::{Res, short};

pub const USAGE: &str = "\
usage: 5w ci [--base <rev>] [--head <rev>] [--ref <refname> | --branch <name>] [--trunk <ref>]

  --head <rev>       the new tip (default HEAD)
  --base <rev>       the old tip; commits in base..head are checked. Omitted, or
                     all zeros (a new ref): the merge-base with the trunk
  --ref <refname>    a push to this ref. refs/heads/<trunk>: the commits land on
                     the trunk. Any other branch: they carry no queue edits
  --branch <name>    a change request from this branch into the trunk: no queue
                     edits, and the ship check — an accepted task names the
                     branch and what it adds is what was reviewed
  --trunk <ref>      where the trunk is (default refs/heads/<trunk>, else
                     refs/remotes/origin/<trunk>)

Needs full history in the checkout. Exit 0 clean, 1 with one line per finding.";

pub fn run(repo: &Repo, args: &[String]) -> Res<()> {
    let (mut base, mut head, mut refname, mut branch, mut trunk_ref) =
        (None, None, None, None, None);
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if matches!(a, "-h" | "--help" | "help") {
            println!("{USAGE}");
            return Ok(());
        }
        let v = args.get(i + 1).cloned().filter(|v| !v.is_empty());
        match a {
            "--base" => base = v,
            "--head" => head = v,
            "--ref" => refname = v,
            "--branch" => branch = v,
            "--trunk" => trunk_ref = v,
            _ => bail!("unexpected {a:?}\n{USAGE}"),
        }
        i += 2;
    }
    if refname.is_some() && branch.is_some() {
        bail!("--ref is a push, --branch a change request: give one");
    }
    let p = &repo.primary;
    // The trunk as a plain sha: a bad --trunk would read an empty queue.
    let trunk_ref = match trunk_ref {
        Some(t) => Some(git::rev(p, &t).ok_or_else(|| {
            format!("ci: --trunk {t} is not a commit — pass a branch, tag or sha")
        })?),
        None => [
            format!("refs/heads/{}", repo.trunk),
            format!("refs/remotes/origin/{}", repo.trunk),
        ]
        .into_iter()
        .find_map(|r| git::rev(p, &r)),
    };
    let head = head.unwrap_or_else(|| "HEAD".into());
    let head = git::rev(p, &head)
        .ok_or_else(|| format!("ci: --head {head} is not a commit — pass a branch, tag or sha"))?;
    let base = base.filter(|b| !b.bytes().all(|c| c == b'0'));
    let base = match base {
        Some(b) => Some(
            git::rev(p, &b)
                .ok_or_else(|| format!("ci: --base {b} is not a commit — pass a branch, tag or sha, with full history fetched"))?,
        ),
        None => trunk_ref
            .as_ref()
            .and_then(|t| git::opt(p, &["merge-base", t, &head])),
    };
    let range = match &base {
        Some(b) if *b == head => vec![],
        Some(b) => git::git(p, &["rev-list", "--reverse", &format!("{b}..{head}")])?
            .lines()
            .map(String::from)
            .collect(),
        None => git::git(p, &["rev-list", "--reverse", &head])?
            .lines()
            .map(String::from)
            .collect(),
    };

    let onto_trunk = refname.as_deref() == Some(format!("refs/heads/{}", repo.trunk).as_str());
    let mut problems = Vec::new();
    lint::commits_on(repo, &range, &|_| onto_trunk, &mut problems)?;

    if let Some(b) = &branch {
        let Some(t) = &trunk_ref else {
            bail!(
                "no trunk ref to check {b} against — fetch {} or pass --trunk",
                repo.trunk
            )
        };
        ship_check(repo, t, b, &head, &mut problems)?;
    }

    let what = match (&branch, &refname) {
        (Some(b), _) => format!("{b} into {}", repo.trunk),
        (_, Some(r)) => format!("push to {r}"),
        _ => "range".into(),
    };
    if problems.is_empty() {
        println!("5w ci: {} commit(s), {what}: ok", range.len());
        return Ok(());
    }
    for pr in &problems {
        eprintln!("  {pr}");
    }
    bail!("{} finding(s) — {what}", problems.len())
}

/// What `5w ship` checks locally, against the trunk as the forge has it.
fn ship_check(
    repo: &Repo,
    trunk: &str,
    branch: &str,
    head: &str,
    out: &mut Vec<String>,
) -> Res<()> {
    let p = &repo.primary;
    let show = |f: &str| git::opt(p, &["show", &format!("{trunk}:{f}")]).unwrap_or_default();
    let rows: Vec<_> = queue::parse(&show(&repo.cfg.file))
        .into_iter()
        .chain(queue::parse(&show(&repo.cfg.archive)))
        .filter(|t| t.branch.as_deref() == Some(branch))
        .collect();
    if rows.is_empty() {
        if repo.cfg.require_task {
            out.push(format!(
                "{branch}: no task names it, and require_task is on"
            ));
        } else {
            println!("5w ci: no task names {branch} — unreviewed change");
        }
        return Ok(());
    }
    for t in &rows {
        match (t.state, t.via.as_deref(), t.reviewed.as_deref()) {
            (State::Done, Some("review"), Some(r)) => {
                let Some(reviewed) = git::rev(p, r) else {
                    out.push(format!(
                        "#{}: reviewed commit {r} is not in this clone — fetch full history",
                        t.id
                    ));
                    continue;
                };
                if git::change_id(p, trunk, head)? == git::change_id(p, trunk, &reviewed)? {
                    println!(
                        "5w ci: #{} accepted at {r}; {branch} at {} is that change",
                        t.id,
                        short(head)
                    );
                } else {
                    out.push(format!(
                        "#{}: {branch} is not the change accepted at {r} — re-review (`git range-diff {r}...{}`)",
                        t.id,
                        short(head)
                    ));
                }
            }
            (State::Done, via, _) => {
                println!(
                    "5w ci: #{} closed via:{} with no reviewed commit",
                    t.id,
                    via.unwrap_or("?")
                );
            }
            (s, _, _) => out.push(format!(
                "#{}: not accepted yet ([{}]) — re-run this check after `5w accept {}`",
                t.id,
                s.mark(),
                t.id
            )),
        }
    }
    Ok(())
}

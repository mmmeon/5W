//! `5w ci` — the checks a forge or a git server runs, with no forge in them.
//!
//! Everything arrives as arguments; a wrapper of a few lines maps its forge's
//! variables onto them. Nothing here reads a CI environment variable, so the
//! same command runs under any CI, in a `pre-receive` hook, and by hand.

use crate::bail;
use crate::git;
use crate::lint;
use crate::queue::{self, State, Task};
use crate::store::Repo;
use crate::util::{Res, parse_id, short};

pub const USAGE: &str = "\
usage: 5w ci [--base <rev>] [--head <rev>] [--ref <refname> | --branch <name>] [--trunk <ref>]
       5w ci --event submit|accept --branch <name> [--head <rev>] [--at <rev>] [--task <id>]

  --head <rev>       the new tip (default HEAD)
  --base <rev>       the old tip; commits in base..head are checked. Omitted, or
                     all zeros (a new ref): the merge-base with the trunk
  --ref <refname>    a push to this ref. refs/heads/<trunk>: the commits land on
                     the trunk. Any other branch: they carry no queue edits
                     (commits the trunk holds are not judged)
  --branch <name>    a change request from this branch into the trunk: no queue
                     edits, and the ship check — an accepted task names the
                     branch and what it adds is what was reviewed
  --trunk <ref>      where the trunk is (default refs/heads/<trunk>, else
                     refs/remotes/origin/<trunk>)

Needs full history in the checkout. Exit 0 clean, 1 with one line per finding.

  --event submit     a change request opened or updated: submit the task naming
                     --branch at --head (default: the branch, else origin's)
  --event accept     an approving review: accept that task at --at, the reviewed
                     commit, which must be --head — the change request's tip now
  --task <id>        the task, when none names the branch yet

An event commits to refs/heads/<trunk> and does not push; re-running one is a
no-op. Exit 0 done or nothing to do, 1 refused.";

pub fn run(repo: &Repo, args: &[String]) -> Res<()> {
    let (mut base, mut head, mut refname, mut branch, mut trunk_ref) =
        (None, None, None, None, None);
    let (mut event, mut at, mut task) = (None, None, None);
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
            "--event" => event = v,
            "--at" => at = v,
            "--task" => task = v,
            _ if a.starts_with('-') => return Err(crate::tasks::unknown_flag(repo, "ci", a)),
            _ => bail!(
                "ci takes only flags, not {a:?} ({} ci --help)",
                repo.cfg.cmd_tasks
            ),
        }
        i += 2;
    }
    if let Some(e) = event {
        if base.is_some() || refname.is_some() || trunk_ref.is_some() {
            bail!("--event takes --branch, --head, --at and --task, not --base, --ref or --trunk");
        }
        let Some(b) = branch else {
            bail!("--event {e} needs --branch <name>, the change request's branch")
        };
        let task = task.as_deref().map(parse_id).transpose()?;
        return match e.as_str() {
            "submit" if at.is_some() => {
                bail!("--at is the reviewed commit: it goes with --event accept")
            }
            "submit" => submit_event(repo, &b, head.as_deref(), task),
            "accept" => {
                let Some(at) = at else {
                    bail!("--event accept needs --at <rev>, the commit the review approved")
                };
                accept_event(repo, &b, head.as_deref(), &at, task)
            }
            _ => bail!("--event is submit or accept, not {e:?}"),
        };
    }
    if at.is_some() || task.is_some() {
        bail!("--at and --task go with --event");
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
    let onto_trunk = refname.as_deref() == Some(format!("refs/heads/{}", repo.trunk).as_str());
    let span = match &base {
        Some(b) => format!("{b}..{head}"),
        None => head.clone(),
    };
    let mut rev_list = vec!["rev-list", "--reverse", &span];
    // Off the trunk, what the trunk already holds (merged in) is the trunk's, not the
    // branch's. A plain range names no branch: it judges every commit in it.
    let off_trunk = branch.is_some() || (refname.is_some() && !onto_trunk);
    if let Some(t) = trunk_ref.as_deref().filter(|_| off_trunk) {
        rev_list.extend(["--not", t]);
    }
    let range: Vec<String> = match &base {
        Some(b) if *b == head => vec![],
        _ => git::git(p, &rev_list)?.lines().map(String::from).collect(),
    };

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
    // A link's blob is its target's path, which parses as an empty queue.
    let links: Vec<_> = [&repo.cfg.file, &repo.cfg.archive]
        .into_iter()
        .filter(|f| {
            git::opt(p, &["ls-tree", "--full-tree", trunk, "--", f])
                .is_some_and(|e| e.starts_with("120000 "))
        })
        .collect();
    if !links.is_empty() {
        for f in links {
            out.push(format!(
                "{f} is a symlink on {}, not the queue file — replace the link with the file",
                repo.trunk
            ));
        }
        return Ok(());
    }
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
                let reviewed = match git::recorded(p, r) {
                    git::Recorded::Commit(c) => c,
                    other => {
                        out.push(format!(
                            "#{}: reviewed:{r} {}",
                            t.id,
                            recorded_problem(&other)
                        ));
                        continue;
                    }
                };
                if git::change_id(p, trunk, head)? == git::change_id(p, trunk, &reviewed)? {
                    println!(
                        "5w ci: #{} accepted at {}; {branch} at {} is that change",
                        t.id,
                        short(r),
                        short(head)
                    );
                } else {
                    out.push(format!(
                        "#{}: {branch} is not the change accepted at {} — re-review (`git range-diff {r}...{}`)",
                        t.id,
                        short(r),
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

/// The task an event is about: `--task`, else the one unclosed task naming the
/// branch. `None` with a note printed when there is nothing to act on.
fn event_task(repo: &Repo, branch: &str, task: Option<u64>, what: &str) -> Res<Option<Task>> {
    if git::rev(&repo.primary, &format!("refs/heads/{}", repo.trunk)).is_none() {
        bail!(
            "ci --event commits to refs/heads/{0}, which this clone lacks — `git fetch origin +{0}:{0}` first",
            repo.trunk
        );
    }
    let committed = |f: &str| repo.committed_file(f).map(Option::unwrap_or_default);
    let mut all = queue::parse(&committed(&repo.cfg.file)?);
    all.extend(queue::parse(&committed(&repo.cfg.archive)?));
    if let Some(id) = task {
        if let Some(o) = all
            .iter()
            .find(|t| t.id != id && t.state != State::Done && t.branch.as_deref() == Some(branch))
        {
            bail!("#{} already names {branch}, not #{id} — drop --task", o.id);
        }
        let Some(t) = all.into_iter().find(|t| t.id == id) else {
            bail!("#{id} is not on {}'s queue", repo.trunk)
        };
        if let Some(tb) = t.branch.as_deref().filter(|tb| *tb != branch) {
            bail!("#{id} names branch {tb}, not {branch}");
        }
        return Ok(Some(t));
    }
    let naming: Vec<Task> = all
        .into_iter()
        .filter(|t| t.branch.as_deref() == Some(branch))
        .collect();
    let open: Vec<&Task> = naming.iter().filter(|t| t.state != State::Done).collect();
    match (open.as_slice(), naming.first()) {
        ([one], _) => Ok(Some((*one).clone())),
        ([], Some(t)) => Ok(Some(t.clone())),
        ([], None) => {
            println!("5w ci: no task names {branch} — nothing to {what} (--task <id> names one)");
            Ok(None)
        }
        (many, _) => bail!(
            "{} name {branch} — pass --task <id>",
            many.iter()
                .map(|t| format!("#{}", t.id))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// `--head`, else the branch, else origin's copy of it.
fn event_head(repo: &Repo, branch: &str, head: Option<&str>) -> Res<String> {
    let p = &repo.primary;
    match head {
        Some(h) => git::rev(p, h)
            .ok_or_else(|| format!("ci: --head {h} is not a commit — pass a branch, tag or sha")),
        None => git::rev(p, &format!("refs/heads/{branch}"))
            .or_else(|| git::rev(p, &format!("refs/remotes/origin/{branch}")))
            .ok_or_else(|| {
                format!("ci: no branch {branch} in this clone — fetch it or pass --head")
            }),
    }
}

fn submit_event(repo: &Repo, branch: &str, head: Option<&str>, task: Option<u64>) -> Res<()> {
    if branch == repo.trunk || repo.is_perennial(branch) {
        bail!("{branch} is not a task branch — a change request from it submits nothing");
    }
    let Some(t) = event_task(repo, branch, task, "submit")? else {
        return Ok(());
    };
    let head = event_head(repo, branch, head)?;
    let id = t.id;
    match (t.state, t.submitted.as_deref()) {
        (State::Done, _) => println!("5w ci: #{id} is closed — nothing to submit"),
        (State::Review, Some(s)) if git::names(&repo.primary, s, &head) => {
            println!("5w ci: #{id} already submitted at {}", short(s))
        }
        (State::Review, s) => println!(
            "5w ci: #{id} submitted at {}; {branch} is now at {} — review reads the drift, accept names the commit reviewed",
            s.map(short).unwrap_or("?"),
            short(&head)
        ),
        (State::Open, _) => match rejected_at(repo, id) {
            // A re-run of the job that submitted what was rejected must not resubmit it.
            Some(r) if t.rework.is_some() && git::names(&repo.primary, &r, &head) => println!(
                "5w ci: #{id} was rejected at {} — a new commit on {branch} submits it again",
                short(&r)
            ),
            _ => crate::tasks::submit_at(repo, &t, branch, &head)?,
        },
    }
    Ok(())
}

fn accept_event(
    repo: &Repo,
    branch: &str,
    head: Option<&str>,
    at: &str,
    task: Option<u64>,
) -> Res<()> {
    let p = &repo.primary;
    let Some(t) = event_task(repo, branch, task, "accept")? else {
        return Ok(());
    };
    let reviewed = git::rev(p, at).ok_or_else(|| {
        format!("ci: --at {at} is not a commit — pass the reviewed sha, with full history fetched")
    })?;
    let head = event_head(repo, branch, head)?;
    let id = t.id;
    if head != reviewed {
        bail!(
            "{branch} is at {}, the review approved {} — a review of the tip accepts it",
            short(&head),
            short(&reviewed)
        );
    }
    match (t.state, t.reviewed.as_deref()) {
        (State::Done, Some(r)) if git::names(p, r, &reviewed) => {
            println!("5w ci: #{id} already accepted at {}", short(r))
        }
        (State::Done, _) => println!(
            "5w ci: #{id} is closed (via:{}{}) — nothing to accept",
            t.via.as_deref().unwrap_or("?"),
            t.reviewed
                .as_deref()
                .map(|r| format!(", reviewed:{}", short(r)))
                .unwrap_or_default()
        ),
        (State::Open, _) => bail!(
            "#{id} is not submitted — `{} ci --event submit --branch {branch}` comes first",
            repo.cfg.cmd_tasks
        ),
        (State::Review, sub) => {
            if let Some(s) = sub {
                let sub = match git::recorded(p, s) {
                    git::Recorded::Commit(c) => c,
                    other => bail!("#{id}'s submitted:{s} {}", recorded_problem(&other)),
                };
                if !git::ok(p, &["merge-base", "--is-ancestor", &sub, &reviewed]) {
                    bail!(
                        "#{id} was submitted at {}, which {} does not contain — the review predates the submit",
                        short(s),
                        short(&reviewed)
                    );
                }
            }
            crate::tasks::accept_one(repo, id, Some(&reviewed), false)?
        }
    }
    Ok(())
}

/// Why a recorded sha names no commit, for a refusal.
fn recorded_problem(r: &git::Recorded) -> &'static str {
    match r {
        git::Recorded::Ambiguous => "is ambiguous: more than one commit starts with it",
        git::Recorded::Invalid => "is not a commit name (7 or more hex digits)",
        _ => "is not in this clone — fetch full history",
    }
}

/// The commit the last rejection of `id` on the trunk sent back, if the history says.
fn rejected_at(repo: &Repo, id: u64) -> Option<String> {
    let p = &repo.primary;
    let c = git::opt(
        p,
        &[
            "log",
            "-1",
            "--format=%H",
            "-E",
            &format!("--grep=reject #{id}([^0-9]|$)"),
            &format!("refs/heads/{}", repo.trunk),
            "--",
            &repo.cfg.file,
        ],
    )
    .filter(|c| !c.is_empty())?;
    let before = git::opt(p, &["show", &format!("{c}^:{}", repo.cfg.file)])?;
    queue::parse(&before)
        .into_iter()
        .find(|t| t.id == id)?
        .submitted
}

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
use std::collections::{HashMap, HashSet};

pub const USAGE: &str = "\
usage: 5w ci [--base <rev>] [--head <rev>] [--ref <refname> | --branch <name>] [--trunk <ref>]
       5w ci --event submit|accept --branch <name> [--head <rev>] [--at <rev>] [--task <id>]

  --head <rev>       the new tip (default HEAD)
  --base <rev>       the old tip; commits in base..head are checked. Omitted, or
                     all zeros (a new ref): the merge-base with the trunk
  --ref <refname>    a push to this ref. refs/heads/<trunk>: the commits land on
                     the trunk (under gate_trunk, code needs a landing
                     record). Any other branch: they carry no queue edits
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
        if let Some(err) = &repo.broken {
            return Err(unreadable(repo, err, None));
        }
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
    // A symbolic ref pushed to moves what it points at: judge that ref.
    let refname = match refname {
        Some(r) => {
            let mut r = r;
            for _ in 0..8 {
                match git::opt(p, &["symbolic-ref", "-q", &r]) {
                    Some(t) if !t.is_empty() && t != r => r = t,
                    _ => break,
                }
            }
            Some(r)
        }
        None => None,
    };
    // The trunk the summary names: --trunk as given, else where a broken config
    // was read, else the trunk.
    let into = trunk_ref.clone().unwrap_or_else(|| {
        repo.broken_at
            .as_ref()
            .map_or(&repo.trunk, |(t, _)| t)
            .clone()
    });
    // The trunk as a plain sha: a bad --trunk would read an empty queue. Over a
    // broken config, the tip its names were read at, not the trunk it names.
    let trunk_ref = match trunk_ref {
        Some(t) => {
            let sha = git::rev(p, &t).ok_or_else(|| {
                format!("ci: --trunk {t} is not a commit — pass a branch, tag or sha")
            })?;
            // A forge whose default branch is not the trunk would read another
            // branch's queue: every task would fail, none naming why.
            if let Some(b) = trunk_ref_branch(p, &t)
                && let Some(named) = committed_trunk(p, Some(&sha)).filter(|n| *n != b)
            {
                bail!(
                    "ci: --trunk {t} is branch {b}, but its .5w.toml names the trunk {named} — make {named} the forge's default branch, or pass --trunk for {named}"
                );
            }
            Some(sha)
        }
        None => repo.broken_at.as_ref().map(|(_, t)| t.clone()).or_else(|| {
            [
                format!("refs/heads/{}", repo.trunk),
                format!("refs/remotes/origin/{}", repo.trunk),
            ]
            .into_iter()
            .find_map(|r| git::rev(p, &r))
        }),
    };
    // A trunk whose config does not parse would refuse every push, the fix too:
    // judge a trunk push, or a change request, whose tip commits a config that
    // parses under that config (the gate reads each commit's own, unreadable as
    // on; require_task the trunk's, likewise); refuse the rest.
    let zeros = |r: &Option<String>| r.as_ref().is_some_and(|r| r.bytes().all(|c| c == b'0'));
    let repaired;
    let repo = match &repo.broken {
        Some(err) if !zeros(&head) => {
            let onto = refname.as_deref() == Some(format!("refs/heads/{}", repo.trunk).as_str());
            // A tip that is not there is the fix to name, not the config.
            let h = head.as_deref().unwrap_or("HEAD");
            let tip = git::rev(p, h).ok_or_else(|| {
                format!("ci: --head {h} is not a commit — pass a branch, tag or sha")
            })?;
            let cfg = match (onto || branch.is_some()).then_some(&tip) {
                Some(t) => {
                    match git::opt(p, &["show", &format!("{t}:{}", crate::store::CONFIG_FILE)]) {
                        Some(text) => crate::config::Config::from_toml(&text).ok(),
                        None => Some(crate::config::Config::default()),
                    }
                }
                None => None,
            };
            let Some(mut cfg) = cfg else {
                return Err(unreadable(repo, err, branch.as_deref()));
            };
            // Queue edits and landings are told by the names the trunk had, never
            // by the pushed config: a repair naming code its queue would ungate it.
            cfg.file = repo.cfg.file.clone();
            cfg.archive = repo.cfg.archive.clone();
            cfg.commit_prefix = repo.cfg.commit_prefix.clone();
            // Nor may a change request's config drop the review its check asks for.
            cfg.require_task = setting_on(p, trunk_ref.as_deref(), "require_task");
            // Named on the trunk it was read from; a repair only if the tip changes
            // the config since it forked from that trunk.
            let config_at = |r: &str| {
                git::opt(
                    p,
                    &["rev-parse", &format!("{r}:{}", crate::store::CONFIG_FILE)],
                )
            };
            let repairs = trunk_ref
                .as_deref()
                .and_then(|t| git::opt(p, &["merge-base", &tip, t]))
                .is_some_and(|b| config_at(&b) != config_at(&tip));
            eprintln!(
                "5w ci: {}'s .5w.toml is unreadable; this {} {}judged under the one it commits",
                repo.broken_at.as_ref().map_or(&repo.trunk, |(t, _)| t),
                if onto { "push" } else { "change request" },
                if repairs { "repairs it and is " } else { "is " }
            );
            repaired = Repo {
                cwd: repo.cwd.clone(),
                primary: repo.primary.clone(),
                common: repo.common.clone(),
                cfg,
                trunk: repo.trunk.clone(),
                bare: repo.bare,
                pin: repo.pin.clone(),
                broken: None,
                broken_at: repo.broken_at.clone(),
                committed_trunk: repo.committed_trunk.clone(),
            };
            &repaired
        }
        _ => repo,
    };
    // On a server, a trunk that is not there while other branches are means the
    // trunk was guessed wrong: nothing would be judged as landing on it.
    let trunk_head = format!("refs/heads/{}", repo.trunk);
    if repo.bare
        && refname.as_ref().is_some_and(|r| *r != trunk_head)
        && git::rev(p, &trunk_head).is_none()
        && git::opt(
            p,
            &["for-each-ref", "--count=1", "--format=x", "refs/heads/"],
        )
        .is_some_and(|o| !o.is_empty())
    {
        bail!(
            "this server has no {trunk_head} to judge pushes against — `git config 5w.trunk <name>` names the trunk"
        );
    }
    // Unpinned, a server's trunk is its HEAD's branch unless that branch commits
    // another name, so a landed rename would move the gate off it. HEAD is no pin
    // (a stale one would refuse every push): warn, and refuse the rename below.
    let unpinned = match (repo.bare && repo.pin.is_none())
        .then(|| crate::store::head_branch(p))
        .flatten()
    {
        Some(h) => {
            let tip = git::rev(p, &format!("refs/heads/{h}"));
            let gated = gate_settings(repo, tip.as_slice())?.values().any(|on| *on);
            if gated && refname.is_some() {
                eprintln!(
                    "5w ci: warning: no trunk pin on this server — `git config 5w.trunk {h}` keeps a committed rename from moving the gate"
                );
            }
            gated.then_some(h)
        }
        None => None,
    };
    if zeros(&head) {
        // A deletion carries no commits. Under the gate the trunk's is refused: a
        // push re-creating it has no trunk to judge against, and would land anything.
        let trunk_ref = format!("refs/heads/{}", repo.trunk);
        let Some(r) = refname.filter(|r| *r == trunk_ref) else {
            println!("5w ci: a deletion: nothing to check");
            return Ok(());
        };
        let old = base
            .filter(|b| !b.bytes().all(|c| c == b'0'))
            .and_then(|b| git::rev(p, &b));
        if gate_settings(repo, old.as_slice())?.values().any(|on| *on) {
            bail!(
                "deleting {r} is refused under gate_trunk — ship a change that turns gate_trunk off first"
            );
        }
        println!("5w ci: deletion of {r}: nothing to check");
        return Ok(());
    }
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
    // Under the gate the trunk only moves forward: a rewind judges no commits, drops
    // landings the server has, and can reset to before the gate was turned on.
    if let Some(b) = base.as_deref().filter(|_| onto_trunk)
        && !git::ok(p, &["merge-base", "--is-ancestor", b, &head])
        && gate_settings(repo, &[b.to_string()])?
            .values()
            .any(|on| *on)
    {
        bail!(
            "rewinding {} is refused under gate_trunk — ship a change that turns it off first",
            repo.trunk
        );
    }
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
    // A trunk tip whose config does not parse would refuse every later push.
    if onto_trunk
        && !range.is_empty()
        && let Some(text) = git::opt(
            p,
            &["show", &format!("{head}:{}", crate::store::CONFIG_FILE)],
        )
        && let Err(e) = crate::config::Config::from_toml(&text)
    {
        let why = match crate::config::requires_newer(&text) {
            Some(v) if repo.bare => format!(
                "requires 5w {v}, newer than this server's 5w {} — upgrade 5w on the server before pushing it",
                crate::upkeep::VERSION
            ),
            Some(v) => format!(
                "requires 5w {v}, newer than the 5w {} running this check — upgrade it before pushing",
                crate::upkeep::VERSION
            ),
            None => format!(
                "{} — fix it: landed, it would refuse every push to {}",
                e.strip_prefix("config")
                    .unwrap_or(&e)
                    .trim_start_matches(':')
                    .trim_start(),
                repo.trunk
            ),
        };
        problems.push(format!("{}: .5w.toml {why}", short(&head)));
    }
    // A server's pinned trunk stays the trunk (store: `Repo::open`); a push that
    // leaves it committing another name is refused rather than let the two part.
    if let Some((pinned, how)) = repo
        .pin
        .as_ref()
        .filter(|_| onto_trunk && !range.is_empty())
        && let Some(named) = committed_trunk(p, Some(&head)).filter(|t| t != pinned)
    {
        // The fix is where the pin came from: git config does not outrank the variable.
        let fix = if how.starts_with("FIVEW_TRUNK=") {
            format!("FIVEW_TRUNK={named}")
        } else {
            format!("`git config 5w.trunk {named}`")
        };
        problems.push(format!(
            "{}: .5w.toml has trunk = \"{named}\" but this server pins {how} — keep trunk = \"{pinned}\", or {fix} on the server once {named} is its trunk",
            short(&head)
        ));
    }
    // Unpinned and gated, a push to HEAD's branch that commits another trunk name
    // would ungate it: refused, whether or not a landing covers it.
    if let Some(h) = unpinned
        .as_ref()
        .filter(|h| refname.as_deref() == Some(format!("refs/heads/{h}").as_str()))
        && committed_trunk(p, base.as_deref()).is_none_or(|t| t == *h)
        && let Some(named) = committed_trunk(p, Some(&head)).filter(|t| t != h)
    {
        problems.push(format!(
            "{}: .5w.toml renames the trunk {h} to \"{named}\" on a server with no trunk pin — keep trunk = \"{h}\"; renaming is the admin's step: `git config 5w.trunk {named}` first",
            short(&head)
        ));
    }
    if onto_trunk {
        let tip = trunk_ref.as_deref();
        trunk_gate(repo, base.as_deref(), tip, &span, &range, &mut problems)?;
    }

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
        (Some(b), _) => format!("{b} into {into}"),
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
    // A server with no trunk yet judged the branch against nothing: the trunk's own
    // queue commits, pushed alongside it, read as the branch's. The trunk's creation
    // can still be refused after this hook, so it is not taken on trust.
    let queue_edits = format!("queue edits go on {}, not", repo.trunk);
    if repo.bare
        && refname.is_some()
        && off_trunk
        && trunk_ref.is_none()
        && problems.iter().any(|pr| pr.contains(&queue_edits))
    {
        bail!(
            "{} finding(s) — {what}; on a new server push {} first, then this branch",
            problems.len(),
            repo.trunk
        );
    }
    bail!("{} finding(s) — {what}", problems.len())
}

/// The refusal on a repository whose trunk config does not parse, naming the fix.
fn unreadable(repo: &Repo, err: &str, branch: Option<&str>) -> String {
    let fix = if err.contains("requires 5w") {
        format!(
            "upgrade 5w{}",
            if repo.bare { " on the server" } else { "" }
        )
    } else if repo.bare
        && repo.pin.is_none()
        && git::rev(&repo.primary, &format!("refs/heads/{}", repo.trunk)).is_none()
    {
        // It names a trunk this server does not have: pin the one it does.
        let head = crate::store::head_branch(&repo.primary).unwrap_or_else(|| "<name>".into());
        format!(
            "it names trunk {}, which this server has no branch for: `git config 5w.trunk {head}`, then push a commit that fixes .5w.toml to {head}",
            repo.trunk
        )
    } else if let Some(b) = branch {
        format!(
            "fix .5w.toml on {b}, or push a commit that fixes it to {}",
            repo.trunk
        )
    } else {
        format!("push a commit that fixes .5w.toml to {}", repo.trunk)
    };
    format!("{}'s .5w.toml is unreadable — {err} — {fix}", repo.trunk)
}

/// A commit of the pushed span: its tree, parents and subject.
struct Commit {
    tree: String,
    parents: Vec<String>,
    subject: String,
}

/// `gate_trunk`: every commit a push brings to the trunk that adds code is covered
/// by a landing record `5w ship` wrote (README: *What the gate guarantees*). A
/// record is recomputed here from the objects, never trusted: it covers its range
/// only when that range adds what its accepted task's `reviewed:` commit added.
fn trunk_gate(
    repo: &Repo,
    base: Option<&str>,
    tip: Option<&str>,
    span: &str,
    range: &[String],
    out: &mut Vec<String>,
) -> Res<()> {
    let p = &repo.primary;
    if range.is_empty() {
        return Ok(());
    }
    // One line a commit: a subject holds no newline, and the pusher's text comes
    // after the tab, where it cannot pose as another commit.
    let log = git::git(p, &["rev-list", "--format=%H %T %P%x09%s", span])?;
    let mut commits: HashMap<String, Commit> = HashMap::new();
    for l in log.lines().filter(|l| !l.starts_with("commit ")) {
        let (head, subject) = l.split_once('\t').unwrap_or((l, ""));
        let mut w = head.split(' ').filter(|x| !x.is_empty());
        let (Some(sha), Some(tree)) = (w.next(), w.next()) else {
            continue;
        };
        commits.entry(sha.to_string()).or_insert(Commit {
            tree: tree.to_string(),
            parents: w.map(String::from).collect(),
            subject: subject.to_string(),
        });
    }
    let first_parents = |c: &str| commits.get(c).and_then(|c| c.parents.first().cloned());

    // Which commits the gate judges: all of them when the trunk had it on before
    // the push. Else the first-parent line under the setting its first parent's
    // config holds, and what a merge on that line brings in, under the merge's —
    // so the push that enables it is judged from that commit on.
    let line = git::git(p, &["rev-list", "--first-parent", span])?;
    let line: Vec<&str> = line.lines().collect();
    let mut parents: Vec<String> = line.iter().filter_map(|c| first_parents(c)).collect();
    parents.extend(base.map(String::from));
    let on = gate_settings(repo, &parents)?;
    let before = base.is_some_and(|b| on.get(b).copied().unwrap_or(false));
    let in_range: HashSet<&str> = range.iter().map(String::as_str).collect();
    let mut judged: Vec<String> = Vec::new();
    if before {
        judged = range.to_vec();
    }
    for f in line.iter().filter(|_| !before) {
        let Some(fp) = first_parents(f) else { continue };
        if !on.get(&fp).copied().unwrap_or(false) {
            continue;
        }
        judged.push(f.to_string());
        if commits.get(*f).is_some_and(|c| c.parents.len() > 1) {
            let side = git::git(p, &["rev-list", &format!("{fp}..{f}")])?;
            judged.extend(
                side.lines()
                    .filter(|c| c != f && in_range.contains(c))
                    .map(String::from),
            );
        }
    }
    if judged.is_empty() {
        return Ok(());
    }

    let queue_only = |from: &str, to: &str| -> Res<bool> {
        let o = git::git(p, &git::pinned_diff(&["--name-only", "-z", from, to]))?;
        Ok(o.split('\0')
            .filter(|n| !n.is_empty())
            .all(|n| n == repo.cfg.file || n == repo.cfg.archive))
    };
    let prefix = format!("{}: land #", repo.cfg.commit_prefix);
    let record_of = |k: &Commit| -> Option<String> {
        k.subject
            .strip_prefix(&prefix)
            .filter(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
            .map(String::from)
    };
    let mut covered: HashSet<String> = HashSet::new();
    // A record the gate does not judge (its first parent has the gate off) still
    // covers the judged commits it lands, as when a push turns the gate on, then
    // off: it is held to the same checks, and its failure is told only when a
    // judged commit it could have covered, one below it, is left uncovered.
    let judged_set: HashSet<&str> = judged.iter().map(String::as_str).collect();
    let mut unjudged_failures: Vec<(&String, String)> = Vec::new();
    for c in range.iter().filter(|c| !judged_set.contains(c.as_str())) {
        let Some(k) = commits.get(c) else { continue };
        let Some(id) = record_of(k) else { continue };
        match landing(repo, tip, c, k, &id, &commits, &in_range) {
            Ok(landed) => covered.extend(landed),
            Err(why) => unjudged_failures.push((
                c,
                format!("{}: land #{id} covers nothing — {why}", short(c)),
            )),
        }
    }
    let mut need: Vec<&String> = Vec::new();
    for c in &judged {
        let Some(k) = commits.get(c) else { continue };
        if let Some(id) = record_of(k) {
            match landing(repo, tip, c, k, &id, &commits, &in_range) {
                Ok(landed) => covered.extend(landed),
                Err(why) => out.push(format!("{}: land #{id} covers nothing — {why}", short(c))),
            }
        }
        let adds_code = match k.parents.as_slice() {
            [] => true,
            [one] => !queue_only(one, c)?,
            [a, b] => {
                // The tree git would make, conflicts left in: a merge that differs
                // from it only in the queue resolved nothing but the queue.
                let m = git::raw(p, &["merge-tree", "--write-tree", a, b], &[], None)?;
                let tree = m.stdout.lines().next().unwrap_or("");
                let tree_ok =
                    matches!(tree.len(), 40 | 64) && tree.bytes().all(|b| b.is_ascii_hexdigit());
                !tree_ok || !queue_only(tree, c)?
            }
            _ => true,
        };
        if adds_code {
            need.push(c);
        }
    }
    let uncovered: Vec<&String> = need.into_iter().filter(|c| !covered.contains(*c)).collect();
    for (record, why) in unjudged_failures {
        if uncovered
            .iter()
            .any(|u| git::ok(p, &["merge-base", "--is-ancestor", u, record]))
        {
            out.push(why);
        }
    }
    for c in uncovered {
        if let Some(why) = commits.get(c).and_then(|k| unmoved_repair(repo, c, k)) {
            out.push(why);
            continue;
        }
        out.push(format!(
            "{}: code on {} that no landing record covers — ship it from an accepted branch (gate_trunk)",
            short(c),
            repo.trunk
        ));
    }
    Ok(())
}

/// Why commit `c`, uncovered, cannot be shipped either: it changes only the config,
/// over a first parent whose config is broken and renamed the queue or archive in
/// place, restoring the last accepted names (`lint::restored_name`). No landing
/// covers that repair — its record would go in a file the trunk lacks — so it is
/// still refused, naming the admin's push past the hook (store: `unmoved_queue`).
fn unmoved_repair(repo: &Repo, c: &str, k: &Commit) -> Option<String> {
    let (p, t, file) = (&repo.primary, &repo.trunk, crate::store::CONFIG_FILE);
    let [fp] = k.parents.as_slice() else {
        return None;
    };
    let only_config = git::opt(p, &git::pinned_diff(&["--name-only", "-z", fp, c]))?
        .split('\0')
        .filter(|n| !n.is_empty())
        .all(|n| n == file);
    let text = git::opt(p, &["show", &format!("{fp}:{file}")]).filter(|_| only_config)?;
    let e = crate::config::Config::from_toml(&text).err()?;
    let cfg =
        crate::config::Config::from_toml(&git::opt(p, &["show", &format!("{c}:{file}")])?).ok()?;
    let broken = crate::store::broken_names(p, &text, Some(fp));
    let (what, names) = lint::describe_restore(&lint::restored_name(p, fp, &broken, &cfg)?);
    Some(format!(
        "{}: {file} on {t} is broken ({e}) and names {what} — no landing covers its repair: an admin pushes this {names} past the server's hook",
        short(c)
    ))
}

/// Whether `gate_trunk` is on in each commit's `.5w.toml`. A config that does not
/// parse is read as the newest one on its first-parent line that does, and counts
/// as on when there is none: a gate is not lifted by breaking its file. Nor by a
/// key the config rejects: then only a written `gate_trunk = false` is off.
fn gate_settings(repo: &Repo, commits: &[String]) -> Res<HashMap<String, bool>> {
    let p = &repo.primary;
    let mut on = HashMap::new();
    if commits.is_empty() {
        return Ok(on);
    }
    let input: String = commits
        .iter()
        .map(|c| format!("{c}:{}\n", crate::store::CONFIG_FILE))
        .collect();
    let o = git::raw(
        p,
        &["cat-file", "--batch-check=%(objectname) %(objecttype)"],
        &[],
        Some(&input),
    )?;
    let read = |text: &str| says_on(text, "gate_trunk");
    let mut blobs: HashMap<String, Option<bool>> = HashMap::new();
    for (c, l) in commits.iter().zip(o.stdout.lines()) {
        let setting = match l.split_once(' ') {
            Some((oid, "blob")) => {
                let b = match blobs.get(oid) {
                    Some(b) => *b,
                    None => {
                        let text = git::git(p, &["cat-file", "blob", oid]).unwrap_or_default();
                        let b = read(&text);
                        blobs.insert(oid.to_string(), b);
                        b
                    }
                };
                match b {
                    Some(b) => b,
                    None => crate::store::last_readable_config(p, c)
                        .and_then(|text| read(&text))
                        .unwrap_or(true),
                }
            }
            _ => false,
        };
        on.insert(c.clone(), setting);
    }
    Ok(on)
}

/// Whether a config text turns a boolean `key` on, failing closed: a config the
/// parser reads but rejects counts as on unless it says false. None: the text
/// does not parse.
fn says_on(text: &str, key: &str) -> Option<bool> {
    let kv = crate::config::parse_toml(text).ok()?;
    // The last one, as the config reads a key given twice.
    let says = |b: bool| {
        kv.iter()
            .rev()
            .find(|(k, _)| k == key)
            .is_some_and(|(_, v)| matches!(v, crate::config::Val::Bool(x) if *x == b))
    };
    Some(if crate::config::Config::from_toml(text).is_ok() {
        says(true)
    } else {
        !says(false)
    })
}

/// A boolean `key` as `commit`'s config says it, read as `gate_settings` reads
/// `gate_trunk`: text that does not parse by the newest config on its first-parent
/// line that does, on when none does or there is no commit.
pub fn setting_on(p: &std::path::Path, commit: Option<&str>, key: &str) -> bool {
    let Some(c) = commit else { return true };
    match git::opt(p, &["show", &format!("{c}:{}", crate::store::CONFIG_FILE)]) {
        Some(text) => says_on(&text, key)
            .or_else(|| crate::store::last_readable_config(p, c).and_then(|t| says_on(&t, key)))
            .unwrap_or(true),
        None => false,
    }
}

/// The branch a `--trunk` ref names: `refs/heads/<name>`, `refs/remotes/<remote>/<name>`
/// or a short form of either. None for a sha, a tag or a remote's HEAD: nothing to compare.
fn trunk_ref_branch(p: &std::path::Path, given: &str) -> Option<String> {
    let full = git::opt(p, &["rev-parse", "--symbolic-full-name", given])?;
    let name = match full.strip_prefix("refs/heads/") {
        Some(n) => n.to_string(),
        None => {
            let rest = full.strip_prefix("refs/remotes/")?;
            // A remote's name may hold a slash: the longest remote that prefixes it.
            let remotes = git::opt(p, &["remote"]).unwrap_or_default();
            remotes
                .lines()
                .filter_map(|r| rest.strip_prefix(r)?.strip_prefix('/'))
                .min_by_key(|n| n.len())
                .or_else(|| rest.split_once('/').map(|(_, n)| n))?
                .to_string()
        }
    };
    (!name.is_empty() && name != "HEAD").then_some(name)
}

/// The `trunk` a commit's `.5w.toml` names, read as the config reads it.
fn committed_trunk(p: &std::path::Path, commit: Option<&str>) -> Option<String> {
    let text = git::opt(
        p,
        &[
            "show",
            &format!("{}:{}", commit?, crate::store::CONFIG_FILE),
        ],
    )?;
    crate::config::Config::from_toml(&text).ok()?.trunk
}

/// The archive as commit `rev` has it, empty when it has none — unless the trunk's
/// config at `tip` (the trunk this check reads: `--trunk`, else where a broken
/// config was read) is broken and renamed the archive in place, leaving the closed
/// tasks under the name its last accepted config keeps, which `rev` still has.
/// Read as empty, an accepted row would stop naming its task and branch: refused
/// with the repair, as a checkout's reads refuse it (store: `no_archive`). The
/// names here are the trunk's, even under a pushed config that parses.
fn archive_at(repo: &Repo, rev: &str, tip: Option<&str>) -> Res<String> {
    let p = &repo.primary;
    let names = tip.map(|t| trunk_config(p, t));
    let new = names
        .as_ref()
        .map_or(&repo.cfg.archive, |(c, _)| &c.archive);
    if let Some(a) = git::opt(p, &["show", &format!("{rev}:{new}")]) {
        return Ok(a);
    }
    let (Some(tip), Some((broken, Some(e)))) = (tip, names) else {
        return Ok(String::new());
    };
    // Whether `rev` still holds the old archive; the names are those `tip`'s broken
    // config gives (`store::restore_fix`), as a checkout's reads name them, on the
    // trunk they were read on.
    let t = repo.broken_at.as_ref().map_or(&repo.trunk, |(t, _)| t);
    let at_rev = crate::store::last_accepted_config(p, tip).is_some_and(|c| {
        c.archive != broken.archive
            && git::ok(p, &["cat-file", "-e", &format!("{rev}:{}", c.archive)])
    });
    let gate = || setting_on(p, Some(tip), "gate_trunk");
    let e = e.as_str();
    match at_rev
        .then(|| crate::store::restore_fix(p, t, tip, &broken, gate(), e, "archive"))
        .flatten()
    {
        Some(fix) => bail!("{fix}"),
        None => Ok(String::new()),
    }
}

/// The queue file, archive and `require_task` trunk commit `tip` gives, as its gate
/// reads them: its config where it parses; over a broken one (its error given),
/// the names it says (`store::broken_names`) and `require_task` failing closed;
/// with none, the defaults. Never a local branch's or the working tree's config.
fn trunk_config(p: &std::path::Path, tip: &str) -> (crate::config::Config, Option<String>) {
    use crate::config::Config;
    let Some(text) = git::opt(
        p,
        &["show", &format!("{tip}:{}", crate::store::CONFIG_FILE)],
    ) else {
        return (Config::default(), None);
    };
    match Config::from_toml(&text) {
        Ok(c) => (c, None),
        Err(e) => {
            let names = crate::store::broken_names(p, &text, Some(tip));
            let require_task = setting_on(p, Some(tip), "require_task");
            (
                Config {
                    require_task,
                    ..names
                },
                Some(e),
            )
        }
    }
}

/// The commits a landing record covers, when it holds: see `trunk_gate`.
fn landing(
    repo: &Repo,
    trunk: Option<&str>,
    sha: &str,
    k: &Commit,
    id: &str,
    commits: &HashMap<String, Commit>,
    in_range: &HashSet<&str>,
) -> Result<Vec<String>, String> {
    let p = &repo.primary;
    let raw = git::git(p, &["cat-file", "commit", sha])?;
    let body = raw.split_once("\n\n").map(|(_, b)| b).unwrap_or("");
    let landed: Vec<&str> = body
        .lines()
        .filter_map(|l| l.strip_prefix("Landed: "))
        .collect();
    let [range] = landed.as_slice() else {
        return Err("it needs one `Landed: <base>..<tip>` line".into());
    };
    let full = |s: &str| matches!(s.len(), 40 | 64) && s.bytes().all(|b| b.is_ascii_hexdigit());
    let Some((base, tip)) = range
        .trim()
        .split_once("..")
        .filter(|(b, t)| full(b) && full(t))
    else {
        return Err(format!("Landed: {range} is not <full sha>..<full sha>"));
    };
    if k.parents.as_slice() != [tip] {
        return Err(format!("its parent is not {}", short(tip)));
    }
    let tip_tree = match commits.get(tip) {
        Some(t) => t.tree.clone(),
        None => git::git(p, &["rev-parse", &format!("{tip}^{{tree}}")])?,
    };
    if k.tree != tip_tree {
        return Err("it changes files".into());
    }
    if git::rev(p, base).as_deref() != Some(base)
        || base == tip
        || !git::ok(p, &["merge-base", "--is-ancestor", base, tip])
    {
        return Err(format!("{} is not below {}", short(base), short(tip)));
    }
    // Only what this push brings: a range reaching back over commits the trunk
    // already has compares a stale start, and could cover taking a landing back out.
    let landed: Vec<String> = git::git(p, &["rev-list", &format!("{base}..{tip}")])?
        .lines()
        .map(String::from)
        .collect();
    if let Some(old) = landed.iter().find(|c| !in_range.contains(c.as_str())) {
        return Err(format!(
            "{}..{} reaches {}, already on {}",
            short(base),
            short(tip),
            short(old),
            repo.trunk
        ));
    }
    let show = |f: &str| git::opt(p, &["show", &format!("{sha}:{f}")]).unwrap_or_default();
    let all: Vec<Task> = queue::parse(&show(&repo.cfg.file))
        .into_iter()
        .chain(queue::parse(&archive_at(repo, sha, trunk)?))
        .collect();
    let id_n = parse_id(id)?;
    let Some(t) = all.iter().find(|t| t.id == id_n) else {
        return Err(format!("#{id} is not in the queue"));
    };
    let (State::Done, Some("review"), Some(r)) = (t.state, t.via.as_deref(), t.reviewed.as_deref())
    else {
        return Err(format!("#{id} is not accepted via:review"));
    };
    let reviewed = match git::recorded(p, r) {
        git::Recorded::Commit(c) => c,
        other => {
            return Err(format!(
                "#{id}'s reviewed:{} {} — push it with the trunk (`git push <remote> <sha>:refs/5w/reviewed/{id}`)",
                short(r),
                recorded_problem(&other)
            ));
        }
    };
    let now = git::change_id(p, base, tip)?;
    if git::change_id(p, base, &reviewed)? == now
        || crate::ship::landed_under(repo, &all, &reviewed, &now, base, false)?.is_some()
    {
        return Ok(landed);
    }
    Err(format!(
        "{}..{} is not the change #{id} accepted at {}",
        short(base),
        short(tip),
        short(r)
    ))
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
    // The queue as the trunk checked against names it, not as a stale local
    // trunk or the branch's own checkout does.
    let (cfg, broken) = trunk_config(p, trunk);
    // A link's blob is its target's path, which parses as an empty queue.
    let links: Vec<_> = [&cfg.file, &cfg.archive]
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
                repo.broken_at.as_ref().map_or(&repo.trunk, |(t, _)| t)
            ));
        }
        return Ok(());
    }
    // A queue a broken config renamed in place, read as empty, would stop naming
    // the branch's task and pass it as unreviewed: refused with the repair, as the
    // archive is (`archive_at`), on the trunk its names were read on.
    let queue = match git::opt(p, &["show", &format!("{trunk}:{}", cfg.file)]) {
        Some(q) => q,
        None => {
            let t = repo.broken_at.as_ref().map_or(&repo.trunk, |(t, _)| t);
            let gate = setting_on(p, Some(trunk), "gate_trunk");
            if let Some(fix) = broken
                .as_deref()
                .and_then(|e| crate::store::restore_fix(p, t, trunk, &cfg, gate, e, "file"))
            {
                out.push(format!("{branch}: {fix}"));
                return Ok(());
            }
            String::new()
        }
    };
    let archive = match archive_at(repo, trunk, Some(trunk)) {
        Ok(a) => a,
        Err(why) => {
            out.push(format!("{branch}: {why}"));
            return Ok(());
        }
    };
    let rows: Vec<_> = queue::parse(&queue)
        .into_iter()
        .chain(queue::parse(&archive))
        .filter(|t| t.branch.as_deref() == Some(branch))
        .collect();
    if rows.is_empty() {
        if cfg.require_task {
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

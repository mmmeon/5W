//! `5w lint` — check that a change to the queue follows PROTOCOL.md.
//!
//! The tool enforces the protocol by construction; lint is how a hand edit,
//! made without the tool, gets the same checks. It compares the queue before
//! and after (queue file and archive together, so archiving is a move and not a
//! deletion) and judges every row that changed by the transition it made.

use crate::bail;
use crate::config::Config;
use crate::git;
use crate::queue::{self, State, Task};
use crate::store::Repo;
use crate::util::Res;
use std::collections::{BTreeSet, HashMap, HashSet};

pub const USAGE: &str = "\
usage: 5w lint [--staged | <rev> | <from>..<to>]

  --staged       the index against HEAD (what the pre-commit hook runs; the default)
  <rev>          that one commit against its parent
  <from>..<to>   every commit in the range, each against its parent

Checks rows against PROTOCOL.md: legal state changes and what each must carry,
closed rows unchanged, no deletion or reused id, and that a queue edit is its
own commit on the trunk.

  5w hook install | uninstall [pre-commit | pre-receive]";

/// One snapshot: queue and archive text.
struct Snap {
    queue: String,
    archive: String,
}

impl Snap {
    fn tasks(&self) -> (Vec<Task>, Vec<Task>) {
        (queue::parse(&self.queue), queue::parse(&self.archive))
    }
}

fn show(repo: &Repo, spec: &str) -> String {
    git::raw(&repo.primary, &["show", spec], &[], None)
        .ok()
        .filter(|o| o.ok)
        .map(|o| o.stdout)
        .unwrap_or_default()
}

fn at_rev(repo: &Repo, rev: Option<&str>) -> Snap {
    match rev {
        None => Snap {
            queue: String::new(),
            archive: String::new(),
        },
        Some(r) => Snap {
            queue: show(repo, &format!("{r}:{}", repo.cfg.file)),
            archive: show(repo, &format!("{r}:{}", repo.cfg.archive)),
        },
    }
}

pub fn run(repo: &Repo, args: &[String]) -> Res<()> {
    if args.iter().any(|a| a == "-h" || a == "--help") || args.first().is_some_and(|a| a == "help")
    {
        println!("{USAGE}");
        return Ok(());
    }
    if let Some(f) = args.iter().find(|a| a.starts_with('-') && *a != "--staged") {
        return Err(crate::tasks::unknown_flag(repo, "lint", f));
    }
    if args.len() > 1 {
        return Err(format!(
            "lint takes one of --staged | <rev> | <from>..<to> ({} lint --help)",
            repo.cfg.cmd_tasks
        ));
    }
    let arg = args.first().map(|s| s.as_str()).unwrap_or("--staged");
    if let Some(e) = &repo.broken
        && arg != "--staged"
    {
        return Err(e.clone());
    }
    let mut problems = Vec::new();
    match arg {
        "--staged" => {
            let index = git::caller_index();
            let env: Vec<(&str, &str)> = index
                .iter()
                .map(|i| ("GIT_INDEX_FILE", i.as_str()))
                .collect();
            let repaired;
            let repo = match (&repo.broken, committed_config(repo)) {
                (Some(err), _) => {
                    repaired = staged_repair(repo, err, &env)?;
                    &repaired
                }
                (None, Some(Ok(committed))) => {
                    repaired = with_config(repo, committed);
                    &repaired
                }
                // The checkout's copy parses, the committed one does not: this
                // commit is that repair, under the names the trunk still gives.
                (None, Some(Err((err, names)))) => {
                    repaired = staged_repair(&with_config(repo, names), &err, &env)?;
                    &repaired
                }
                (None, None) => repo,
            };
            // Read first, so a marker the checkout no longer needs goes even on
            // a commit that leaves the queue alone.
            let missed = crate::store::missed_marker_fix(repo);
            let o = git::raw(&repo.cwd, &["diff", "--cached", "--name-only"], &env, None)?;
            if !o.ok {
                return Err(format!("git diff --cached: {}", o.stderr.trim()));
            }
            let files = o.stdout;
            let files: Vec<&str> = files.lines().collect();
            if !touches_queue(&repo.cfg, &files) {
                return Ok(());
            }
            let staged_entry = |name: &str| {
                let pathspec = format!(":(top){name}");
                let l = git::opt(&repo.cwd, &["ls-files", "-s", "--", &pathspec])?;
                let mut f = l.split_whitespace();
                Some((f.next()?.to_string(), f.next()?.to_string()))
            };
            let new = Snap {
                queue: show_index(repo, &env, &repo.cfg.file),
                archive: show_index(repo, &env, &repo.cfg.archive),
            };
            // A merge being committed is judged as `lint <rev>` will judge it.
            let merging = git::opt(&repo.cwd, &["rev-parse", "--git-path", "MERGE_HEAD"])
                .and_then(|p| std::fs::read_to_string(repo.cwd.join(p)).ok());
            if let (Some(head), Some(m)) = (git::rev(&repo.cwd, "HEAD"), merging) {
                let parents: Vec<String> = std::iter::once(head)
                    .chain(m.split_whitespace().map(String::from))
                    .collect();
                merge(repo, &parents, &new, &staged_entry, "staged", &mut problems);
                return report(problems);
            }
            shape(
                repo,
                &files,
                git::current_branch(&repo.cwd).as_deref(),
                "staged",
                &mut problems,
            );
            let head = git::rev(&repo.cwd, "HEAD");
            links(
                repo,
                |name| head.as_deref().and_then(|h| tree_entry(repo, h, name)),
                staged_entry,
                "staged",
                &mut problems,
            );
            let mut old = at_rev(repo, head.as_deref());
            unlinked(repo, head.as_deref(), &mut old, |name| {
                let pathspec = format!(":(top){name}");
                git::opt(&repo.cwd, &["ls-files", "-s", "--", &pathspec])
                    .is_some_and(|l| !l.is_empty() && !l.starts_with("120000 "))
            });
            check(&repo.cfg, repo, &old, &new, None, "staged", &mut problems);
            unarchived(repo, &old, &new, None, missed, "staged", &mut problems);
        }
        range => {
            // Resolve each side to a plain sha (git::rev refuses `^HEAD`).
            let commit = |r: &str| {
                git::rev(&repo.cwd, if r.is_empty() { "HEAD" } else { r }).ok_or_else(|| {
                    format!(
                        "lint: {range} is not a commit or a <from>..<to> range ({} lint --help)",
                        repo.cfg.cmd_tasks
                    )
                })
            };
            let list: Vec<String> = match range.split_once("..") {
                Some((from, to)) => {
                    let (sep, to) = match to.strip_prefix('.') {
                        Some(to) => ("...", to),
                        None => ("..", to),
                    };
                    let spec = format!("{}{sep}{}", commit(from)?, commit(to)?);
                    git::git(&repo.cwd, &["rev-list", "--reverse", &spec])?
                        .lines()
                        .map(String::from)
                        .collect()
                }
                None => vec![commit(range)?],
            };
            let judged;
            let repo = match committed_rules(repo) {
                Some(cfg) => {
                    judged = with_config(repo, cfg);
                    &judged
                }
                None => repo,
            };
            let trunk = repo.trunk.clone();
            commits_on(
                repo,
                &list,
                &|c| git::ok(&repo.cwd, &["merge-base", "--is-ancestor", c, &trunk]),
                &mut problems,
            )?;
        }
    }
    report(problems)
}

fn report(problems: Vec<String>) -> Res<()> {
    if problems.is_empty() {
        return Ok(());
    }
    for p in &problems {
        eprintln!("  {p}");
    }
    bail!("{} protocol violation(s) — see PROTOCOL.md", problems.len())
}

/// Lint each commit against its parent. `on_trunk` says whether a commit is
/// (or is landing) on the trunk — asked of git locally, told by the caller in CI
/// and in a pre-receive hook, where the ref has not moved yet.
pub fn commits_on(
    repo: &Repo,
    commits: &[String],
    on_trunk: &dyn Fn(&str) -> bool,
    problems: &mut Vec<String>,
) -> Res<()> {
    for c in commits {
        let c = c.as_str();
        let short = &c[..c.len().min(12)];
        let line = git::git(&repo.cwd, &["rev-list", "--parents", "-n", "1", c])?;
        let parents: Vec<String> = line.split_whitespace().skip(1).map(String::from).collect();
        if parents.len() > 1 {
            let new = at_rev(repo, Some(c));
            merge(
                repo,
                &parents,
                &new,
                &|name| tree_entry(repo, c, name),
                short,
                problems,
            );
            continue;
        }
        let files = git::git(
            &repo.cwd,
            &[
                "diff-tree",
                "--no-commit-id",
                "--name-only",
                "-r",
                "--root",
                c,
            ],
        )?;
        let files: Vec<&str> = files.lines().collect();
        unarchive_elsewhere(repo, c, short, &files, problems)?;
        if !touches_queue(&repo.cfg, &files) {
            continue;
        }
        shape(
            repo,
            &files,
            on_trunk(c).then_some(repo.trunk.as_str()),
            short,
            problems,
        );
        let parent = git::rev(&repo.cwd, &format!("{c}^"));
        links(
            repo,
            |name| parent.as_deref().and_then(|p| tree_entry(repo, p, name)),
            |name| tree_entry(repo, c, name),
            short,
            problems,
        );
        let mut old = at_rev(repo, parent.as_deref());
        unlinked(repo, parent.as_deref(), &mut old, |name| {
            tree_entry(repo, c, name).is_some_and(|(m, _)| m != LINK)
        });
        let new = at_rev(repo, Some(c));
        let subject = git::git(&repo.cwd, &["log", "-1", "--format=%s", c])?;
        check(
            &repo.cfg,
            repo,
            &old,
            &new,
            Some(subject.trim()),
            short,
            problems,
        );
        unarchived(
            repo,
            &old,
            &new,
            Some(subject.trim()),
            None,
            short,
            problems,
        );
    }
    Ok(())
}

/// The ids an unarchive commit names: its subject is `<prefix>: unarchive #<id>`,
/// or several joined by `, ` as a batch's are. Not an edit 5w makes, so not a verb
/// of `single_edit` or `batch_edits`.
fn unarchive_ids(prefix: &str, subject: &str) -> Option<BTreeSet<u64>> {
    let rest = subject.strip_prefix(prefix)?.strip_prefix(": ")?;
    rest.split(", ")
        .map(|part| {
            let id = part.strip_prefix("unarchive #")?;
            let digits = !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit());
            digits.then(|| id.parse().ok()).flatten()
        })
        .collect()
}

/// An unarchive subject on a commit that leaves the queue files alone: it moves
/// no row, so it names rows it does not move.
fn unarchive_elsewhere(
    repo: &Repo,
    c: &str,
    at: &str,
    files: &[&str],
    out: &mut Vec<String>,
) -> Res<()> {
    if touches_queue(&repo.cfg, files) {
        return Ok(());
    }
    let subject = git::git(&repo.cwd, &["log", "-1", "--format=%s", c])?;
    for id in unarchive_ids(&repo.cfg.commit_prefix, subject.trim()).unwrap_or_default() {
        out.push(format!(
            "{at} #{id}: named by an unarchive commit that does not move it from {} back to {}",
            repo.cfg.archive, repo.cfg.file
        ));
    }
    Ok(())
}

/// A closed row the old archive holds that the new queue holds instead. Only an
/// unarchive commit moves one back, unchanged, naming exactly the rows it moves
/// and changing none — the way to reopen an archived row. Anything else is what a
/// trunk checkout that missed an archive stages: committed, it takes the archive
/// (or its new rows) off the trunk.
///
/// A staged change has no subject yet, and its content cannot tell the two
/// apart, so it is flagged only while `missed` holds a missed commit's fix (see
/// `store::missed_marker_fix`), naming it; the commit itself is judged by its
/// subject.
fn unarchived(
    repo: &Repo,
    old: &Snap,
    new: &Snap,
    subject: Option<&str>,
    missed: Option<String>,
    at: &str,
    out: &mut Vec<String>,
) {
    let (oq, oa) = old.tasks();
    let (nq, na) = new.tasks();
    let back: BTreeSet<u64> = oa
        .iter()
        .filter(|t| nq.iter().any(|n| n.id == t.id) && !na.iter().any(|n| n.id == t.id))
        .map(|t| t.id)
        .collect();
    let named = subject.and_then(|s| unarchive_ids(&repo.cfg.commit_prefix, s));
    let say = |out: &mut Vec<String>, id: u64, why: &str| {
        out.push(format!(
            "{at} #{id}: archived in {}, back in {} — {why}",
            repo.cfg.archive, repo.cfg.file
        ))
    };
    match (subject, named) {
        (None, _) => {
            if back.is_empty() {
                return;
            }
            // No marker: a deliberate move, judged by its subject once committed.
            let Some(fix) = missed else { return };
            let why = format!(
                "the trunk checkout missed a commit to {}, and `{fix}` catches it up",
                repo.trunk
            );
            for id in back {
                say(out, id, &why);
            }
        }
        (Some(_), Some(named)) => {
            for &id in back.difference(&named) {
                say(out, id, "an unarchive commit names each row it moves back");
            }
            for id in named.difference(&back) {
                out.push(format!(
                    "{at} #{id}: named by an unarchive commit that does not move it from {} back to {}",
                    repo.cfg.archive, repo.cfg.file
                ));
            }
            let (o, n) = ([&oq[..], &oa[..]].concat(), [&nq[..], &na[..]].concat());
            let changed = queue::changed_ids(&queue::by_id(&o), &queue::by_id(&n));
            for id in changed {
                out.push(format!(
                    "{at} #{id}: an unarchive commit moves rows back unchanged and edits none"
                ));
            }
        }
        (Some(_), None) => {
            for id in back {
                say(
                    out,
                    id,
                    &format!(
                        "an archived row stays archived, unless moved back unchanged by a `{}: unarchive #{id}` commit",
                        repo.cfg.commit_prefix
                    ),
                );
            }
        }
    }
}

/// A merge lists no changed file, so judge the tree it makes (`new`, whose
/// entries `new_entry` gives) against its first parent — the trunk's side — row by row:
///
/// - a row as the first parent holds it passes (it is no change to the trunk);
/// - a row as another parent holds it passes where the first parent left that
///   row as it was at one of their merge bases;
/// - a row both sides changed since a merge base is a resolved conflict: it must
///   merge the two field by field (`three_way`) — or be the side's close, which
///   wins over edits made while open — and is judged as a change from the parent
///   whose state it keeps;
/// - a row no parent holds is an add (a colliding id renumbered past every
///   parent's highest), judged as one;
/// - anything else — a row dropped, or edited as neither side had it — is judged
///   against the first parent and flagged as a queue edit inside a merge.
///
/// `lint --staged` judges a merge being committed the same way.
fn merge(
    repo: &Repo,
    parents: &[String],
    new: &Snap,
    new_entry: &dyn Fn(&str) -> Option<(String, String)>,
    at: &str,
    out: &mut Vec<String>,
) {
    let names = [&repo.cfg.file, &repo.cfg.archive];
    if names
        .iter()
        .all(|n| tree_entry(repo, &parents[0], n) == new_entry(n))
    {
        return;
    }
    links(
        repo,
        |name| tree_entry(repo, &parents[0], name),
        new_entry,
        at,
        out,
    );
    let is_file = |name: &str| new_entry(name).is_some_and(|(m, _)| m != LINK);
    let snap = |rev: &str| {
        let mut s = at_rev(repo, Some(rev));
        unlinked(repo, Some(rev), &mut s, is_file);
        s
    };
    let parse = |s: &Snap| [queue::parse(&s.queue), queue::parse(&s.archive)];
    let snaps: Vec<Snap> = parents.iter().map(|p| snap(p)).collect();
    let rows: Vec<[Vec<Task>; 2]> = snaps.iter().map(parse).collect();
    // Every merge base of the first parent with each other one, as rows: a
    // criss-cross history has several, and git's pick among them is arbitrary.
    let bases: Vec<Vec<[Vec<Task>; 2]>> = parents
        .iter()
        .map(|p| {
            git::opt(&repo.cwd, &["merge-base", "--all", &parents[0], p])
                .unwrap_or_default()
                .lines()
                .map(|b| parse(&snap(b)))
                .collect()
        })
        .collect();
    let [nq, na] = parse(new);
    let new_ids: HashSet<u64> = nq.iter().chain(&na).map(|t| t.id).collect();
    let old_max = snaps
        .iter()
        .map(|s| queue::max_id(&s.queue).max(queue::max_id(&s.archive)))
        .max()
        .unwrap_or(0);
    let mut old: HashMap<u64, &Task> = HashMap::new();
    let mut own = BTreeSet::new();
    let mut close_hint = BTreeSet::new();
    for (file, n) in nq.iter().map(|t| (0, t)).chain(na.iter().map(|t| (1, t))) {
        let id = n.id;
        let first = find_row(&rows[0], id);
        // Parent k's row is as it was at a merge base of k with the first parent.
        let at_base = |k: usize, r| bases[k].iter().any(|b| same_row(r, find_row(b, id)));
        if same_row(first, Some((file, n))) {
            old.insert(id, n);
            continue;
        }
        let Some(before) = rows.iter().find_map(|r| find_row(r, id)) else {
            continue; // an add
        };
        let mut merged = false;
        for k in 1..parents.len() {
            let side = find_row(&rows[k], id);
            if same_row(side, Some((file, n))) && at_base(k, first) {
                // The side's change, the first parent's row as it was.
                old.insert(id, n);
                merged = true;
                break;
            }
            let both = side.is_some() && !at_base(k, first) && !at_base(k, side);
            // A close wins over edits the other side made to the row while open:
            // a closed row does not change, so a resolution cannot keep both.
            let closes = |a: Option<(usize, &Task)>, b: Option<(usize, &Task)>| {
                a.is_some_and(|(_, t)| t.state == State::Done)
                    && bases[k].iter().any(|base| {
                        let at = |r: Option<(usize, &Task)>| r.map(|(_, t)| t.state);
                        at(find_row(base, id)) == at(b)
                    })
            };
            if both && same_row(side, Some((file, n))) && closes(side, first) {
                old.insert(id, n);
                merged = true;
                break;
            }
            if both
                && bases[k]
                    .iter()
                    .any(|b| three_way(find_row(b, id), first, side, (file, n)))
            {
                // A conflict resolved field by field: judged as a change from
                // the parent whose state it keeps, the first when both do.
                let keeps = |r: Option<(usize, &Task)>| {
                    r.is_some_and(|(f, t)| f == file && t.state == n.state)
                };
                let prev = if keeps(first) || !keeps(side) {
                    first
                } else {
                    side
                };
                if let Some((_, t)) = prev {
                    old.insert(id, t);
                }
                if closes(side, first) || closes(first, side) {
                    close_hint.insert(id);
                }
                merged = true;
                break;
            }
        }
        if !merged {
            old.insert(id, first.unwrap_or(before).1);
            own.insert(id);
        }
    }
    for t in rows.iter().flat_map(|r| r.iter().flatten()) {
        if !new_ids.contains(&t.id) {
            old.entry(t.id).or_insert(t);
        }
    }
    let start = out.len();
    judge(&repo.cfg, repo, &old, old_max, new, None, at, out);
    for id in close_hint {
        let changed = format!("{at} #{id}: a closed row changed — reopen it first");
        for f in out[start..].iter_mut().filter(|f| **f == changed) {
            f.push_str(" (take the closed side's row whole)");
        }
    }
    if !own.is_empty() {
        out.push(format!(
            "{at}: a merge changes {} against its first parent — a queue edit is its own commit, on {}",
            own.iter()
                .map(|i| format!("#{i}"))
                .collect::<Vec<_>>()
                .join(" "),
            repo.trunk
        ));
    }
}

/// Whether `n`, in `file`, merges `a` and `s` from `base` field by field: a
/// field neither side changed keeps the base's value, one a single side changed
/// takes that side's, and one both changed takes either side's.
fn three_way(
    base: Option<(usize, &Task)>,
    a: Option<(usize, &Task)>,
    s: Option<(usize, &Task)>,
    (file, n): (usize, &Task),
) -> bool {
    let (Some((fa, a)), Some((fs, s))) = (a, s) else {
        return false;
    };
    fn pick<T: PartialEq>(b: Option<&T>, a: &T, s: &T, n: &T) -> bool {
        match b {
            Some(b) if a == b => n == s,
            Some(b) if s == b => n == a,
            _ => n == a || n == s,
        }
    }
    let b = base.map(|(_, t)| t);
    pick(base.map(|(f, _)| f).as_ref(), &fa, &fs, &file)
        && pick(b.map(|t| &t.state), &a.state, &s.state, &n.state)
        && pick(b.map(|t| &t.text), &a.text, &s.text, &n.text)
        && pick(b.map(|t| &t.body), &a.body, &s.body, &n.body)
        && pick(b.map(|t| &t.area), &a.area, &s.area, &n.area)
        && pick(b.map(|t| &t.level), &a.level, &s.level, &n.level)
        && pick(b.map(|t| &t.lane), &a.lane, &s.lane, &n.lane)
        && pick(b.map(|t| &t.needs), &a.needs, &s.needs, &n.needs)
        && pick(b.map(|t| &t.branch), &a.branch, &s.branch, &n.branch)
        && pick(b.map(|t| &t.rework), &a.rework, &s.rework, &n.rework)
        && pick(b.map(|t| &t.via), &a.via, &s.via, &n.via)
        && pick(
            b.map(|t| &t.submitted),
            &a.submitted,
            &s.submitted,
            &n.submitted,
        )
        && pick(
            b.map(|t| &t.reviewed),
            &a.reviewed,
            &s.reviewed,
            &n.reviewed,
        )
}

/// A row by id in a queue and archive, with the file (0 queue, 1 archive) it sits in.
fn find_row(r: &[Vec<Task>; 2], id: u64) -> Option<(usize, &Task)> {
    (0..2).find_map(|f| r[f].iter().find(|t| t.id == id).map(|t| (f, t)))
}

/// The same row in the same file, or absent from both.
fn same_row(a: Option<(usize, &Task)>, b: Option<(usize, &Task)>) -> bool {
    match (a, b) {
        (Some((fa, a)), Some((fb, b))) => fa == fb && a.state == b.state && queue::identical(a, b),
        (a, b) => a.is_none() && b.is_none(),
    }
}

/// Judge one queue change given the texts on either side of it — for `audit`,
/// which has rebuilt them from history and need not ask git again.
pub fn check_texts(
    repo: &Repo,
    old: [String; 2],
    new: [String; 2],
    subject: &str,
    at: &str,
    out: &mut Vec<String>,
) {
    let [queue, archive] = old;
    let old = Snap { queue, archive };
    let [queue, archive] = new;
    let new = Snap { queue, archive };
    check(&repo.cfg, repo, &old, &new, Some(subject), at, out);
}

/// The config the trunk commits, which a staged queue edit is judged under as
/// `ci` judges it — not the trunk checkout's working copy, whose uncommitted edit
/// (a lane's kind, `default_lane`) would otherwise let a close skip review. None:
/// the trunk commits no config to judge by. Err: it does not parse — its error,
/// and the queue names it still gives, as the server reads them (`store::broken_names`).
/// The trunk is the one committed state names: an uncommitted `trunk` edit does
/// not send the rules to another branch's config.
fn committed_config(repo: &Repo) -> Option<Result<Config, (String, Config)>> {
    let (_, cfg) = committed_config_text(repo)?;
    Some(cfg)
}

type Committed = Result<Config, (String, Config)>;

/// `committed_config`, with the text the trunk commits.
fn committed_config_text(repo: &Repo) -> Option<(String, Committed)> {
    let file = crate::store::CONFIG_FILE;
    let trunk = &repo.committed_trunk;
    let tip = [
        format!("refs/heads/{trunk}"),
        format!("refs/remotes/origin/{trunk}"),
    ]
    .iter()
    .find_map(|r| git::rev(&repo.primary, r))?;
    let text = git::opt(&repo.primary, &["show", &format!("{tip}:{file}")])?;
    let cfg = Config::from_toml(&text).map_err(|e| {
        (
            e,
            crate::store::broken_names(&repo.primary, &text, Some(&tip)),
        )
    });
    Some((text, cfg))
}

/// The config queue rules are judged under in a checkout: the one its trunk
/// commits, as the hook and `ci` read it, not the trunk checkout's working copy.
/// None: judge under `repo`'s own — the trunk commits no config (adoption), or
/// one that does not parse (the repair `Repo::open_for_repair` reads by).
pub fn committed_rules(repo: &Repo) -> Option<Config> {
    if repo.broken.is_some() {
        return None;
    }
    committed_config(repo)?.ok()
}

/// `repo` as queue commands read and write it: under the config its trunk
/// commits (`committed_rules`), so an uncommitted edit to the trunk checkout's
/// copy renames no lane, section, prefix or queue file a write commits by —
/// None: `repo` itself — and, when that copy differs, the note that says so.
pub fn under_committed_rules(repo: &Repo) -> (Option<Repo>, Option<String>) {
    if repo.broken.is_some() {
        return (None, None);
    }
    let Some((text, Ok(mut cfg))) = committed_config_text(repo) else {
        return (None, None);
    };
    // As git compares them: a checkout's CRLF line ends (`core.autocrlf`) are no edit.
    let plain = |t: &str| t.replace("\r\n", "\n").trim().to_string();
    let edited = (repo.cfg.checkout_text.as_deref()).filter(|t| plain(t) != plain(&text));
    let note = edited.map(|_| {
        format!(
            "{} on {} has uncommitted edits; 5w reads the committed one — commit it first",
            crate::store::CONFIG_FILE,
            repo.committed_trunk
        )
    });
    cfg.checkout_text = repo.cfg.checkout_text.clone();
    (Some(with_config(repo, cfg)), note)
}

/// `under_committed_rules`' note, held from the start of a command until it
/// changes something (`say_note`) or ends (`noted`).
static NOTE: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

fn take_note() -> Option<String> {
    NOTE.lock().map(|mut n| n.take()).unwrap_or_default()
}

/// Hold `under_committed_rules`' note for the command about to run.
pub fn hold_note(note: Option<String>) {
    if let (Some(n), Ok(mut held)) = (note, NOTE.lock()) {
        *held = Some(n);
    }
}

/// Print the held note, once: before a command's first change — a queue write,
/// a worktree, a link, the install command, a rebase — so it is read before
/// what the committed config does, not after.
pub fn say_note() {
    if let Some(n) = take_note() {
        eprintln!("5w: {n}");
    }
}

/// `res` with the held note, if nothing printed it yet: on stderr after a
/// success, and on a refusal's one line unless it already names the
/// uncommitted config.
pub fn noted(res: Res<()>) -> Res<()> {
    match (res, take_note()) {
        (Ok(()), Some(n)) => {
            eprintln!("5w: {n}");
            Ok(())
        }
        (Err(e), Some(n)) if !e.contains("uncommitted") => Err(format!("{e} (note: {n})")),
        (res, _) => res,
    }
}

/// `repo` judged under `cfg`, on the trunk committed state names.
fn with_config(repo: &Repo, cfg: Config) -> Repo {
    Repo {
        cwd: repo.cwd.clone(),
        primary: repo.primary.clone(),
        common: repo.common.clone(),
        cfg,
        trunk: repo.committed_trunk.clone(),
        bare: repo.bare,
        pin: repo.pin.clone(),
        broken: None,
        committed_trunk: repo.committed_trunk.clone(),
    }
}

/// A checkout whose trunk config does not parse commits only its repair: an
/// index whose `.5w.toml` parses (or has none), judged under that config with
/// the queue names the broken one gives (`Repo::open_lenient`), which it must keep
/// unless it restores a queue or archive renamed in place (`restores_accepted_names`).
fn staged_repair(repo: &Repo, err: &str, env: &[(&str, &str)]) -> Res<Repo> {
    let file = crate::store::CONFIG_FILE;
    if err.contains("requires 5w") {
        return Err(err.to_string());
    }
    let staged = git::raw(&repo.cwd, &["show", &format!(":{file}")], env, None)?;
    let cfg = match staged.ok {
        true => Config::from_toml(&staged.stdout).ok(),
        false => Some(Config::default()),
    };
    let Some(cfg) = cfg else {
        bail!(
            "{}'s {file} is unreadable — {err} — stage a {file} that parses to commit its repair",
            repo.trunk
        )
    };
    // One that keeps every broken name is taken as any repair is: the refusal
    // names the other, which restores a queue or archive renamed in place.
    let restores = restores_accepted_names(repo);
    if kept(&repo.cfg, &[], &cfg).is_some()
        && let Some((key, want)) = kept(&repo.cfg, &restores, &cfg)
    {
        bail!(
            "a commit repairing {}'s {file} keeps {key} = {want:?}: stage it so, and rename in a later commit",
            repo.trunk
        );
    }
    Ok(with_config(repo, cfg))
}

/// A break that renamed the queue or its archive in place — the trunk has no file
/// under the name its broken config gives, and has one under the name its last
/// accepted config gave — is repaired by a config restoring the names
/// `names_to_restore` gives for the trunk's tip, keeping the others. Anything this
/// cannot read keeps the broken names.
fn restores_accepted_names(repo: &Repo) -> Vec<Restore> {
    trunk_tip(&repo.primary, &repo.trunk)
        .map(|tip| names_to_restore(&repo.primary, &tip, &repo.cfg))
        .unwrap_or_default()
}

/// A name a repair restores: its key, the name the broken config gives, the name
/// the last accepted config gave.
pub(crate) type Restore = (&'static str, String, String);

/// The trunk's tip commit, or origin's copy when there is no local branch.
pub(crate) fn trunk_tip(dir: &std::path::Path, t: &str) -> Option<String> {
    [
        format!("refs/heads/{t}"),
        format!("refs/remotes/origin/{t}"),
    ]
    .iter()
    .find_map(|r| git::rev(dir, r))
}

/// The names a repair of commit `tip`, whose broken config gives the names in
/// `broken`, restores: each of the queue and archive renamed in place (`tip` has
/// no file under its broken name and one under the last accepted config's), and,
/// when there is one, the commit prefix. A name whose file moved, or that names no
/// file either way, stays.
pub(crate) fn names_to_restore(dir: &std::path::Path, tip: &str, broken: &Config) -> Vec<Restore> {
    let Some(last) = crate::store::last_accepted_config(dir, tip) else {
        return Vec::new();
    };
    let has = |name: &str| git::ok(dir, &["cat-file", "-e", &format!("{tip}:{name}")]);
    let mut restores: Vec<Restore> = [
        ("file", &broken.file, &last.file),
        ("archive", &broken.archive, &last.archive),
    ]
    .into_iter()
    .filter(|(_, new, old)| old != new && !has(new) && has(old))
    .map(|(key, new, old)| (key, new.clone(), old.clone()))
    .collect();
    if !restores.is_empty() && broken.commit_prefix != last.commit_prefix {
        restores.push((
            "commit_prefix",
            broken.commit_prefix.clone(),
            last.commit_prefix,
        ));
    }
    restores
}

/// Whether `cfg` restores, over commit `tip` whose broken config gives the names
/// in `broken`, a queue or archive renamed in place: the names it restores
/// (`names_to_restore`), when it gives exactly those back and keeps the rest.
pub(crate) fn restored_name(
    dir: &std::path::Path,
    tip: &str,
    broken: &Config,
    cfg: &Config,
) -> Option<Vec<Restore>> {
    let restores = names_to_restore(dir, tip, broken);
    (!restores.is_empty() && kept(broken, &restores, cfg).is_none()).then_some(restores)
}

/// The first name `cfg` does not give as a repair restoring `restores` over the
/// names in `broken` must, as (key, the name it must give).
fn kept<'a>(
    broken: &'a Config,
    restores: &'a [Restore],
    cfg: &Config,
) -> Option<(&'static str, &'a str)> {
    [
        ("file", &broken.file, &cfg.file),
        ("archive", &broken.archive, &cfg.archive),
        ("commit_prefix", &broken.commit_prefix, &cfg.commit_prefix),
    ]
    .into_iter()
    .map(|(key, old, new)| {
        let want = restores
            .iter()
            .find(|(k, ..)| *k == key)
            .map_or(old.as_str(), |(.., accepted)| accepted.as_str());
        (key, want, new)
    })
    .find(|(_, want, new)| want != new)
    .map(|(key, want, _)| (key, want))
}

/// A repair as refusals name it: what the broken config names ("the queue Q.md,
/// not TASKS.md"), and the names to give back (`file = "TASKS.md", …`).
pub(crate) fn describe_restore(restores: &[Restore]) -> (String, String) {
    let what = restores
        .iter()
        .filter_map(|(key, new, old)| {
            let kind = match *key {
                "file" => "queue",
                "archive" => "archive",
                _ => return None,
            };
            Some(format!("the {kind} {new}, not {old}"))
        })
        .collect::<Vec<_>>()
        .join(" and ");
    let names = restores
        .iter()
        .map(|(key, _, old)| format!("{key} = {old:?}"))
        .collect::<Vec<_>>()
        .join(", ");
    (what, names)
}

/// A file as the index `env` names (the caller's, see `git::caller_index`) holds it.
fn show_index(repo: &Repo, env: &[(&str, &str)], name: &str) -> String {
    git::raw(&repo.cwd, &["show", &format!(":{name}")], env, None)
        .ok()
        .filter(|o| o.ok)
        .map(|o| o.stdout)
        .unwrap_or_default()
}

/// A path's mode and blob in a commit's tree.
fn tree_entry(repo: &Repo, rev: &str, name: &str) -> Option<(String, String)> {
    let l = git::opt(&repo.cwd, &["ls-tree", "--full-tree", rev, "--", name])?;
    let mut f = l.split_whitespace();
    let mode = f.next()?.to_string();
    f.next()?;
    Some((mode, f.next()?.to_string()))
}

const LINK: &str = "120000";

/// A change that turns a symlinked queue file back into a file is judged against
/// the file as the first-parent history last held it, not the link, whose target
/// path reads as an empty queue — else a restore that drops rows passes.
fn unlinked(repo: &Repo, old: Option<&str>, snap: &mut Snap, is_file: impl Fn(&str) -> bool) {
    let Some(old) = old else { return };
    for (name, is_queue) in [(&repo.cfg.file, true), (&repo.cfg.archive, false)] {
        let was_link = tree_entry(repo, old, name).is_some_and(|(m, _)| m == LINK);
        if !was_link || !is_file(name) {
            continue;
        }
        let log = git::opt(
            &repo.cwd,
            &["log", "--first-parent", "--format=%H", old, "--", name],
        )
        .unwrap_or_default();
        let plain = log
            .lines()
            .map(|c| (c, tree_entry(repo, c, name)))
            .take_while(|(_, e)| e.is_some())
            .find(|(_, e)| e.as_ref().is_some_and(|(m, _)| m != LINK))
            .map(|(c, _)| show(repo, &format!("{c}:{name}")))
            .unwrap_or_default();
        if is_queue {
            snap.queue = plain;
        } else {
            snap.archive = plain;
        }
    }
}

/// The queue file and archive are files: a change that makes either a symlink or
/// points one elsewhere is a queue edit no row shows —
/// retargeted, it would aim queue writes at another file.
fn links(
    repo: &Repo,
    old: impl Fn(&str) -> Option<(String, String)>,
    new: impl Fn(&str) -> Option<(String, String)>,
    at: &str,
    out: &mut Vec<String>,
) {
    for name in [&repo.cfg.file, &repo.cfg.archive] {
        let (old, new) = (old(name), new(name));
        let is_link = |e: &Option<(String, String)>| e.as_ref().is_some_and(|(m, _)| m == LINK);
        let what = match (is_link(&old), is_link(&new)) {
            (false, true) => "makes it a symlink",
            (true, true) if old != new => "points its symlink elsewhere",
            _ => continue,
        };
        out.push(format!(
            "{at}: {name} is the queue file itself — this {what}; keep it a plain file"
        ));
    }
}

fn touches_queue(cfg: &Config, files: &[&str]) -> bool {
    files.iter().any(|f| *f == cfg.file || *f == cfg.archive)
}

/// A queue edit is its own commit, on the trunk.
fn shape(repo: &Repo, files: &[&str], branch: Option<&str>, at: &str, out: &mut Vec<String>) {
    let cfg = &repo.cfg;
    let others: Vec<&&str> = files
        .iter()
        .filter(|f| {
            **f != cfg.file
                && **f != cfg.archive
                && **f != crate::store::CONFIG_FILE
                && **f != "PROTOCOL.md"
        })
        .collect();
    if !others.is_empty() {
        out.push(format!(
            "{at}: a queue edit is its own commit — also touches {}",
            others
                .iter()
                .take(3)
                .map(|f| f.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if branch != Some(repo.trunk.as_str()) {
        out.push(format!(
            "{at}: queue edits go on {}, not {}",
            repo.trunk,
            branch.unwrap_or("a branch")
        ));
    }
}

/// Words of a row's title and body, fields and pure punctuation dropped — what
/// must survive a reflow such as `5w split`.
fn words(t: &Task) -> Vec<String> {
    std::iter::once(t.text.as_str())
        .chain(t.body.iter().map(|s| s.as_str()))
        .flat_map(|s| s.split_whitespace())
        .map(|w| w.trim_end_matches('…').to_string())
        .filter(|w| w.chars().any(|c| c.is_alphanumeric()))
        .collect()
}

/// Every field of a row, as one comparable value.
fn fields(t: &Task) -> String {
    format!(
        "{:?}",
        (
            &t.area,
            t.level,
            &t.lane,
            &t.needs,
            &t.branch,
            &t.rework,
            &t.via,
            &t.submitted,
            &t.reviewed
        )
    )
}

/// The verbs a queue commit's subject uses for an edit of one row.
const VERBS: &[&str] = &[
    "add", "set", "submit", "accept", "reject", "close", "reopen",
];

/// The edits a `5w batch` commit names: its subject is `<prefix>: <verb> #<id>`
/// for each, joined by `, ` (`chore(tasks): accept #4, reject #5`). `None` for
/// any other subject, a single edit's own message included.
pub fn batch_edits<'a>(prefix: &str, subject: &'a str) -> Option<Vec<(&'a str, u64)>> {
    let rest = subject.strip_prefix(prefix)?.strip_prefix(": ")?;
    let edits = rest
        .split(", ")
        .map(|part| {
            let (verb, id) = part.split_once(" #")?;
            let digits = !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit());
            (VERBS.contains(&verb) && digits).then_some((verb, id.parse().ok()?))
        })
        .collect::<Option<Vec<_>>>()?;
    (edits.len() > 1).then_some(edits)
}

/// The one edit a single queue commit names: its subject is `<prefix>: <verb> #<id>`,
/// alone or followed by a space and the rest of the message (`set #4 level 1`,
/// `add #5 — title`). `None` for any other subject, a batch's included.
pub fn single_edit<'a>(prefix: &str, subject: &'a str) -> Option<(&'a str, u64)> {
    let rest = subject.strip_prefix(prefix)?.strip_prefix(": ")?;
    let (verb, rest) = rest.split_once(" #")?;
    let digits = rest.split(' ').next()?;
    let ok =
        VERBS.contains(&verb) && !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit());
    ok.then_some((verb, digits.parse().ok()?))
}

/// What is wrong with a queue subject that names rows, against the rows its
/// commit changes: a batch names exactly those, each once; a single edit
/// changes no row but the one it names. That row may come out as it was: a
/// `set` to the value a row has rewrote its line in releases through 0.1.3.
pub fn subject_rows(
    prefix: &str,
    subject: &str,
    old: &HashMap<u64, &Task>,
    new: &HashMap<u64, &Task>,
) -> Option<String> {
    let (edits, batch) = match batch_edits(prefix, subject) {
        Some(e) => (e, true),
        None => (vec![single_edit(prefix, subject)?], false),
    };
    let named: BTreeSet<u64> = edits.iter().map(|e| e.1).collect();
    let mut changed = queue::changed_ids(old, new);
    if batch {
        // A batch names a row it moves to its lane's section, too.
        changed.extend(queue::moved_ids(old, new));
    }
    let fits = match batch {
        true => named == changed && named.len() == edits.len(),
        false => changed.is_subset(&named),
    };
    if fits {
        return None;
    }
    let list = |v: &BTreeSet<u64>| match v.is_empty() {
        true => "no row".to_string(),
        false => v
            .iter()
            .map(|i| format!("#{i}"))
            .collect::<Vec<_>>()
            .join(" "),
    };
    Some(format!(
        "its subject names {}, but it changes {} — a queue commit names each row it edits, once",
        edits
            .iter()
            .map(|e| format!("#{}", e.1))
            .collect::<Vec<_>>()
            .join(" "),
        list(&changed)
    ))
}

/// `subject_rows` for a commit given as the texts on either side of it.
pub fn subject_rows_texts(
    prefix: &str,
    subject: &str,
    old: [&str; 2],
    new: [&str; 2],
) -> Option<String> {
    let (old, new) = (queue::parse_all(old), queue::parse_all(new));
    subject_rows(prefix, subject, &queue::by_id(&old), &queue::by_id(&new))
}

/// A commit name: full (40 or 64 hex, what 5w records) or a short prefix of one,
/// as rows recorded before full ones hold.
fn is_sha(s: Option<&str>) -> bool {
    s.is_some_and(|s| s.len() >= 7 && s.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// `subject` is the commit's subject line, when there is a commit: a commit
/// `5w reject` made (alone, or in a batch that names it) is let gain `rework:` from any state, because released
/// versions through 0.1.3 rejected open and closed tasks, and lint passes every
/// commit the tool made. The staged edit has no subject, so a hand edit is
/// always judged.
fn check(
    cfg: &Config,
    repo: &Repo,
    old: &Snap,
    new: &Snap,
    subject: Option<&str>,
    at: &str,
    out: &mut Vec<String>,
) {
    let (oq, oa) = old.tasks();
    let old_all: HashMap<u64, &Task> = oq.iter().chain(&oa).map(|t| (t.id, t)).collect();
    let old_max = queue::max_id(&old.queue).max(queue::max_id(&old.archive));
    judge(cfg, repo, &old_all, old_max, new, subject, at, out);
}

/// `check` given the rows before the change by id, and the highest id then.
#[allow(clippy::too_many_arguments)]
fn judge(
    cfg: &Config,
    repo: &Repo,
    old_all: &HashMap<u64, &Task>,
    old_max: u64,
    new: &Snap,
    subject: Option<&str>,
    at: &str,
    out: &mut Vec<String>,
) {
    let (nq, na) = new.tasks();
    let mut say = |id: u64, m: String| out.push(format!("{at} #{id}: {m}"));

    // The file itself.
    for (id, a, b) in queue::duplicates(&nq) {
        say(
            id,
            format!("appears twice in {} (lines {a}, {b})", cfg.file),
        );
    }
    for (id, a, b) in queue::duplicates(&na) {
        say(
            id,
            format!("appears twice in {} (lines {a}, {b})", cfg.archive),
        );
    }
    for t in &na {
        if nq.iter().any(|q| q.id == t.id) {
            say(t.id, format!("in both {} and {}", cfg.file, cfg.archive));
        }
        if t.state != State::Done {
            say(t.id, format!("is open but sits in {}", cfg.archive));
        }
    }
    if queue::unclosed_fence(&new.queue) {
        out.push(format!("{at}: a ``` fence in {} is never closed", cfg.file));
    }

    let new_all: HashMap<u64, &Task> = nq.iter().chain(&na).map(|t| (t.id, t)).collect();
    let new_ids: HashSet<u64> = new_all.keys().copied().collect();

    if let Some(m) = subject.and_then(|s| subject_rows(&cfg.commit_prefix, s, old_all, &new_all)) {
        out.push(format!("{at}: {m}"));
    }

    let mut say = |id: u64, m: String| out.push(format!("{at} #{id}: {m}"));
    let mut ids: Vec<&u64> = old_all.keys().collect();
    ids.sort();
    for id in ids {
        if !new_all.contains_key(id) {
            say(
                *id,
                "deleted — rows are never removed (close it, or archive it)".into(),
            );
        }
    }

    let mut ids: Vec<&u64> = new_all.keys().collect();
    ids.sort();
    for &id in ids {
        let n = new_all[&id];
        // Judge only what this change touched: an untouched row was judged when
        // it last changed, and re-reporting it on every commit buries the news.
        if let Some(o) = old_all.get(&id)
            && o.state == n.state
            && (queue::identical(o, n) || (words(o) == words(n) && fields(o) == fields(n)))
        {
            continue;
        }
        judge_row(
            cfg,
            repo,
            old_all.get(&id).copied(),
            n,
            &new_ids,
            old_max,
            subject,
            at,
            out,
        );
    }
}

/// Judge one row's change from `old` (none: added) to `n`, by the rules a
/// commit follows.
#[allow(clippy::too_many_arguments)]
fn judge_row(
    cfg: &Config,
    repo: &Repo,
    old: Option<&Task>,
    n: &Task,
    new_ids: &HashSet<u64>,
    old_max: u64,
    subject: Option<&str>,
    at: &str,
    out: &mut Vec<String>,
) {
    let id = n.id;
    let lane_of = |t: &Task| t.lane.clone().unwrap_or_else(|| cfg.default_lane.clone());
    let mut say = |id: u64, m: String| out.push(format!("{at} #{id}: {m}"));
    let lane = lane_of(n);
    let close = cfg.lane(&lane).map(|l| l.close.clone());

    // Fields that are always checkable.
    // A sha written or changed here is the full name. A prefix stays valid in
    // a row that carried it before, and in a submit or accept commit the tool
    // made (its subject names that edit of this row) when it is the 12 digits
    // releases through 0.1.3 recorded: lint passes every commit the tool made.
    let tool_edit = |verb: &str| {
        subject.is_some_and(|s| {
            single_edit(&cfg.commit_prefix, s) == Some((verb, id))
                || batch_edits(&cfg.commit_prefix, s).is_some_and(|e| e.contains(&(verb, id)))
        })
    };
    for (field, verb, v, was) in [
        (
            "submitted",
            "submit",
            &n.submitted,
            old.and_then(|o| o.submitted.as_ref()),
        ),
        (
            "reviewed",
            "accept",
            &n.reviewed,
            old.and_then(|o| o.reviewed.as_ref()),
        ),
    ] {
        if let Some(v) = v
            && was != Some(v)
            && is_sha(Some(v))
            && !matches!(v.len(), 40 | 64)
            && !(v.len() == 12 && tool_edit(verb))
        {
            say(
                id,
                format!("{field}:{v} is not a full sha — record `git rev-parse <commit>`"),
            );
        }
    }
    if cfg.lane(&lane).is_none() {
        say(id, format!("lane >{lane} is not in the config"));
    }
    for d in &n.needs {
        if !new_ids.contains(d) {
            say(id, format!("needs #{d}, which does not exist"));
        }
    }
    if let Some(b) = &n.branch
        && !git::ok(&repo.primary, &["check-ref-format", "--branch", b])
    {
        say(id, format!("branch:{b} is not a valid branch name"));
    }

    let Some(o) = old else {
        // Added.
        if id <= old_max {
            say(
                id,
                format!("new row reuses id {id} (highest before was {old_max})"),
            );
        }
        if n.state != State::Open
            || n.via.is_some()
            || n.submitted.is_some()
            || n.reviewed.is_some()
        {
            say(
                id,
                "a new row is open, with no via:, submitted: or reviewed:".into(),
            );
        }
        if n.rework.is_some() {
            say(
                id,
                "a new row carries rework: — only a reject adds one".into(),
            );
        }
        return;
    };

    let same_content = words(o) == words(n) && fields(o) == fields(n);
    if o.rework.is_none()
        && n.rework.is_some()
        && n.state != State::Done
        && (o.state, n.state) != (State::Review, State::Open)
        && subject != Some(format!("{}: reject #{id}", cfg.commit_prefix).as_str())
        && !subject
            .and_then(|s| batch_edits(&cfg.commit_prefix, s))
            .is_some_and(|e| e.contains(&("reject", id)))
    {
        say(id, "gained rework: outside a reject ([~]→[ ])".into());
    }
    match (o.state, n.state) {
        (State::Done, State::Done) => {
            if !same_content {
                say(id, "a closed row changed — reopen it first".into());
            }
        }
        (State::Done, State::Open) => {
            if n.via.is_some() || n.reviewed.is_some() || n.submitted.is_some() {
                say(
                    id,
                    "reopened, but still carries via:, reviewed: or submitted:".into(),
                );
            }
        }
        (State::Done, State::Review) => say(
            id,
            "[x]→[~] is not a transition; reopen, then submit".into(),
        ),
        (State::Open, State::Open) | (State::Review, State::Open) => {
            if n.via.is_some() || n.reviewed.is_some() {
                say(id, "an open row carries via: or reviewed:".into());
            }
            if n.submitted.is_some() {
                say(id, "an open row carries submitted:".into());
            }
            if o.state == State::Review && n.rework.is_none() {
                say(id, "rejected without rework:\"why\"".into());
            }
        }
        (_, State::Review) => {
            if n.branch.is_none() {
                say(id, "submitted without branch:".into());
            }
            if !is_sha(n.submitted.as_deref()) {
                say(
                    id,
                    "submitted without submitted:<sha> (git rev-parse <branch>)".into(),
                );
            }
            if n.via.is_some() || n.reviewed.is_some() {
                say(id, "a submitted row carries via: or reviewed:".into());
            }
        }
        (_, State::Done) => {
            match n.via.as_deref() {
                Some("review") => {
                    if n.branch.is_some() && !is_sha(n.reviewed.as_deref()) {
                        say(id, "accepted without reviewed:<sha>".into());
                    }
                    if o.state == State::Open {
                        say(id, "accepted but never submitted".into());
                    }
                }
                Some(v) if Some(v) == close.as_deref() => {}
                Some(v) => say(
                    id,
                    format!(
                        "closed via:{v}, but >{lane} closes via:{}",
                        close.as_deref().unwrap_or("?")
                    ),
                ),
                None => say(id, "closed without via:".into()),
            }
            if n.rework.is_some() {
                say(id, "closed but still carries rework:".into());
            }
        }
    }
}

// --- the hook ----------------------------------------------------------------------------

use crate::upkeep::{HOOK_MARK, hook_path, hook_script};

/// A server installing its hook over a broken trunk config: say what it will take.
fn broken_note(repo: &Repo, trunk: &str) {
    if let Some(e) = &repo.broken {
        println!(
            "hook: {} on {trunk} is broken ({e}); the hook accepts only a push to {trunk} that repairs it",
            crate::store::CONFIG_FILE
        );
    }
}

/// The server's trunk, pinned when `5w.trunk` is unset or empty (on every install, so a
/// removed pin comes back): from `FIVEW_TRUNK`, which outranks it, else from HEAD — a
/// guess that finds no such branch judges no push as landing on it. Returns the trunk
/// the hook judges.
fn pin_trunk(repo: &Repo, kind: &str) -> Res<String> {
    if kind == "pre-receive"
        && repo.bare
        && git::opt(&repo.primary, &["config", "5w.trunk"]).is_none_or(|s| s.is_empty())
    {
        let env = std::env::var("FIVEW_TRUNK").ok().filter(|s| !s.is_empty());
        let from = if env.is_some() { "FIVEW_TRUNK" } else { "HEAD" };
        if let Some(b) = env.or_else(|| crate::store::head_branch(&repo.primary)) {
            git::git(&repo.primary, &["config", "5w.trunk", &b])?;
            println!("hook: 5w.trunk = {b} (the trunk pushes are judged against; from {from})");
            return Ok(b);
        }
    }
    Ok(repo.trunk.clone())
}

pub fn hook(repo: &Repo, args: &[String]) -> Res<()> {
    let kind = args.get(1).map(|s| s.as_str()).unwrap_or("pre-commit");
    if !matches!(kind, "pre-commit" | "pre-receive") {
        bail!("usage: 5w hook install | uninstall [pre-commit | pre-receive]");
    }
    // Opened leniently only for pre-receive: a server takes its repair by push,
    // a checkout fixes the file in place.
    if let Some(e) = &repo.broken
        && !(kind == "pre-receive" && repo.bare)
    {
        return Err(e.clone());
    }
    let path = hook_path(repo, kind)?;
    let ours = std::fs::read_to_string(&path).map(|s| s.contains(HOOK_MARK));
    match args.first().map(|s| s.as_str()) {
        Some("install") => {
            match ours {
                Ok(true)
                    if std::fs::read_to_string(&path).ok().as_deref()
                        == Some(hook_script(repo, kind).as_str()) =>
                {
                    let trunk = pin_trunk(repo, kind)?;
                    broken_note(repo, &trunk);
                    println!("hook: already installed at {}", path.display());
                    return Ok(());
                }
                Ok(true) => {} // ours, from another version: rewrite it
                Ok(false) => bail!(
                    "{} exists and is not ours; call 5w from it instead (see `5w hook` in the README)",
                    path.display()
                ),
                Err(_) => {}
            }
            if let Some(d) = path.parent() {
                std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
            }
            let script = hook_script(repo, kind);
            std::fs::write(&path, script).map_err(|e| e.to_string())?;
            let trunk = pin_trunk(repo, kind)?;
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                .map_err(|e| e.to_string())?;
            // Named by the trunk the hook judges: the pin just written wins over
            // what the broken config says.
            broken_note(repo, &trunk);
            println!("hook: installed {}", path.display());
            Ok(())
        }
        Some("uninstall") => match ours {
            Ok(true) => {
                std::fs::remove_file(&path).map_err(|e| e.to_string())?;
                println!("hook: removed {}", path.display());
                Ok(())
            }
            Ok(false) => bail!("{} is not ours; leaving it", path.display()),
            Err(_) => {
                println!("hook: none installed");
                Ok(())
            }
        },
        _ => bail!("usage: 5w hook install | uninstall [pre-commit | pre-receive]"),
    }
}

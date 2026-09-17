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
    let mut problems = Vec::new();
    match arg {
        "--staged" => {
            let index = git::caller_index();
            let env: Vec<(&str, &str)> = index
                .iter()
                .map(|i| ("GIT_INDEX_FILE", i.as_str()))
                .collect();
            let o = git::raw(&repo.cwd, &["diff", "--cached", "--name-only"], &env, None)?;
            if !o.ok {
                return Err(format!("git diff --cached: {}", o.stderr.trim()));
            }
            let files = o.stdout;
            let files: Vec<&str> = files.lines().collect();
            if !touches_queue(&repo.cfg, &files) {
                return Ok(());
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
                |name| {
                    let pathspec = format!(":(top){name}");
                    let l = git::opt(&repo.cwd, &["ls-files", "-s", "--", &pathspec])?;
                    let mut f = l.split_whitespace();
                    Some((f.next()?.to_string(), f.next()?.to_string()))
                },
                "staged",
                &mut problems,
            );
            let old = at_rev(repo, head.as_deref());
            let new = Snap {
                queue: show_index(repo, &env, &repo.cfg.file),
                archive: show_index(repo, &env, &repo.cfg.archive),
            };
            check(&repo.cfg, repo, &old, &new, None, "staged", &mut problems);
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
            let trunk = repo.trunk.clone();
            commits_on(
                repo,
                &list,
                &|c| git::ok(&repo.cwd, &["merge-base", "--is-ancestor", c, &trunk]),
                &mut problems,
            )?;
        }
    }
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
        let old = at_rev(repo, parent.as_deref());
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
    }
    Ok(())
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
    const LINK: &str = "120000";
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

    let old_all: HashMap<u64, &Task> = oq.iter().chain(&oa).map(|t| (t.id, t)).collect();
    let new_all: HashMap<u64, &Task> = nq.iter().chain(&na).map(|t| (t.id, t)).collect();
    let new_ids: HashSet<u64> = new_all.keys().copied().collect();
    let old_max = queue::max_id(&old.queue).max(queue::max_id(&old.archive));
    let lane_of = |t: &Task| t.lane.clone().unwrap_or_else(|| cfg.default_lane.clone());

    if let Some(m) = subject.and_then(|s| subject_rows(&cfg.commit_prefix, s, &old_all, &new_all)) {
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
                old_all.get(&id).and_then(|o| o.submitted.as_ref()),
            ),
            (
                "reviewed",
                "accept",
                &n.reviewed,
                old_all.get(&id).and_then(|o| o.reviewed.as_ref()),
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

        let Some(o) = old_all.get(&id) else {
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
            continue;
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
}

// --- the hook ----------------------------------------------------------------------------

use crate::upkeep::{HOOK_MARK, hook_path, hook_script};

pub fn hook(repo: &Repo, args: &[String]) -> Res<()> {
    let kind = args.get(1).map(|s| s.as_str()).unwrap_or("pre-commit");
    if !matches!(kind, "pre-commit" | "pre-receive") {
        bail!("usage: 5w hook install | uninstall [pre-commit | pre-receive]");
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
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                .map_err(|e| e.to_string())?;
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

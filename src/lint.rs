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
use std::collections::{HashMap, HashSet};

pub const USAGE: &str = "\
usage: 5w lint [--staged | <rev> | <from>..<to>]

  --staged       the index against HEAD (what the pre-commit hook runs; the default)
  <rev>          that one commit against its parent
  <from>..<to>   every commit in the range, each against its parent

Checks rows against PROTOCOL.md: legal state changes and what each must carry,
closed rows unchanged, no deletion or reused id, and that a queue edit is its
own commit on the trunk.

  5w hook install | uninstall    the pre-commit hook that runs `5w lint --staged`";

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
    let arg = args.first().map(|s| s.as_str()).unwrap_or("--staged");
    let mut problems = Vec::new();
    match arg {
        "-h" | "--help" | "help" => {
            println!("{USAGE}");
            return Ok(());
        }
        "--staged" => {
            let files = git::git(&repo.cwd, &["diff", "--cached", "--name-only"])?;
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
            let old = at_rev(repo, head.as_deref());
            let new = Snap {
                queue: show_index(repo, &repo.cfg.file),
                archive: show_index(repo, &repo.cfg.archive),
            };
            check(&repo.cfg, repo, &old, &new, "staged", &mut problems);
        }
        range => {
            let commits = if range.contains("..") {
                git::git(&repo.cwd, &["rev-list", "--reverse", range])?
            } else {
                git::git(
                    &repo.cwd,
                    &["rev-parse", "--verify", &format!("{range}^{{commit}}")],
                )?
            };
            for c in commits.lines() {
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
                let on_trunk = git::ok(&repo.cwd, &["merge-base", "--is-ancestor", c, &repo.trunk]);
                shape(
                    repo,
                    &files,
                    on_trunk.then_some(repo.trunk.as_str()),
                    short,
                    &mut problems,
                );
                let parent = git::rev(&repo.cwd, &format!("{c}^"));
                let old = at_rev(repo, parent.as_deref());
                let new = at_rev(repo, Some(c));
                check(&repo.cfg, repo, &old, &new, short, &mut problems);
            }
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

fn show_index(repo: &Repo, name: &str) -> String {
    git::raw(&repo.cwd, &["show", &format!(":{name}")], &[], None)
        .ok()
        .filter(|o| o.ok)
        .map(|o| o.stdout)
        .unwrap_or_default()
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
            branch.unwrap_or("a detached HEAD")
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

fn is_sha(s: Option<&str>) -> bool {
    s.is_some_and(|s| s.len() >= 7 && s.bytes().all(|b| b.is_ascii_hexdigit()))
}

fn check(cfg: &Config, repo: &Repo, old: &Snap, new: &Snap, at: &str, out: &mut Vec<String>) {
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
            && words(o) == words(n)
            && fields(o) == fields(n)
        {
            continue;
        }
        let lane = lane_of(n);
        let close = cfg.lane(&lane).map(|l| l.close.clone());

        // Fields that are always checkable.
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
            continue;
        };

        let same_content = words(o) == words(n) && fields(o) == fields(n);
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
                        "submitted without submitted:<sha> (git rev-parse --short=12 <branch>)"
                            .into(),
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

const HOOK_MARK: &str = "# installed by 5w";

fn hook_path(repo: &Repo) -> Res<std::path::PathBuf> {
    let dir = git::opt(
        &repo.primary,
        &["rev-parse", "--path-format=absolute", "--git-path", "hooks"],
    )
    .ok_or("cannot find the hooks directory")?;
    Ok(std::path::PathBuf::from(dir).join("pre-commit"))
}

pub fn hook(repo: &Repo, args: &[String]) -> Res<()> {
    let path = hook_path(repo)?;
    let ours = std::fs::read_to_string(&path).map(|s| s.contains(HOOK_MARK));
    match args.first().map(|s| s.as_str()) {
        Some("install") => {
            match ours {
                Ok(true) => {
                    println!("hook: already installed at {}", path.display());
                    return Ok(());
                }
                Ok(false) => bail!(
                    "{} exists and is not ours; add this line to it instead:\n  command -v 5w >/dev/null && 5w lint --staged",
                    path.display()
                ),
                Err(_) => {}
            }
            if let Some(d) = path.parent() {
                std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
            }
            let script = format!(
                "#!/bin/sh\n{HOOK_MARK} — checks queue edits against PROTOCOL.md\n\
                 if command -v 5w >/dev/null 2>&1; then\n  exec 5w lint --staged\nfi\n\
                 if git diff --cached --name-only | grep -qx -e '{}' -e '{}'; then\n  \
                 echo '5w is not installed: this queue edit is unchecked. Follow PROTOCOL.md.' >&2\nfi\nexit 0\n",
                repo.cfg.file, repo.cfg.archive
            );
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
        _ => bail!("usage: 5w hook install | uninstall"),
    }
}

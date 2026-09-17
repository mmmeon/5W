//! One worktree per branch, with the gitignored artifacts a working tree needs
//! symlinked in from the primary. Stacking uses git-town's own config keys, so
//! git-town works on these branches when it is installed and nothing needs it
//! when it is not.

use crate::bail;
use crate::git;
use crate::store::Repo;
use crate::util::Res;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub const USAGE: &str = "\
usage: 5w wt <command> [args]

  new <branch> [--from <parent>] [--install]
        Create <branch> as a child of <parent> (default: the branch checked out
        where you stand, or the trunk) in its own worktree.
  add <branch> [--install]   Worktree for a branch that already exists
  ls                          Worktrees with their parent branch
  path <branch>               Print the worktree path: cd \"$(5w wt path <branch>)\"
  rm <branch> [--force]       Remove the worktree; the branch is kept
  prune [--yes]               List branches with no commits past their parent, no task naming
                              them and a clean worktree (or none); --yes removes both
  link [<path>]               Re-link gitignored artifacts (default: cwd)
  install [<path>]            Run the configured install command
  setup                       Configure git (and git-town, if present). Idempotent
  discard-copy <branch>       Discard the trunk checkout's uncommitted changes when they
                              are exactly <branch>'s diff (as git would stage them); else refuse

Links are shared, not copied: a write through one hits the primary's file.";

pub fn run(repo: &Repo, args: &[String]) -> Res<()> {
    let Some(cmd) = args.first() else {
        println!("{USAGE}");
        return Ok(());
    };
    let rest = &args[1..];
    // `wt <command> --help` is the help its refusals point to.
    if rest.iter().any(|a| a == "-h" || a == "--help") {
        println!("{USAGE}");
        return Ok(());
    }
    // The commands that take no flag at all; the others check their own.
    if matches!(
        cmd.as_str(),
        "ls" | "list" | "path" | "link" | "install" | "setup" | "discard-copy"
    ) && let Some(f) = rest.iter().find(|a| a.starts_with("--"))
    {
        return Err(unknown_flag(repo, cmd, f));
    }
    match cmd.as_str() {
        "new" => new(repo, rest),
        "add" => add(repo, rest),
        "ls" | "list" => ls(repo),
        "path" => {
            let b = rest.first().ok_or("usage: 5w wt path <branch>")?;
            match git::worktree_of(&repo.primary, b)? {
                Some(p) => {
                    println!("{}", p.display());
                    Ok(())
                }
                None => bail!("no worktree for {b}"),
            }
        }
        "rm" | "remove" => {
            let usage = "usage: 5w wt rm <branch> [--force]";
            let (mut branch, mut force) = (None, false);
            for a in rest {
                match a.as_str() {
                    "--force" => force = true,
                    f if f.starts_with("--") => return Err(unknown_flag(repo, cmd, f)),
                    b if branch.is_none() => branch = Some(b),
                    _ => bail!("{usage}"),
                }
            }
            remove(repo, branch.ok_or(usage)?, force)
        }
        "prune" => {
            let yes = match rest {
                [] => false,
                [y] if y == "--yes" => true,
                _ => match rest.iter().find(|a| a.starts_with("--") && *a != "--yes") {
                    Some(f) => return Err(unknown_flag(repo, cmd, f)),
                    None => bail!("usage: 5w wt prune [--yes]"),
                },
            };
            // The branches the queue names, by the queue names the trunk commits.
            let (judged, note) = crate::lint::under_committed_rules(repo);
            crate::lint::noted(note, prune(judged.as_ref().unwrap_or(repo), yes))
        }
        "link" => link(repo, &target(repo, rest)?).map(|_| ()),
        "install" => install(repo, &target(repo, rest)?),
        "setup" => setup(repo),
        "discard-copy" => {
            let usage = "usage: 5w wt discard-copy <branch>";
            match rest {
                [b] => discard_copy(repo, b),
                _ => bail!("{usage}"),
            }
        }
        "help" | "-h" | "--help" => {
            println!("{USAGE}");
            Ok(())
        }
        _ => bail!("unknown wt command: {cmd} (try: 5w wt help)"),
    }
}

/// A refusal for a `--flag` the wt command does not take, worded as the queue
/// commands word theirs.
fn unknown_flag(repo: &Repo, cmd: &str, flag: &str) -> String {
    format!(
        "unknown flag {flag} for wt {cmd} ({} wt {cmd} --help)",
        repo.cfg.cmd_tasks
    )
}

fn target(repo: &Repo, rest: &[String]) -> Res<PathBuf> {
    Ok(rest
        .first()
        .map(PathBuf::from)
        .unwrap_or_else(|| repo.cwd.clone()))
}

fn slug(b: &str) -> String {
    b.replace('/', "-")
}

fn flags(
    repo: &Repo,
    cmd: &str,
    rest: &[String],
    allowed: &[&str],
) -> Res<(Option<String>, Vec<String>, Option<String>)> {
    let mut branch = None;
    let mut seen = Vec::new();
    let mut from = None;
    let mut i = 0;
    while i < rest.len() {
        let a = &rest[i];
        if a == "--from" && allowed.contains(&"--from") {
            from = Some(rest.get(i + 1).ok_or("--from needs a branch")?.clone());
            i += 1;
        } else if a.starts_with("--") {
            if !allowed.contains(&a.as_str()) {
                return Err(unknown_flag(repo, cmd, a));
            }
            seen.push(a.clone());
        } else if branch.is_none() {
            branch = Some(a.clone());
        } else {
            bail!("unexpected {a:?}");
        }
        i += 1;
    }
    Ok((branch, seen, from))
}

fn new(repo: &Repo, rest: &[String]) -> Res<()> {
    let (branch, seen, from) = flags(repo, "new", rest, &["--from", "--install"])?;
    let branch = branch.ok_or("usage: 5w wt new <branch> [--from <parent>] [--install]")?;
    git::git(&repo.primary, &["check-ref-format", "--branch", &branch])
        .map_err(|_| format!("not a valid branch name: {branch}"))?;
    if git::branch_exists(&repo.primary, &branch) {
        bail!("branch {branch} already exists (use: 5w wt add {branch})");
    }
    let parent = from
        .or_else(|| git::current_branch(&repo.cwd))
        .unwrap_or_else(|| repo.trunk.clone());
    if !git::branch_exists(&repo.primary, &parent) {
        bail!("parent branch {parent} does not exist");
    }
    let dir = repo.wt_root().join(slug(&branch));
    if dir.exists() {
        bail!("{} already exists", dir.display());
    }
    // Branch off the parent's tip and record the lineage git-town reads. Not
    // `git town append`: that moves the current worktree onto the child.
    git::git(&repo.primary, &["branch", &branch, &parent])?;
    git::git(
        &repo.primary,
        &[
            "config",
            &format!("git-town-branch.{branch}.parent"),
            &parent,
        ],
    )?;
    git::git(
        &repo.primary,
        &[
            "config",
            &format!("git-town-branch.{branch}.branchtype"),
            "feature",
        ],
    )?;
    fs::create_dir_all(repo.wt_root()).map_err(|e| e.to_string())?;
    git::git(
        &repo.primary,
        &["worktree", "add", &dir.to_string_lossy(), &branch],
    )?;
    link(repo, &dir)?;
    if seen.iter().any(|s| s == "--install") {
        install(repo, &dir)?;
    }
    println!(
        "\nwt: {branch} (child of {parent})\n    cd {}",
        dir.display()
    );
    Ok(())
}

fn add(repo: &Repo, rest: &[String]) -> Res<()> {
    let (branch, seen, _) = flags(repo, "add", rest, &["--install"])?;
    let branch = branch.ok_or("usage: 5w wt add <branch> [--install]")?;
    add_worktree(repo, &branch, seen.iter().any(|s| s == "--install"))
}

pub fn add_worktree(repo: &Repo, branch: &str, do_install: bool) -> Res<()> {
    if !git::branch_exists(&repo.primary, branch) {
        bail!("no such branch: {branch}");
    }
    if let Some(p) = git::worktree_of(&repo.primary, branch)? {
        bail!("{branch} already has a worktree at {}", p.display());
    }
    let dir = repo.wt_root().join(slug(branch));
    if dir.exists() {
        bail!("{} already exists", dir.display());
    }
    fs::create_dir_all(repo.wt_root()).map_err(|e| e.to_string())?;
    git::git(
        &repo.primary,
        &["worktree", "add", &dir.to_string_lossy(), branch],
    )?;
    link(repo, &dir)?;
    if do_install {
        install(repo, &dir)?;
    }
    println!("\nwt: cd {}", dir.display());
    Ok(())
}

fn tilde(path: &Path) -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    let p = path.display().to_string();
    match p.strip_prefix(&home) {
        Some(rest) if !home.is_empty() && (rest.is_empty() || rest.starts_with('/')) => {
            format!("~{rest}")
        }
        _ => p,
    }
}

fn ls(repo: &Repo) -> Res<()> {
    for w in git::worktrees(&repo.primary)? {
        let p = tilde(&w.path);
        match &w.branch {
            Some(b) => {
                let parent = git::parent_of(&repo.primary, b).unwrap_or("-".into());
                println!("{b:<32} {p:<40} parent: {parent}");
            }
            None => println!("{:<32} {p}", "(detached)"),
        }
    }
    for n in stack_notes(repo) {
        println!("note: {n}");
    }
    Ok(())
}

/// Branches stacked on another unshipped branch while recording the trunk as
/// their parent: their commits include that branch's tip. Ship would then take
/// the child for bottom-of-stack, and compare its review against the trunk with
/// the parent's change in it. Nearest such branch named; errors mean no note.
///
/// Both sides must be stack branches (a recorded parent), so a backup made with
/// `git branch` is never named, nor flagged; a branch recorded anywhere below the
/// flagged one is its child, not its parent. Three git processes whatever the
/// number of branches: refs, config, and one walk of every unshipped commit.
pub fn stack_notes(repo: &Repo) -> Vec<String> {
    use std::collections::{HashMap, HashSet};
    let p = &repo.primary;
    let trunk = &repo.trunk;
    let Some(refs) = git::opt(
        p,
        &[
            "for-each-ref",
            "--format=%(objectname) %(refname)",
            "refs/heads",
        ],
    ) else {
        return Vec::new();
    };
    let tips: HashMap<&str, &str> = refs
        .lines()
        .filter_map(|l| l.split_once(' '))
        .filter_map(|(sha, r)| Some((r.strip_prefix("refs/heads/")?, sha)))
        .collect();
    if !tips.contains_key(trunk.as_str()) {
        return Vec::new();
    }
    let config = git::opt(
        p,
        &[
            "config",
            "--get-regexp",
            r"^git-town(-branch\..*\.(parent|branchtype)|\.perennial-branches)$",
        ],
    )
    .unwrap_or_default();
    let mut parent: HashMap<&str, &str> = HashMap::new();
    let mut perennial: HashSet<&str> = repo.cfg.perennial.iter().map(|s| s.as_str()).collect();
    for (key, val) in config.lines().filter_map(|l| l.split_once(' ')) {
        if key == "git-town.perennial-branches" {
            perennial.extend(val.split_whitespace());
        } else if let Some(b) = key.strip_prefix("git-town-branch.") {
            if let Some(b) = b.strip_suffix(".parent") {
                parent.insert(b, val);
            } else if let Some(b) = b.strip_suffix(".branchtype")
                && val == "perennial"
            {
                perennial.insert(b);
            }
        }
    }
    // Every commit on a branch and not on the trunk, with its parents.
    let walk = git::opt(
        p,
        &[
            "rev-list",
            "--parents",
            "--branches",
            "--not",
            &format!("refs/heads/{trunk}"),
        ],
    )
    .unwrap_or_default();
    let graph: HashMap<&str, Vec<&str>> = walk
        .lines()
        .filter_map(|l| {
            let mut it = l.split(' ');
            Some((it.next()?, it.collect()))
        })
        .collect();
    let reach = |tip: &str| -> HashSet<&str> {
        let mut seen = HashSet::new();
        let mut todo = vec![tip];
        while let Some(c) = todo.pop() {
            if let Some((c, ps)) = graph.get_key_value(c)
                && seen.insert(*c)
            {
                todo.extend(ps);
            }
        }
        seen
    };
    // Is `b` recorded anywhere up `o`'s chain of parents?
    let below = |o: &str, b: &str| {
        let mut cur = o;
        for _ in 0..=parent.len() {
            match parent.get(cur) {
                Some(&up) if up == b => return true,
                Some(&up) => cur = up,
                None => break,
            }
        }
        false
    };
    let stacked = |b: &str| b != trunk && !perennial.contains(b) && parent.contains_key(b);
    let mut names: Vec<&str> = tips.keys().copied().collect();
    names.sort();
    let mut at: HashMap<&str, Vec<&str>> = HashMap::new();
    for &b in &names {
        at.entry(tips[b]).or_default().push(b);
    }
    let mut notes = Vec::new();
    for &b in &names {
        let tip = tips[b];
        if !stacked(b) || parent[b] != trunk || !graph.contains_key(tip) {
            continue;
        }
        let nearest = reach(tip)
            .into_iter()
            .filter(|c| *c != tip)
            .flat_map(|c| at.get(c).into_iter().flatten().copied())
            .filter(|&o| o != b && stacked(o) && !below(o, b))
            .max_by_key(|&o| (reach(tips[o]).len(), std::cmp::Reverse(o)));
        if let Some(o) = nearest {
            notes.push(format!(
                "{b} holds {o}'s unshipped commits but records {trunk} as its parent — `git config git-town-branch.{b}.parent {o}`"
            ));
        }
    }
    notes
}

/// Branches safe to drop: no commits past the recorded parent (else the trunk), no
/// task in the queue or archive naming them, not the trunk or perennial, not checked
/// out in the primary, and a worktree — if any — clean, unlocked and without
/// ignored files ship would refuse to delete. Every candidate is checked before
/// anything is removed; a refusal on one removes none.
fn prune(repo: &Repo, yes: bool) -> Res<()> {
    use std::collections::{BTreeMap, BTreeSet};
    let p = &repo.primary;
    let mut named = BTreeSet::new();
    let mut open = BTreeSet::new();
    for text in [
        repo.load()?,
        repo.committed()?.unwrap_or_default(),
        repo.load_archive()?,
        repo.committed_file(&repo.cfg.archive)?.unwrap_or_default(),
    ] {
        for t in crate::queue::parse(&text) {
            // A worker's branch exists from `wt new`, but the task names it only at
            // submit: keep `<any area>/task-<id>` for every task not closed, since
            // the area may have changed since the branch was made.
            if t.state != crate::queue::State::Done {
                open.insert(format!("task-{}", t.id));
            }
            named.extend(t.branch);
        }
    }
    let busy = busy_branches(repo)?;
    let root = fs::canonicalize(repo.wt_root()).unwrap_or(repo.wt_root());
    let wts = git::worktrees(p)?;
    let primary = fs::canonicalize(p).unwrap_or(p.clone());
    let cwd = fs::canonicalize(&repo.cwd).unwrap_or(repo.cwd.clone());
    let branches: Vec<String> =
        git::git(p, &["for-each-ref", "--format=%(refname)", "refs/heads"])?
            .lines()
            .filter_map(|r| r.strip_prefix("refs/heads/"))
            .map(String::from)
            .collect();
    let parents: BTreeMap<&str, String> = branches
        .iter()
        .map(|b| {
            (
                b.as_str(),
                git::parent_of(p, b).unwrap_or(repo.trunk.clone()),
            )
        })
        .collect();

    // (branch, tip, worktree) that may go; (branch, reason) that could but may not.
    let mut go: Vec<(String, String, Option<PathBuf>)> = Vec::new();
    let mut kept: Vec<(String, String)> = Vec::new();
    for b in &branches {
        let parent = &parents[b.as_str()];
        if *b == repo.trunk
            || repo.is_perennial(b)
            || named.contains(b)
            || open.contains(b.rsplit('/').next().unwrap_or(b))
            || parent == b
            || !git::branch_exists(p, parent)
        {
            continue;
        }
        let Some(tip) = git::rev(p, &format!("refs/heads/{b}")) else {
            continue;
        };
        let range = format!("refs/heads/{parent}..refs/heads/{b}");
        if git::opt(p, &["rev-list", "--count", &range]).as_deref() != Some("0") {
            continue;
        }
        if busy.contains(b) {
            kept.push((b.clone(), "a rebase or bisect is in progress on it".into()));
            continue;
        }
        let Some(w) = wts.iter().find(|w| w.branch.as_deref() == Some(b)) else {
            go.push((b.clone(), tip, None));
            continue;
        };
        let dir = fs::canonicalize(&w.path).unwrap_or(w.path.clone());
        if dir == primary {
            continue;
        }
        let why = if !dir.starts_with(&root) {
            "worktree outside worktrees.root".to_string()
        } else if w.held {
            "worktree is locked or missing — `git worktree list`".to_string()
        } else if cwd.starts_with(&dir) {
            "you are in its worktree — run from elsewhere".to_string()
        } else {
            worktree_blocker(repo, &w.path).unwrap_or_default()
        };
        if why.is_empty() {
            go.push((b.clone(), tip, Some(w.path.clone())));
        } else {
            kept.push((b.clone(), why));
        }
    }
    // A branch stays while a branch that stays records it as parent.
    loop {
        let going: BTreeSet<&str> = go.iter().map(|g| g.0.as_str()).collect();
        let child = |b: &str| {
            parents
                .iter()
                .find(|(c, par)| par.as_str() == b && !going.contains(*c))
                .map(|(c, _)| c.to_string())
        };
        let Some(i) = go.iter().position(|g| child(&g.0).is_some()) else {
            break;
        };
        let c = child(&go[i].0).unwrap_or_default();
        let (b, _, _) = go.remove(i);
        kept.push((b, format!("parent of {c}")));
    }
    kept.sort();
    // Children before their parents, so a stopped run never leaves a branch whose
    // recorded parent is gone.
    let mut ordered = Vec::with_capacity(go.len());
    while !go.is_empty() {
        let i = (0..go.len())
            .find(|&i| !go.iter().any(|g| parents[g.0.as_str()] == go[i].0))
            .unwrap_or(0);
        ordered.push(go.remove(i));
    }
    let go = ordered;

    let at = |w: &Option<PathBuf>| w.as_deref().map(tilde).unwrap_or("(no worktree)".into());
    if !yes {
        for (b, _, w) in &go {
            println!("remove {b} {}", at(w));
        }
    }
    for (b, why) in &kept {
        println!("kept {b} — {why}");
    }
    if go.is_empty() {
        println!("wt prune: nothing to remove");
        return Ok(());
    }
    if !yes {
        println!(
            "wt prune: {} to remove — `5w wt prune --yes` removes them",
            go.len()
        );
        return Ok(());
    }
    for (b, tip, w) in &go {
        // Checked again at the moment of removal: anything that changed since the
        // scan stops the run, and children went first, so no parent is left dangling.
        let moved = git::rev(p, &format!("refs/heads/{b}")).as_deref() != Some(tip.as_str());
        let why = if moved {
            Some("its branch moved".to_string())
        } else if busy_branches(repo)?.contains(b) {
            Some("a rebase or bisect started on it".to_string())
        } else {
            w.as_deref().and_then(|d| worktree_blocker(repo, d))
        };
        if let Some(why) = why {
            bail!("{b} changed since it was checked ({why}); stopped, nothing more removed");
        }
        if let Some(dir) = w {
            unlink(repo, dir)?;
            git::git(p, &["worktree", "remove", &dir.to_string_lossy()])?;
        }
        // Deleted only if the branch still points where it was checked.
        git::git(p, &["update-ref", "-d", &format!("refs/heads/{b}"), tip])?;
        let _ = git::raw(
            p,
            &[
                "config",
                "--remove-section",
                &format!("git-town-branch.{b}"),
            ],
            &[],
            None,
        );
        println!("removed {b} {}", at(w));
    }
    println!("wt prune: removed {} branch(es)", go.len());
    Ok(())
}

/// Why a worktree may not be removed: changes git lists, or ignored files ship would
/// refuse to delete.
fn worktree_blocker(repo: &Repo, dir: &Path) -> Option<String> {
    if git::dirty(dir).unwrap_or(true) {
        return Some(format!("uncommitted or untracked files in {}", tilde(dir)));
    }
    match crate::ship::ignored_files(repo, dir) {
        Err(e) => Some(e),
        Ok(f) if f.is_empty() => None,
        Ok(f) => Some(format!(
            "ignored files: {}{} — move them or add to worktrees.disposable",
            f.iter().take(5).cloned().collect::<Vec<_>>().join(", "),
            if f.len() > 5 {
                format!(" (+{} more)", f.len() - 5)
            } else {
                String::new()
            }
        )),
    }
}

/// Branches a rebase or bisect in any worktree is working on. Their worktree shows
/// detached meanwhile, so the branch would otherwise look free.
fn busy_branches(repo: &Repo) -> Res<std::collections::BTreeSet<String>> {
    let common = PathBuf::from(git::git(
        &repo.primary,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?);
    let mut dirs = vec![common.clone()];
    if let Ok(rd) = fs::read_dir(common.join("worktrees")) {
        dirs.extend(rd.flatten().map(|e| e.path()));
    }
    let mut v = std::collections::BTreeSet::new();
    for d in dirs {
        for f in [
            "rebase-merge/head-name",
            "rebase-apply/head-name",
            "BISECT_START",
        ] {
            if let Ok(s) = fs::read_to_string(d.join(f)) {
                let s = s.trim();
                v.insert(s.strip_prefix("refs/heads/").unwrap_or(s).to_string());
            }
        }
    }
    Ok(v)
}

// --- links ------------------------------------------------------------------------------

fn glob_match(pat: &[u8], s: &[u8]) -> bool {
    match (pat.first(), s.first()) {
        (None, None) => true,
        (Some(b'*'), _) => glob_match(&pat[1..], s) || (!s.is_empty() && glob_match(pat, &s[1..])),
        (Some(b'?'), Some(_)) => glob_match(&pat[1..], &s[1..]),
        (Some(p), Some(c)) if p == c => glob_match(&pat[1..], &s[1..]),
        _ => false,
    }
}

/// Expand a relative path pattern against `root`, component by component.
fn expand(root: &Path, pattern: &str) -> Vec<PathBuf> {
    let mut paths = vec![PathBuf::new()];
    for comp in pattern.split('/').filter(|c| !c.is_empty()) {
        let mut next = Vec::new();
        for rel in &paths {
            if comp.contains(['*', '?']) {
                let Ok(rd) = fs::read_dir(root.join(rel)) else {
                    continue;
                };
                let mut names: Vec<String> = rd
                    .filter_map(|e| e.ok())
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect();
                names.sort();
                for n in names {
                    if (!n.starts_with('.') || comp.starts_with('.'))
                        && glob_match(comp.as_bytes(), n.as_bytes())
                    {
                        next.push(rel.join(n));
                    }
                }
            } else {
                let p = rel.join(comp);
                if root.join(&p).symlink_metadata().is_ok() {
                    next.push(p);
                }
            }
        }
        paths = next;
    }
    paths.retain(|p| !p.as_os_str().is_empty());
    paths
}

fn patterns(repo: &Repo) -> Vec<String> {
    let Ok(s) = fs::read_to_string(repo.primary.join(&repo.cfg.links_file)) else {
        return vec![];
    };
    s.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(String::from)
        .collect()
}

pub fn link(repo: &Repo, dest: &Path) -> Res<usize> {
    let dest = fs::canonicalize(dest).map_err(|e| format!("{}: {e}", dest.display()))?;
    if dest == fs::canonicalize(&repo.primary).unwrap_or(repo.primary.clone()) {
        bail!("refusing to link into the primary worktree");
    }
    let (mut n, mut present) = (0, 0);
    for pat in patterns(repo) {
        let found = expand(&repo.primary, &pat);
        if found.is_empty() {
            eprintln!("wt: no match for {pat}");
        }
        for rel in found {
            let src = repo.primary.join(&rel);
            let target = dest.join(&rel);
            // A symlink already there, or a real file the glob happened to match
            // (tracked content): leave either alone.
            if target.symlink_metadata().is_ok() {
                present += 1;
                continue;
            }
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            std::os::unix::fs::symlink(&src, &target)
                .map_err(|e| format!("link {}: {e}", target.display()))?;
            n += 1;
        }
    }
    if n + present > 0 {
        println!(
            "wt: linked {n} artifact(s){}",
            if present > 0 {
                format!(", {present} already present")
            } else {
                String::new()
            }
        );
    }
    Ok(n)
}

/// Remove only the links that point into the primary — never anything real.
fn unlink(repo: &Repo, dest: &Path) -> Res<usize> {
    let mut n = 0;
    for pat in patterns(repo) {
        for rel in expand(&repo.primary, &pat) {
            let target = dest.join(&rel);
            if fs::read_link(&target).ok().as_deref() == Some(repo.primary.join(&rel).as_path()) {
                fs::remove_file(&target).map_err(|e| e.to_string())?;
                n += 1;
            }
        }
    }
    Ok(n)
}

pub fn install(repo: &Repo, dest: &Path) -> Res<()> {
    let Some(cmd) = &repo.cfg.install else {
        println!("wt: no worktrees.install configured, nothing to install");
        return Ok(());
    };
    let mut dirs = Vec::new();
    match &repo.cfg.install_marker {
        None => dirs.push(dest.to_path_buf()),
        Some(marker) => {
            let mut stack = vec![(dest.to_path_buf(), 0)];
            while let Some((d, depth)) = stack.pop() {
                if d.join(marker).is_file() {
                    dirs.push(d.clone());
                }
                if depth >= 2 {
                    continue;
                }
                let Ok(rd) = fs::read_dir(&d) else { continue };
                for e in rd.flatten() {
                    let name = e.file_name();
                    let name = name.to_string_lossy();
                    if name == "node_modules" || name.starts_with('.') {
                        continue;
                    }
                    if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                        stack.push((e.path(), depth + 1));
                    }
                }
            }
            dirs.sort();
        }
    }
    if dirs.is_empty() {
        println!("wt: nothing to install");
    }
    for d in dirs {
        println!("wt: {cmd}  (in {})", d.display());
        let st = Command::new("sh")
            .arg("-c")
            .arg(cmd)
            .current_dir(&d)
            .status()
            .map_err(|e| e.to_string())?;
        if !st.success() {
            bail!("install failed in {}", d.display());
        }
    }
    Ok(())
}

pub fn remove(repo: &Repo, branch: &str, force: bool) -> Res<()> {
    if repo.is_perennial(branch) {
        bail!("{branch} is perennial — its worktree is not ours to remove");
    }
    let Some(dir) = git::worktree_of(&repo.primary, branch)? else {
        bail!("no worktree for {branch}")
    };
    if dir == repo.primary {
        bail!("{branch} is checked out in the primary worktree");
    }
    // Ask before stripping links: a refused removal must leave a usable worktree.
    if !force && git::dirty(&dir)? {
        bail!(
            "{} has uncommitted or untracked files; commit or clean them, or `5w wt rm {branch} --force` to discard them",
            dir.display()
        );
    }
    let n = unlink(repo, &dir)?;
    if n > 0 {
        println!("wt: unlinked {n} artifact(s)");
    }
    let d = dir.to_string_lossy();
    let mut args = vec!["worktree", "remove"];
    if force {
        args.push("--force");
    }
    args.push(&d);
    git::git(&repo.primary, &args)?;
    println!("wt: removed {} (branch {branch} kept)", dir.display());
    Ok(())
}

// --- copies of a branch in the trunk checkout -------------------------------------------

/// One side of a file's change: `(mode, blob)`, or `None` where the file is absent.
type Side = Option<(String, String)>;
type Changes = std::collections::BTreeMap<String, (Side, Side)>;

fn side(mode: &str, sha: &str) -> Side {
    (mode != "000000").then(|| (mode.to_string(), sha.to_string()))
}

/// Parse `git diff --raw -z`: `:old new oldsha newsha status\0path\0` per file.
fn raw_changes(out: &str) -> Changes {
    let mut v = Changes::new();
    let mut it = out.split('\0');
    while let (Some(meta), Some(path)) = (it.next(), it.next()) {
        let f: Vec<&str> = meta.trim_start_matches(':').split(' ').collect();
        if f.len() < 4 {
            break;
        }
        v.insert(path.to_string(), (side(f[0], f[2]), side(f[1], f[3])));
    }
    v
}

/// What `branch` changes over its merge-base with the trunk: `trunk...branch`.
fn branch_changes(repo: &Repo, branch: &str) -> Res<Changes> {
    let p = &repo.primary;
    let tip = format!("refs/heads/{branch}");
    let mb = git::git(
        p,
        &["merge-base", &format!("refs/heads/{}", repo.trunk), &tip],
    )?;
    let out = git::git(
        p,
        &[
            "diff",
            "--raw",
            "-z",
            "--no-renames",
            "--full-index",
            "--no-abbrev",
            &mb,
            &tip,
        ],
    )?;
    Ok(raw_changes(&out))
}

/// The checkout's uncommitted changes against HEAD, untracked files included, each
/// working file hashed as `git add` would stage it (and nothing written). An error
/// is any reason the changes cannot be compared or discarded safely.
fn working_changes(dir: &Path) -> Res<Changes> {
    let names = |args: &[&str]| -> Res<std::collections::HashSet<String>> {
        Ok(git::git(dir, args)?
            .split('\0')
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect())
    };
    let staged = names(&["diff", "--cached", "--name-only", "-z", "--no-renames"])?;
    let unstaged = names(&["diff", "--name-only", "-z", "--no-renames"])?;
    if let Some(p) = staged.intersection(&unstaged).next() {
        bail!("{p} is partly staged");
    }
    let raw = git::git(
        dir,
        &[
            "diff",
            "--raw",
            "-z",
            "--no-renames",
            "--full-index",
            "--no-abbrev",
            "HEAD",
        ],
    )?;
    let mut v = raw_changes(&raw);
    for p in git::git(dir, &["ls-files", "-o", "--exclude-standard", "-z"])?
        .split('\0')
        .filter(|s| !s.is_empty())
    {
        let old = v.remove(p).and_then(|(o, _)| o);
        v.insert(p.to_string(), (old, Some((String::new(), String::new()))));
    }
    // A file that turns into a directory, or back: restoring one side would take
    // whatever else lives under that path with it, ignored files included.
    for p in v.keys() {
        if p.contains(['\u{fffd}', '\n']) {
            bail!("cannot compare the path {p:?}");
        }
        let mut a = p.as_str();
        while let Some((parent, _)) = a.rsplit_once('/') {
            if v.contains_key(parent) {
                bail!("{parent} and {p} both change: a file becomes a directory");
            }
            a = parent;
        }
    }
    // Hash what is on disk now; the index's idea of a working file may be stale.
    let mut plain = Vec::new();
    for (p, (old, new)) in v.iter_mut() {
        if old.as_ref().is_some_and(|o| o.0 == "160000") {
            bail!("{p} is a submodule");
        }
        // Restoring writes through every parent: each must be a real directory.
        let mut a = p.as_str();
        while let Some((parent, _)) = a.rsplit_once('/') {
            if let Ok(m) = fs::symlink_metadata(dir.join(parent))
                && !m.is_dir()
            {
                bail!("{parent} is not a directory on disk");
            }
            a = parent;
        }
        let file = dir.join(p);
        let Some(n) = new else {
            // Deleted as far as git knows, but something git does not list may
            // stand there — an ignored file, or a directory of them.
            if fs::symlink_metadata(&file).is_ok() {
                bail!("{p} is deleted, but something is on disk there");
            }
            continue;
        };
        let meta = fs::symlink_metadata(&file).map_err(|e| format!("{p}: {e}"))?;
        if meta.file_type().is_symlink() {
            let target = fs::read_link(&file).map_err(|e| format!("{p}: {e}"))?;
            let o = git::raw(
                dir,
                &["hash-object", "--stdin"],
                &[],
                Some(&target.to_string_lossy()),
            )?;
            if !o.ok {
                bail!("cannot hash {p}");
            }
            *n = ("120000".into(), o.stdout.trim().to_string());
        } else if meta.is_file() {
            use std::os::unix::fs::PermissionsExt;
            let mode = if meta.permissions().mode() & 0o111 != 0 {
                "100755"
            } else {
                "100644"
            };
            *n = (mode.into(), String::new());
            if p.starts_with('"') || p.contains('\r') {
                // --stdin-paths would read these as C-quoted or strip the CR.
                let o = git::raw(dir, &["hash-object", "--", p], &[], None)?;
                if !o.ok {
                    bail!("cannot hash {p}");
                }
                n.1 = o.stdout.trim().to_string();
            } else {
                plain.push((p.clone(), n));
            }
        } else {
            bail!("{p} is not a file");
        }
    }
    if !plain.is_empty() {
        let list: String = plain.iter().map(|(p, _)| format!("{p}\n")).collect();
        let o = git::raw(dir, &["hash-object", "--stdin-paths"], &[], Some(&list))?;
        let shas: Vec<&str> = o.stdout.lines().collect();
        if !o.ok || shas.len() != plain.len() {
            bail!("cannot hash the changed files");
        }
        for ((_, n), sha) in plain.iter_mut().zip(shas) {
            n.1 = sha.to_string();
        }
    }
    Ok(v)
}

/// Branches whose tips hold every changed path as the checkout does: one git call
/// for all of them, so the full diff runs only for the few that could be a copy.
fn candidates(repo: &Repo, work: &Changes) -> Res<Vec<String>> {
    let branches: Vec<String> = git::git(
        &repo.primary,
        &["for-each-ref", "--format=%(refname)", "refs/heads"],
    )?
    .lines()
    .filter_map(|r| r.strip_prefix("refs/heads/"))
    .filter(|b| *b != repo.trunk)
    .map(String::from)
    .collect();
    // cat-file reads a line each, stripping a trailing CR: such a path is left to
    // the full comparison rather than asked about wrongly.
    let work: Vec<_> = work.iter().filter(|(p, _)| !p.contains('\r')).collect();
    if work.is_empty() {
        return Ok(branches);
    }
    let mut query = String::new();
    for b in &branches {
        for (p, _) in &work {
            query.push_str(&format!("refs/heads/{b}:{p}\n"));
        }
    }
    let o = git::raw(
        &repo.primary,
        &["cat-file", "--batch-check=%(objectname)"],
        &[],
        Some(&query),
    )?;
    let lines: Vec<&str> = o.stdout.lines().collect();
    if !o.ok || lines.len() != branches.len() * work.len() {
        bail!("git cat-file --batch-check: {}", o.stderr.trim());
    }
    Ok(branches
        .into_iter()
        .zip(lines.chunks(work.len()))
        .filter(|(_, got)| {
            work.iter()
                .zip(got.iter())
                .all(|((_, (_, new)), g)| match new {
                    Some((_, sha)) => sha == g,
                    None => g.ends_with(" missing"),
                })
        })
        .map(|(b, _)| b)
        .collect())
}

/// Branches whose whole diff the trunk checkout holds uncommitted, exactly — with
/// the checkout. Such a copy blocks shipping the branch and is safe to discard.
/// Anything that stops the comparison means no copy: doctor must never fail on it.
fn copies(repo: &Repo) -> Option<(PathBuf, Vec<String>)> {
    let dir = repo.trunk_checkout().ok()??;
    if !git::dirty(&dir).ok()? {
        return None;
    }
    let work = working_changes(&dir).ok()?;
    if work.is_empty() {
        return None;
    }
    let found = candidates(repo, &work)
        .ok()?
        .into_iter()
        .filter(|b| branch_changes(repo, b).is_ok_and(|c| c == work))
        .collect();
    Some((dir, found))
}

/// Doctor's notes: each branch the trunk checkout holds an uncommitted copy of.
pub fn copy_notes(repo: &Repo) -> Vec<String> {
    let Some((dir, found)) = copies(repo) else {
        return Vec::new();
    };
    found
        .iter()
        .map(|b| {
            format!(
                "{} holds {b}'s diff uncommitted, exactly — `5w wt discard-copy {b}` discards it",
                dir.display()
            )
        })
        .collect()
}

fn discard_copy(repo: &Repo, branch: &str) -> Res<()> {
    if branch == repo.trunk || !git::branch_exists(&repo.primary, branch) {
        bail!("no branch {branch} other than the trunk (see: git branch)");
    }
    let Some(dir) = repo.trunk_checkout()? else {
        bail!(
            "no worktree has {} checked out; nothing to discard",
            repo.trunk
        )
    };
    let work = working_changes(&dir).map_err(|e| {
        format!(
            "{}: {e}; nothing discarded — `git -C {} status` to see it",
            dir.display(),
            dir.display()
        )
    })?;
    if work.is_empty() {
        bail!(
            "{} has no uncommitted changes; nothing to discard",
            dir.display()
        );
    }
    if work != branch_changes(repo, branch)? {
        bail!(
            "{}'s uncommitted changes are not exactly {branch}'s diff; nothing discarded — compare `git -C {} diff HEAD` with `git diff {}...{branch}`",
            dir.display(),
            dir.display(),
            repo.trunk
        );
    }
    // Everything is checked above; from here on only the compared paths change.
    let env = [("GIT_LITERAL_PATHSPECS", "1")];
    let tracked: Vec<&str> = (work.iter())
        .filter(|(_, (old, _))| old.is_some())
        .map(|(p, _)| p.as_str())
        .collect();
    let added: Vec<&str> = (work.iter())
        .filter(|(_, (old, _))| old.is_none())
        .map(|(p, _)| p.as_str())
        .collect();
    if !added.is_empty() {
        let mut args = vec!["rm", "--cached", "-q", "--ignore-unmatch", "--"];
        args.extend(&added);
        let o = git::raw(&dir, &args, &env, None)?;
        if !o.ok {
            bail!("git rm --cached: {}", o.stderr.trim());
        }
    }
    if !tracked.is_empty() {
        let mut args = vec!["restore", "--source=HEAD", "--staged", "--worktree", "--"];
        args.extend(&tracked);
        let o = git::raw(&dir, &args, &env, None)?;
        if !o.ok {
            bail!("git restore: {}", o.stderr.trim());
        }
    }
    for p in added {
        let file = dir.join(p);
        fs::remove_file(&file).map_err(|e| format!("{p}: {e}"))?;
        // Directories the branch added go with their last file.
        let mut d = file.parent();
        while let Some(parent) = d {
            if parent == dir || fs::remove_dir(parent).is_err() {
                break;
            }
            d = parent.parent();
        }
    }
    println!(
        "wt: discarded {} file(s) in {} — the uncommitted copy of {branch}; the branch is untouched",
        work.len(),
        dir.display()
    );
    Ok(())
}

fn setup(repo: &Repo) -> Res<()> {
    let p = &repo.primary;
    if let Err(e) = crate::lint::hook(repo, &["install".to_string()]) {
        println!("wt: pre-commit hook not installed: {e}");
    }
    // Keeps a stack consistent even when rebasing by hand.
    git::git(p, &["config", "rebase.updateRefs", "true"])?;
    if git::has_git_town() {
        git::git(p, &["config", "git-town.main-branch", &repo.trunk])?;
        if !repo.cfg.perennial.is_empty() {
            git::git(
                p,
                &[
                    "config",
                    "git-town.perennial-branches",
                    &repo.cfg.perennial.join(" "),
                ],
            )?;
        }
        for (k, v) in [
            ("git-town.sync-feature-strategy", "rebase"),
            ("git-town.sync-perennial-strategy", "ff-only"),
            ("git-town.ship-strategy", "fast-forward"),
            ("git-town.share-new-branches", "no"),
            ("git-town.unknown-branch-type", "feature"),
        ] {
            git::git(p, &["config", k, v])?;
        }
        println!("wt: git and git-town configured (trunk {})", repo.trunk);
    } else {
        println!(
            "wt: git configured (trunk {}); git-town not installed, which is fine",
            repo.trunk
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::glob_match;
    #[test]
    fn globs() {
        assert!(glob_match(b"*.png", b"a.png"));
        assert!(glob_match(b"Screenshot-*.png", b"Screenshot-1.png"));
        assert!(!glob_match(b"*.png", b"a.jpg"));
        assert!(glob_match(b"*", b"anything"));
    }
}

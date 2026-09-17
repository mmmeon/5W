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
                    f if f.starts_with("--") => bail!("unknown flag {f} ({usage})"),
                    b if branch.is_none() => branch = Some(b),
                    _ => bail!("{usage}"),
                }
            }
            remove(repo, branch.ok_or(usage)?, force)
        }
        "link" => link(repo, &target(repo, rest)?).map(|_| ()),
        "install" => install(repo, &target(repo, rest)?),
        "setup" => setup(repo),
        "discard-copy" => {
            let usage = "usage: 5w wt discard-copy <branch>";
            match rest {
                [b] if !b.starts_with("--") => discard_copy(repo, b),
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

fn target(repo: &Repo, rest: &[String]) -> Res<PathBuf> {
    Ok(rest
        .first()
        .map(PathBuf::from)
        .unwrap_or_else(|| repo.cwd.clone()))
}

fn slug(b: &str) -> String {
    b.replace('/', "-")
}

fn flags(rest: &[String], allowed: &[&str]) -> Res<(Option<String>, Vec<String>, Option<String>)> {
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
                bail!("unknown flag {a}");
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
    let (branch, seen, from) = flags(rest, &["--from", "--install"])?;
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
    let (branch, seen, _) = flags(rest, &["--install"])?;
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

fn ls(repo: &Repo) -> Res<()> {
    let home = std::env::var("HOME").unwrap_or_default();
    for w in git::worktrees(&repo.primary)? {
        let mut p = w.path.display().to_string();
        if !home.is_empty() && p.starts_with(&home) {
            p = format!("~{}", &p[home.len()..]);
        }
        match &w.branch {
            Some(b) => {
                let parent = git::parent_of(&repo.primary, b).unwrap_or("-".into());
                println!("{b:<32} {p:<40} parent: {parent}");
            }
            None => println!("{:<32} {p}", "(detached)"),
        }
    }
    Ok(())
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
fn unlink(repo: &Repo, dest: &Path) -> Res<()> {
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
    if n > 0 {
        println!("wt: unlinked {n} artifact(s)");
    }
    Ok(())
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
            "{} has uncommitted or untracked files.\n  Commit or clean them, or `5w wt rm {branch} --force` to discard them.",
            dir.display()
        );
    }
    unlink(repo, &dir)?;
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
    let mb = git::git(p, &["merge-base", &repo.trunk, branch])?;
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
            branch,
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
        let Some(n) = new else { continue };
        let file = dir.join(p);
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
            plain.push((p.clone(), n));
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
        &["for-each-ref", "--format=%(refname:short)", "refs/heads"],
    )?
    .lines()
    .filter(|b| *b != repo.trunk)
    .map(String::from)
    .collect();
    let mut query = String::new();
    for b in &branches {
        for p in work.keys() {
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
            work.values()
                .zip(got.iter())
                .all(|((_, new), g)| match new {
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

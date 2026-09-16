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
            let b = rest.first().ok_or("usage: 5w wt rm <branch> [--force]")?;
            remove(
                repo,
                b,
                rest.get(1).map(|s| s == "--force").unwrap_or(false),
            )
        }
        "link" => link(repo, &target(repo, rest)?).map(|_| ()),
        "install" => install(repo, &target(repo, rest)?),
        "setup" => setup(repo),
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

fn setup(repo: &Repo) -> Res<()> {
    let p = &repo.primary;
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

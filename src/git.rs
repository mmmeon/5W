use crate::util::Res;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub struct Out {
    pub ok: bool,
    pub stdout: String,
    pub stderr: String,
}

pub fn raw(dir: &Path, args: &[&str], env: &[(&str, &str)], input: Option<&str>) -> Res<Out> {
    let mut c = Command::new("git");
    c.arg("-C").arg(dir).args(args);
    for (k, v) in env {
        c.env(k, v);
    }
    c.stdin(if input.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    })
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    let mut child = c.spawn().map_err(|e| format!("cannot run git: {e}"))?;
    let writer = input.map(|s| {
        let mut stdin = child.stdin.take().expect("piped stdin");
        let s = s.to_owned();
        std::thread::spawn(move || {
            let _ = stdin.write_all(s.as_bytes());
        })
    });
    let o = child.wait_with_output().map_err(|e| format!("git: {e}"))?;
    if let Some(w) = writer {
        let _ = w.join();
    }
    Ok(Out {
        ok: o.status.success(),
        stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
    })
}

/// Run git, trailing newlines trimmed; a non-zero exit is an error carrying stderr.
pub fn git(dir: &Path, args: &[&str]) -> Res<String> {
    let o = raw(dir, args, &[], None)?;
    if !o.ok {
        return Err(format!("git {}: {}", args.join(" "), o.stderr.trim()));
    }
    Ok(o.stdout.trim_end_matches('\n').to_string())
}

pub fn opt(dir: &Path, args: &[&str]) -> Option<String> {
    match raw(dir, args, &[], None) {
        Ok(o) if o.ok => Some(o.stdout.trim_end_matches('\n').to_string()),
        _ => None,
    }
}

pub fn ok(dir: &Path, args: &[&str]) -> bool {
    raw(dir, args, &[], None).map(|o| o.ok).unwrap_or(false)
}

pub fn branch_exists(dir: &Path, b: &str) -> bool {
    ok(
        dir,
        &[
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{b}"),
        ],
    )
}

pub fn rev(dir: &Path, r: &str) -> Option<String> {
    opt(
        dir,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{r}^{{commit}}"),
        ],
    )
}

pub fn current_branch(dir: &Path) -> Option<String> {
    opt(dir, &["symbolic-ref", "--quiet", "--short", "HEAD"]).filter(|s| !s.is_empty())
}

pub struct Worktree {
    pub path: PathBuf,
    pub branch: Option<String>,
}

pub fn worktrees(dir: &Path) -> Res<Vec<Worktree>> {
    let out = git(dir, &["worktree", "list", "--porcelain"])?;
    let mut v: Vec<Worktree> = Vec::new();
    for line in out.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            v.push(Worktree {
                path: PathBuf::from(p),
                branch: None,
            });
        } else if let Some(b) = line.strip_prefix("branch refs/heads/")
            && let Some(w) = v.last_mut()
        {
            w.branch = Some(b.to_string());
        }
    }
    Ok(v)
}

pub fn worktree_of(dir: &Path, branch: &str) -> Res<Option<PathBuf>> {
    Ok(worktrees(dir)?
        .into_iter()
        .find(|w| w.branch.as_deref() == Some(branch))
        .map(|w| w.path))
}

/// Any change at all, untracked files included.
pub fn dirty(dir: &Path) -> Res<bool> {
    Ok(!git(dir, &["status", "--porcelain"])?.is_empty())
}

/// Tracked changes only. An untracked stray file does not stop a fast-forward
/// unless the merge would overwrite it, and git says so itself when it does.
pub fn dirty_tracked(dir: &Path) -> Res<bool> {
    Ok(!git(dir, &["status", "--porcelain", "--untracked-files=no"])?.is_empty())
}

/// The patch-id of everything `tip` adds over its merge-base with `base`. Equal
/// patch-ids mean the same change, whatever it was rebased onto.
pub fn change_id(dir: &Path, base: &str, tip: &str) -> Res<String> {
    let mb = git(dir, &["merge-base", base, tip])?;
    let diff = raw(
        dir,
        &["diff", "--no-color", "--no-ext-diff", &mb, tip],
        &[],
        None,
    )?;
    if !diff.ok {
        return Err(format!("git diff {mb} {tip}: {}", diff.stderr.trim()));
    }
    if diff.stdout.is_empty() {
        return Ok("empty".into());
    }
    let pid = raw(dir, &["patch-id", "--stable"], &[], Some(&diff.stdout))?;
    Ok(pid
        .stdout
        .split_whitespace()
        .next()
        .unwrap_or("empty")
        .to_string())
}

pub fn parent_of(dir: &Path, branch: &str) -> Option<String> {
    opt(
        dir,
        &["config", &format!("git-town-branch.{branch}.parent")],
    )
    .filter(|s| !s.is_empty())
}

pub fn has_git_town() -> bool {
    Command::new("git")
        .args(["town", "--version"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

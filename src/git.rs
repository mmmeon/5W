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
    // Every object is what it is: a replace ref (which can be pushed) must not
    // dress a post-review commit or blob up as the reviewed one, nor a graft
    // rewrite ancestry. The graft file is a path that cannot exist: an existing
    // one, even empty, prints git's deprecation hint on every call.
    c.env("GIT_NO_REPLACE_OBJECTS", "1")
        .env("GIT_GRAFT_FILE", "/dev/null/no-grafts");
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

/// The commit `r` names, as a full sha — or None. rev-parse answers `^HEAD`
/// with `^<sha>`, which is not a commit; only a plain sha (SHA-1 or SHA-256) is.
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
    .filter(|s| matches!(s.len(), 40 | 64) && s.bytes().all(|b| b.is_ascii_hexdigit()))
}

pub fn current_branch(dir: &Path) -> Option<String> {
    opt(dir, &["symbolic-ref", "--quiet", "--short", "HEAD"]).filter(|s| !s.is_empty())
}

pub struct Worktree {
    pub path: PathBuf,
    pub branch: Option<String>,
    /// Locked, or its directory gone (`prunable`): not ours to remove.
    pub held: bool,
}

pub fn worktrees(dir: &Path) -> Res<Vec<Worktree>> {
    let out = git(dir, &["worktree", "list", "--porcelain"])?;
    let mut v: Vec<Worktree> = Vec::new();
    for line in out.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            v.push(Worktree {
                path: PathBuf::from(p),
                branch: None,
                held: false,
            });
        } else if let Some(b) = line.strip_prefix("branch refs/heads/")
            && let Some(w) = v.last_mut()
        {
            w.branch = Some(b.to_string());
        } else if (line == "locked" || line.starts_with("locked ") || line.starts_with("prunable"))
            && let Some(w) = v.last_mut()
        {
            w.held = true;
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

/// An identity for everything `tip` adds over its merge-base with `base`, the
/// same across a clean rebase and different for any other change.
///
/// Not `git patch-id`: it ignores whitespace, so an indentation change made after
/// review — a semantic change in Python or YAML — would pass as the reviewed one.
/// This hashes the exact diff, binary content and modes included, with only the
/// parts a rebase legitimately moves taken out: `index` blob lines and the line
/// numbers in hunk headers. Context lines stay, so a rebase that changed text
/// next to the change reads as different and asks for a fresh look — the safe way
/// to be wrong.
///
/// The diff is `diff-tree`, plumbing, with every setting a repository's config,
/// attributes or environment could use to change its output pinned: a textconv
/// driver or `-diff` must not hide content, `diff.context=0` must not drop the
/// context, `diff.ignoreSubmodules` must not drop a gitlink.
pub fn change_id(dir: &Path, base: &str, tip: &str) -> Res<String> {
    let mb = git(dir, &["merge-base", base, tip])?;
    let diff = raw(
        dir,
        &pinned_diff(&["-p", "--binary", "--full-index", &mb, tip]),
        &PINNED_ENV,
        None,
    )?;
    if !diff.ok {
        return Err(format!("git diff-tree {mb} {tip}: {}", diff.stderr.trim()));
    }
    let mut norm = String::with_capacity(diff.stdout.len());
    for line in diff.stdout.split_inclusive('\n') {
        if line.starts_with("index ") {
            continue;
        }
        if line.starts_with("@@ ")
            && let Some(end) = line[3..].find(" @@")
        {
            norm.push_str("@@");
            norm.push_str(&line[3 + end + 3..]);
            continue;
        }
        norm.push_str(line);
    }
    let o = raw(dir, &["hash-object", "--stdin"], &[], Some(&norm))?;
    Ok(o.stdout.trim().to_string())
}

const PINNED_ENV: [(&str, &str); 2] = [("GIT_DIFF_OPTS", "--unified=3"), ("GIT_EXTERNAL_DIFF", "")];

/// `git diff-tree -r` with the given arguments, and every option config could
/// otherwise set spelled out, for output that depends on the commits alone.
pub fn pinned_diff<'a>(args: &[&'a str]) -> Vec<&'a str> {
    let mut v = vec![
        "-c",
        "core.quotePath=true",
        "-c",
        "diff.suppressBlankEmpty=false",
        "-c",
        "diff.noprefix=false",
        "-c",
        "diff.mnemonicPrefix=false",
        "-c",
        "diff.relative=false",
        "-c",
        "diff.orderFile=",
        "-c",
        "diff.renames=false",
        "-c",
        "diff.ignoreSubmodules=none",
        "diff-tree",
        "-r",
        "--no-color",
        "--no-ext-diff",
        "--no-textconv",
        "--no-renames",
        "--no-relative",
        "--ignore-submodules=none",
        "--unified=3",
        "--inter-hunk-context=0",
        "--diff-algorithm=myers",
        "--indent-heuristic",
        "--src-prefix=a/",
        "--dst-prefix=b/",
        "-O/dev/null",
    ];
    v.extend_from_slice(args);
    v
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

/// `git commit-tree`, signed when the repository asks for signed commits.
///
/// Plumbing does not read `commit.gpgsign`, so without this every commit 5w
/// writes — queue changes, squashes — would be the unsigned one in a history
/// that is otherwise signed.
pub fn commit_tree(
    dir: &Path,
    tree: &str,
    parent: &str,
    message: &str,
    env: &[(&str, &str)],
) -> Res<String> {
    let mut args = vec![
        "commit-tree".to_string(),
        tree.to_string(),
        "-p".into(),
        parent.to_string(),
    ];
    if opt(dir, &["config", "--bool", "commit.gpgsign"]).as_deref() == Some("true") {
        match opt(dir, &["config", "user.signingkey"]).filter(|k| !k.is_empty()) {
            Some(k) => args.push(format!("-S{k}")),
            None => args.push("-S".into()),
        }
    }
    args.extend(["-F".to_string(), "-".to_string()]);
    let refs: Vec<&str> = args.iter().map(|a| a.as_str()).collect();
    let o = raw(dir, &refs, env, Some(&format!("{}\n", message.trim_end())))?;
    if !o.ok {
        return Err(format!("git commit-tree: {}", o.stderr.trim()));
    }
    Ok(o.stdout.trim().to_string())
}

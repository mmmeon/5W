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
    // Every call names its directory, and git answers for the repository found
    // there — not one the environment names. A hook exports these (git sets
    // GIT_DIR in a linked worktree and GIT_INDEX_FILE for every commit), and
    // inherited by `git -C <another worktree>` they point that call at the
    // hook's gitdir and index: a status reads the wrong index, a checkout kept
    // in step writes it. `check_env` refuses a GIT_DIR naming another repository
    // before anything runs; `lint --staged` hands GIT_INDEX_FILE back explicitly.
    for k in SCOPE {
        c.env_remove(k);
    }
    // A pre-receive hook reads the pushed objects from git's quarantine, which
    // git names in these two; outside one they are cleared like the rest.
    if !quarantined() {
        c.env_remove("GIT_OBJECT_DIRECTORY")
            .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES");
    }
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
        // Only ever carried into a refusal, which is one line.
        stderr: crate::util::one_line(&String::from_utf8_lossy(&o.stderr)),
    })
}

/// What the environment can point a git call at other than the repository in
/// its directory: the gitdir, worktree, common dir, index and ref namespace.
const SCOPE: [&str; 5] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
    "GIT_NAMESPACE",
];

/// Inside receive-pack's quarantine: GIT_OBJECT_DIRECTORY is the path git says
/// holds the objects being pushed, a directory inside the repository's own
/// object store, which git names as the alternate.
fn quarantined() -> bool {
    let (Some(q), Some(o), Some(alt)) = (
        std::env::var_os("GIT_QUARANTINE_PATH"),
        std::env::var_os("GIT_OBJECT_DIRECTORY"),
        std::env::var_os("GIT_ALTERNATE_OBJECT_DIRECTORIES"),
    ) else {
        return false;
    };
    if q != o {
        return false;
    }
    let canon = |p: &Path| std::fs::canonicalize(p).ok();
    let Some(store) = Path::new(&q).parent().and_then(canon) else {
        return false;
    };
    alternates(std::os::unix::ffi::OsStrExt::as_bytes(alt.as_os_str()))
        .iter()
        .any(|a| canon(a).as_ref() == Some(&store))
}

/// GIT_ALTERNATE_OBJECT_DIRECTORIES as git reads it: entries separated by `:`,
/// an entry that starts with `"` C-quoted — as git writes a path holding `:`.
fn alternates(mut s: &[u8]) -> Vec<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    let mut out = Vec::new();
    while !s.is_empty() {
        let mut entry = Vec::new();
        if s[0] == b'"' {
            let mut i = 1;
            loop {
                match s.get(i) {
                    None => return out, // unterminated: git ignores the rest too
                    Some(b'"') => {
                        i += 1;
                        break;
                    }
                    Some(b'\\') => {
                        let (byte, len) = match s.get(i + 1) {
                            Some(d @ b'0'..=b'3')
                                if s.get(i + 2).is_some_and(|c| (b'0'..=b'7').contains(c))
                                    && s.get(i + 3).is_some_and(|c| (b'0'..=b'7').contains(c)) =>
                            {
                                (
                                    (d - b'0') << 6 | (s[i + 2] - b'0') << 3 | (s[i + 3] - b'0'),
                                    4,
                                )
                            }
                            Some(b'a') => (7, 2),
                            Some(b'b') => (8, 2),
                            Some(b't') => (b'\t', 2),
                            Some(b'n') => (b'\n', 2),
                            Some(b'v') => (11, 2),
                            Some(b'f') => (12, 2),
                            Some(b'r') => (b'\r', 2),
                            Some(c @ (b'\\' | b'"')) => (*c, 2),
                            _ => return out,
                        };
                        entry.push(byte);
                        i += len;
                    }
                    Some(c) => {
                        entry.push(*c);
                        i += 1;
                    }
                }
            }
            s = &s[i..];
            match s.first() {
                None => {}
                Some(b':') => s = &s[1..],
                Some(_) => return out,
            }
        } else {
            let end = s.iter().position(|&c| c == b':').unwrap_or(s.len());
            entry.extend_from_slice(&s[..end]);
            s = &s[(end + 1).min(s.len())..];
        }
        if !entry.is_empty() {
            out.push(PathBuf::from(std::ffi::OsString::from_vec(entry)));
        }
    }
    out
}

/// Refuse a GIT_DIR, GIT_WORK_TREE or GIT_COMMON_DIR that names another
/// repository or worktree than the directory 5w runs in. One naming the same
/// one — as git sets for a hook — is fine: every call finds it from its path.
pub fn check_env() -> Res<()> {
    let set: Vec<&str> = ["GIT_DIR", "GIT_WORK_TREE", "GIT_COMMON_DIR"]
        .into_iter()
        .filter(|k| std::env::var_os(k).is_some())
        .collect();
    if set.is_empty() {
        return Ok(());
    }
    // One rev-parse per side: the gitdir, the common dir, and where the
    // worktree's top is from here (empty in a bare repository).
    let probe = |inherit: bool| {
        let mut c = Command::new("git");
        c.args([
            "rev-parse",
            "--path-format=absolute",
            "--absolute-git-dir",
            "--git-common-dir",
            "--show-cdup",
            "--show-prefix",
        ]);
        if !inherit {
            for k in SCOPE {
                c.env_remove(k);
            }
        }
        c.stdin(Stdio::null()).stderr(Stdio::null());
        c.output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| o.stdout)
    };
    let named = probe(true);
    let here = probe(false);
    if named != here {
        let names: Vec<String> = set
            .iter()
            .map(|k| {
                format!(
                    "{k}={}",
                    std::env::var_os(k).unwrap_or_default().to_string_lossy()
                )
            })
            .collect();
        return Err(crate::util::one_line(&format!(
            "{} names another repository or worktree than the one here: unset {}, or run 5w inside it",
            names.join(" "),
            set.join(" ")
        )));
    }
    Ok(())
}

/// The index `lint --staged` reads: the one the caller names, which during
/// `git commit -a` or `git commit <path>` is git's temporary index of what is
/// being committed.
pub fn caller_index() -> Option<String> {
    std::env::var("GIT_INDEX_FILE")
        .ok()
        .filter(|s| !s.is_empty())
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

/// What a `submitted:` or `reviewed:` value names.
#[derive(Debug, PartialEq)]
pub enum Recorded {
    /// The one commit it names, as a full sha.
    Commit(String),
    /// A prefix more than one commit shares.
    Ambiguous,
    /// No commit here has that name.
    Missing,
    /// Not a commit name: not hex, or under 7 digits.
    Invalid,
}

/// Resolve a recorded sha. A full one (40 or 64 hex) is that object; a prefix,
/// as rows written before full shas hold, is looked up among objects only —
/// never as a ref — and names a commit only when exactly one commit has it.
pub fn recorded(dir: &Path, value: &str) -> Recorded {
    let v = value.to_ascii_lowercase();
    if v.len() < 7 || v.len() > 64 || !v.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Recorded::Invalid;
    }
    if matches!(v.len(), 40 | 64) {
        return match rev(dir, &v) {
            Some(c) if c == v => Recorded::Commit(c),
            _ => Recorded::Missing,
        };
    }
    let Some(found) = opt(dir, &["rev-parse", &format!("--disambiguate={v}")]) else {
        return Recorded::Missing;
    };
    let types = if found.is_empty() {
        String::new()
    } else {
        raw(
            dir,
            &["cat-file", "--batch-check=%(objecttype) %(objectname)"],
            &[],
            Some(&format!("{found}\n")),
        )
        .map(|o| o.stdout)
        .unwrap_or_default()
    };
    let commits: Vec<&str> = types
        .lines()
        .filter_map(|l| l.strip_prefix("commit "))
        .collect();
    match commits.as_slice() {
        [] => Recorded::Missing,
        [c] => Recorded::Commit(c.to_string()),
        _ => Recorded::Ambiguous,
    }
}

/// Whether the recorded `value` names exactly the commit `sha`.
pub fn names(dir: &Path, value: &str, sha: &str) -> bool {
    recorded(dir, value) == Recorded::Commit(sha.to_string())
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

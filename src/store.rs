//! Where the queue lives, and how a state change reaches it.
//!
//! The queue lives on the trunk, in one file, and every change to it is its own
//! commit. Two properties the shell version could not give:
//!
//! 1. **A commit carries exactly its own change.** The edit is applied to the
//!    trunk's committed copy and committed with plumbing (a private index,
//!    `commit-tree`, and a compare-and-swap `update-ref`) — never with
//!    `git commit` over the working tree. A peer's uncommitted row in the same
//!    file is left uncommitted, instead of being swept in or making the command
//!    decline.
//! 2. **Writers are serialised** by a lock in the common git dir, so two agents
//!    adding at once cannot mint the same id or lose each other's line.
//!
//! The same edit is then applied to the trunk checkout's index entry and file,
//! wherever that checkout is, so it shows the change and its next commit does
//! not revert it.

use crate::bail;
use crate::config::Config;
use crate::git;
use crate::queue::{self, Doc};
use crate::util::Res;
use std::cell::RefCell;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

pub struct Repo {
    pub cwd: PathBuf,
    pub primary: PathBuf,
    pub common: PathBuf,
    pub cfg: Config,
    pub trunk: String,
}

pub const CONFIG_FILE: &str = ".5w.toml";

/// Resolve `.` and `..` components without touching the filesystem.
fn normalize(p: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => match out.components().next_back() {
                Some(Component::Normal(_)) => {
                    out.pop();
                }
                Some(Component::RootDir | Component::Prefix(_)) => {}
                _ => out.push(".."),
            },
            c => out.push(c),
        }
    }
    out
}

impl Repo {
    pub fn open() -> Res<Repo> {
        let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
        let common = git::git(
            &cwd,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        )
        .map_err(|_| "not inside a git repository".to_string())?;
        let common = PathBuf::from(common);
        // A bare repository — a git server running the pre-receive hook — has no
        // working tree; the repository directory itself is where git runs.
        let bare =
            git::opt(&cwd, &["rev-parse", "--is-bare-repository"]).as_deref() == Some("true");
        let primary = if bare {
            common.clone()
        } else {
            common
                .parent()
                .ok_or("git common dir has no parent")?
                .to_path_buf()
        };
        // The config belongs to the trunk: read it from the trunk's checkout,
        // else the trunk's commit, else the primary (before `init` commits it).
        // The trunk's own name is needed to find it, so resolve that first from
        // git config, and let the config override it.
        let guess = std::env::var("FIVEW_TRUNK")
            .ok()
            .or_else(|| git::opt(&primary, &["config", "5w.trunk"]).filter(|s| !s.is_empty()))
            .or_else(|| {
                git::opt(&primary, &["config", "git-town.main-branch"]).filter(|s| !s.is_empty())
            })
            .unwrap_or_else(|| "main".into());
        let from_checkout = git::worktree_of(&primary, &guess)
            .ok()
            .flatten()
            .and_then(|w| fs::read_to_string(w.join(CONFIG_FILE)).ok());
        let src = from_checkout
            .or_else(|| {
                git::opt(
                    &primary,
                    &["show", &format!("refs/heads/{guess}:{CONFIG_FILE}")],
                )
            })
            // A CI checkout often has the trunk only as a remote-tracking ref.
            .or_else(|| {
                git::opt(
                    &primary,
                    &[
                        "show",
                        &format!("refs/remotes/origin/{guess}:{CONFIG_FILE}"),
                    ],
                )
            })
            .or_else(|| fs::read_to_string(primary.join(CONFIG_FILE)).ok());
        let cfg = match src {
            Some(s) => Config::from_toml(&s)?,
            None => Config::default(),
        };
        let trunk = cfg.trunk.clone().unwrap_or(guess);
        Ok(Repo {
            cwd,
            primary,
            common,
            cfg,
            trunk,
        })
    }

    /// The worktree that has the trunk checked out, if any. Git allows one, and
    /// it need not be the primary: a queue commit has to carry that checkout's
    /// index and file along wherever it is, or its next commit reverts the queue.
    pub fn trunk_checkout(&self) -> Res<Option<PathBuf>> {
        git::worktree_of(&self.primary, &self.trunk)
    }

    /// A file as the trunk's tip commits it.
    pub fn committed_file(&self, name: &str) -> Res<Option<String>> {
        let spec = format!("refs/heads/{}:{name}", self.trunk);
        let o = git::raw(&self.primary, &["show", &spec], &[], None)?;
        Ok(o.ok.then_some(o.stdout))
    }

    pub fn committed(&self) -> Res<Option<String>> {
        self.committed_file(&self.cfg.file)
    }

    /// What reads see: the trunk checkout's working copy when there is one (so a
    /// just-added row is visible), otherwise the trunk's committed copy.
    fn load_file(&self, name: &str) -> Res<Option<String>> {
        // Inside a batch, the edits before this one are written nowhere yet.
        let pending = BATCH.with(|b| {
            b.borrow().as_ref().and_then(|b| {
                let c = b.cur.iter().find(|c| c.name == name)?;
                match (&b.checkout, &c.working) {
                    (Some(_), Some(w)) => Some(w.clone()),
                    _ => (c.old_blob.is_some() || !c.committed.is_empty())
                        .then(|| c.committed.clone()),
                }
            })
        });
        if pending.is_some() {
            return Ok(pending);
        }
        if let Some(w) = self.trunk_checkout()?
            && let Ok(s) = fs::read_to_string(w.join(name))
        {
            return Ok(Some(s));
        }
        self.committed_file(name)
    }

    pub fn load(&self) -> Res<String> {
        match self.load_file(&self.cfg.file)? {
            Some(s) => Ok(s),
            None => bail!(
                "no {} on {} — `{} init`",
                self.cfg.file,
                self.trunk,
                self.cfg.cmd_tasks
            ),
        }
    }

    /// Closed tasks moved out of the queue by `archive`. Empty when there is none.
    pub fn load_archive(&self) -> Res<String> {
        Ok(self.load_file(&self.cfg.archive)?.unwrap_or_default())
    }

    /// Worktree root for new branches, with `.` and `..` resolved lexically so every path
    /// printed under it is clean. Lexical, not `fs::canonicalize`: symlinks stay as written.
    /// A relative root, from `$FIVEW_WT_ROOT` or `worktrees.root`, is read from the primary
    /// checkout whatever the current directory, so checks and `git worktree add` agree.
    pub fn wt_root(&self) -> PathBuf {
        // An empty or blank value counts as unset: joined on, it would put worktrees in the primary.
        let root = match std::env::var_os("FIVEW_WT_ROOT") {
            Some(r) if !r.to_string_lossy().trim().is_empty() => self.primary.join(r),
            _ => self.configured_wt_root(),
        };
        normalize(&root)
    }

    fn configured_wt_root(&self) -> PathBuf {
        match &self.cfg.wt_root {
            Some(r) => self.primary.join(r),
            None => {
                let name = self
                    .primary
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or("repo".into());
                self.primary
                    .parent()
                    .unwrap_or(&self.primary)
                    .join(format!("{name}-wt"))
            }
        }
    }

    pub fn is_perennial(&self, branch: &str) -> bool {
        if self.cfg.perennial.iter().any(|p| p == branch) {
            return true;
        }
        let town = git::opt(
            &self.primary,
            &["config", "--get-all", "git-town.perennial-branches"],
        )
        .unwrap_or_default();
        if town.split_whitespace().any(|p| p == branch) {
            return true;
        }
        git::opt(
            &self.primary,
            &["config", &format!("git-town-branch.{branch}.branchtype")],
        )
        .as_deref()
            == Some("perennial")
    }
}

pub struct Ctx {
    /// The next free id, computed under the lock from every copy of both files.
    pub next_id: u64,
}

/// One copy of the queue and its archive, edited together.
pub struct Files {
    pub queue: Doc,
    pub archive: Doc,
}

struct Lock(#[allow(dead_code)] fs::File);

fn lock(repo: &Repo) -> Res<Lock> {
    let path = repo.common.join("5w.lock");
    let f = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    f.lock()
        .map_err(|e| format!("cannot lock {}: {e}", path.display()))?;
    Ok(Lock(f))
}

/// Carry into `base` any task in `ids` that it lacks and `donor` has — a row
/// someone added and never committed — so an op on it does not fail.
fn carry(repo: &Repo, base: &str, donor: Option<&str>, ids: &[u64]) -> Doc {
    let mut d = Doc::new(base);
    if let Some(w) = donor {
        let wdoc = Doc::new(w);
        let wtasks = queue::parse(w);
        for &id in ids {
            if d.block(id).is_none()
                && let (Some((at, len)), Some(t)) =
                    (wdoc.block(id), wtasks.iter().find(|t| t.id == id))
            {
                let section = t
                    .section
                    .clone()
                    .unwrap_or_else(|| repo.cfg.open_section.clone());
                d.insert(
                    wdoc.lines[at..at + len].to_vec(),
                    &section,
                    Some(&repo.cfg.done_section),
                );
            }
        }
    }
    d
}

/// Per file: the committed text, the checkout's working text and the staged
/// text where it differs from the commit, and the staged blob id.
#[derive(Clone)]
struct Copies {
    name: String,
    old_blob: Option<String>,
    committed: String,
    working: Option<String>,
    staged_blob: Option<String>,
    staged: Option<String>,
}

fn copies(repo: &Repo, checkout: Option<&Path>, old: &str, name: &str) -> Res<Copies> {
    let old_blob = git::opt(
        &repo.primary,
        &["rev-parse", "--verify", "--quiet", &format!("{old}:{name}")],
    );
    let committed = match &old_blob {
        Some(b) => git::raw(&repo.primary, &["cat-file", "blob", b], &[], None)?.stdout,
        None => String::new(),
    };
    let working = checkout.and_then(|w| fs::read_to_string(w.join(name)).ok());
    let staged_blob = checkout.and_then(|w| {
        git::opt(w, &["ls-files", "-s", "--", name])
            .and_then(|l| l.split_whitespace().nth(1).map(String::from))
    });
    let staged = match (&staged_blob, &old_blob) {
        (Some(s), o) if Some(s) != o.as_ref() => {
            Some(git::raw(&repo.primary, &["cat-file", "blob", s], &[], None)?.stdout)
        }
        _ => None,
    };
    Ok(Copies {
        name: name.to_string(),
        old_blob,
        committed,
        working,
        staged_blob,
        staged,
    })
}

/// A batch in progress (`5w batch`): the lock, held from the first edit to the
/// commit; the trunk and its checkout as they were when it began; and every copy
/// of both files as the edits so far have left them.
struct Batch {
    _lock: Lock,
    old: String,
    checkout: Option<PathBuf>,
    orig: [Copies; 2],
    cur: [Copies; 2],
    messages: Vec<String>,
    /// Rows an edit in the batch has changed.
    edited: HashSet<u64>,
}

thread_local! {
    static BATCH: RefCell<Option<Batch>> = const { RefCell::new(None) };
}

/// Whether queue edits are being gathered into one batch commit.
pub fn batching() -> bool {
    BATCH.with(|b| b.borrow().is_some())
}

/// The lock, the trunk's tip and checkout, and both files' copies: where every
/// transaction, alone or batched, starts.
fn begin(repo: &Repo) -> Res<(Lock, String, Option<PathBuf>, Copies, Copies)> {
    let lock = lock(repo)?;
    let trunk_ref = format!("refs/heads/{}", repo.trunk);
    let Some(old) = git::rev(&repo.primary, &trunk_ref) else {
        bail!("no trunk branch {}", repo.trunk)
    };
    let checkout = repo.trunk_checkout()?;
    if let Some(w) = checkout.as_deref() {
        refuse_unmerged(repo, w)?;
    }
    for name in [&repo.cfg.file, &repo.cfg.archive] {
        refuse_link(repo, checkout.as_deref(), &old, name)?;
        if let Some(w) = checkout.as_deref() {
            refuse_working_link(repo, w, &old, name)?;
        }
    }
    let q = copies(repo, checkout.as_deref(), &old, &repo.cfg.file)?;
    if q.old_blob.is_none() {
        bail!(
            "{} is not committed on {} — `{} init`",
            repo.cfg.file,
            repo.trunk,
            repo.cfg.cmd_tasks
        );
    }
    let a = copies(repo, checkout.as_deref(), &old, &repo.cfg.archive)?;
    let (q, a) = catch_up(repo, checkout.as_deref(), &old, q, a)?;
    Ok((lock, old, checkout, q, a))
}

/// The mode and blob `rev` tracks at `name`.
fn entry(repo: &Repo, rev: &str, name: &str) -> Option<(String, String)> {
    let l = git::opt(&repo.primary, &["ls-tree", "--full-tree", rev, "--", name])?;
    let mut f = l.split_whitespace();
    let mode = f.next()?.to_string();
    f.next()?;
    Some((mode, f.next()?.to_string()))
}

/// Where a refusal's fix runs: `git -C <checkout>` and paths under it, so the
/// fix lands in the trunk checkout wherever it is typed; a relative form, to run
/// in a checkout of the trunk, when there is none.
fn in_checkout(checkout: Option<&Path>, name: &str) -> (String, String) {
    match checkout {
        Some(w) => (
            format!("git -C {}", shell_word(&w.to_string_lossy())),
            shell_word(&w.join(name).to_string_lossy()),
        ),
        None => ("git".into(), shell_word(name)),
    }
}

/// How far back the checkout's index is looked for among the trunk's versions.
const CATCH_UP_DEPTH: usize = 10;

/// Catch up a checkout that missed trunk commits — the trunk moved, then the
/// mirror failed (its `index.lock` held): left so, the next write would carry
/// the checkout's stale copies forward, and they would read as a revert.
///
/// Only where the checkout's index entries are exactly the files of a recent
/// trunk commit, so nothing is staged of its own: the index takes the trunk's
/// files, and the working files take each row changed since that the checkout
/// still has as that commit did — a row edited there by hand is left. Under the
/// lock, before planning; a copy that cannot be written is left for the write's
/// own refusal. Returns the copies as they now are.
fn catch_up(
    repo: &Repo,
    checkout: Option<&Path>,
    old: &str,
    q: Copies,
    a: Copies,
) -> Res<(Copies, Copies)> {
    let Some(w) = checkout else {
        return Ok((q, a));
    };
    if (q.staged_blob == q.old_blob && a.staged_blob == a.old_blob)
        || q.staged_blob.is_none()
        || (a.staged_blob != a.old_blob && a.old_blob.is_none())
    {
        return Ok((q, a));
    }
    let Some((_, seen)) = held_commit(
        repo,
        old,
        [&q.name, &a.name],
        [&q.staged_blob, &a.staged_blob],
    )?
    else {
        return Ok((q, a));
    };
    let text = |b: Option<String>| -> Res<String> {
        Ok(match b {
            Some(b) => git::raw(&repo.primary, &["cat-file", "blob", &b], &[], None)?.stdout,
            None => String::new(),
        })
    };
    let [bq, ba] = seen;
    let (sq, sa) = (text(bq)?, text(ba)?);
    // Per file, so a row moving between them counts.
    let mut ids = rows_changed([&sq, ""], [&q.committed, ""]);
    ids.extend(rows_changed(["", &sa], ["", &a.committed]));
    ids.sort_unstable();
    ids.dedup();
    if ids.is_empty() {
        return Ok((q, a));
    }

    let healed = (|| -> Res<()> {
        // Working files first: were the index then to fail, the next write
        // finds it behind again, and these rows already caught up.
        if let Some(wq) = &q.working {
            let wa = a.working.as_deref().unwrap_or(&sa);
            let mut docs = [Doc::new(wq), Doc::new(wa)];
            for &id in &ids {
                let texts = [docs[0].text(), docs[1].text()];
                let here = row([&texts[0], &texts[1]], id);
                if here != row([&sq, &sa], id) {
                    continue;
                }
                let there = row([&q.committed, &a.committed], id);
                match (here, there) {
                    (Some((f, s, _)), Some((tf, ts, lines))) if f == tf && s == ts => {
                        if let Some((at, len)) = docs[f].block(id) {
                            docs[f].lines.splice(at..at + len, lines);
                        }
                    }
                    (here, there) => {
                        if let Some((f, ..)) = here {
                            docs[f].take(|t| t.id == id);
                        }
                        if let Some((tf, ts, lines)) = there {
                            let section = ts.unwrap_or_else(|| repo.cfg.open_section.clone());
                            docs[tf].insert(lines, &section, Some(&repo.cfg.done_section));
                        }
                    }
                }
            }
            let (nq, na) = (docs[0].text(), docs[1].text());
            let changes: Vec<Change> = vec![
                (&q, &q.committed, Some(&nq), None),
                (&a, &a.committed, Some(&na), None),
            ];
            Pending::prepare(&repo.common, w, &changes)?
                .finish()
                .map_err(|(e, _)| e)?;
        }
        let entries: Vec<(&str, &str)> = [&q, &a]
            .into_iter()
            .filter(|c| c.staged_blob != c.old_blob)
            .filter_map(|c| Some((c.name.as_str(), c.old_blob.as_deref()?)))
            .collect();
        update_index(w, &entries)
    })();
    if healed.is_ok() {
        let rows: Vec<String> = ids.iter().map(|id| format!("#{id}")).collect();
        println!("  checkout caught up: {}", rows.join(", "));
    }
    Ok((
        copies(repo, Some(w), old, &q.name)?,
        copies(repo, Some(w), old, &a.name)?,
    ))
}

/// The nearest earlier trunk commit, within `CATCH_UP_DEPTH` of `old` along its
/// first parents, whose blobs at `names` are exactly `staged` (`None`: the
/// commit lacks the file): the commit and its blobs.
fn held_commit(
    repo: &Repo,
    old: &str,
    names: [&str; 2],
    staged: [&Option<String>; 2],
) -> Res<Option<(String, [Option<String>; 2])>> {
    let revs = git::opt(
        &repo.primary,
        &[
            "rev-list",
            "--first-parent",
            &format!("--max-count={CATCH_UP_DEPTH}"),
            &format!("{old}^"),
        ],
    )
    .unwrap_or_default();
    let query: String = revs
        .lines()
        .flat_map(|c| names.map(|n| format!("{c}:{n}\n")))
        .collect();
    if query.is_empty() {
        return Ok(None);
    }
    let o = git::raw(
        &repo.primary,
        &["cat-file", "--batch-check=%(objectname)"],
        &[],
        Some(&query),
    )?;
    let blob = |l: &str| (!l.ends_with(" missing")).then(|| l.to_string());
    let lines: Vec<&str> = o.stdout.lines().collect();
    Ok(lines
        .chunks(2)
        .zip(revs.lines())
        .find(|(p, _)| p.len() == 2 && &blob(p[0]) == staged[0] && &blob(p[1]) == staged[1])
        .map(|(p, c)| (c.to_string(), [blob(p[0]), blob(p[1])])))
}

/// Where the trunk checkout missed a trunk commit — its index entries are an
/// earlier trunk commit's queue files exactly — the fix that applies the trunk's
/// diff since to its working files and index. A read can refuse there before any
/// write gets to catch it up: after a repo's first archive, the checkout's queue
/// still holds the rows the trunk's new archive has.
pub fn missed_commit_fix(repo: &Repo) -> Option<String> {
    let w = repo.trunk_checkout().ok()??;
    let old = git::rev(&repo.primary, &format!("refs/heads/{}", repo.trunk))?;
    let names = [repo.cfg.file.as_str(), repo.cfg.archive.as_str()];
    let at = |rev: &str, n: &str| {
        git::opt(
            &repo.primary,
            &["rev-parse", "--verify", "--quiet", &format!("{rev}:{n}")],
        )
    };
    let staged = names.map(|n| {
        git::opt(&w, &["ls-files", "-s", "--", n])
            .and_then(|l| l.split_whitespace().nth(1).map(String::from))
    });
    if staged[0].is_none() || staged == names.map(|n| at(&old, n)) {
        return None;
    }
    let (seen, _) = held_commit(repo, &old, names, [&staged[0], &staged[1]]).ok()??;
    let g = format!("git -C {}", shell_word(&w.to_string_lossy()));
    let diff = format!(
        "{g} diff {}..{} -- {}",
        &seen[..seen.len().min(12)],
        &old[..old.len().min(12)],
        names.map(shell_word).join(" ")
    );
    Some(format!("{diff} | {g} apply && {diff} | {g} apply --cached"))
}

/// Set the checkout's index entries for `(name, blob)` in one `update-index`, so
/// the queue's and the archive's change together or neither does: a row moving
/// between them is in exactly one.
fn update_index(w: &Path, entries: &[(&str, &str)]) -> Res<()> {
    if entries.is_empty() {
        return Ok(());
    }
    let input: String = entries
        .iter()
        .map(|(name, blob)| format!("100644 {blob}\t{}\n", quote_path(name)))
        .collect();
    let o = git::raw(w, &["update-index", "--index-info"], &[], Some(&input))?;
    if !o.ok {
        bail!("git update-index --index-info: {}", o.stderr.trim());
    }
    Ok(())
}

/// A path as `--index-info` reads it: C-quoted where a raw one would misparse.
fn quote_path(name: &str) -> String {
    if !name.starts_with('"') && !name.contains(['\n', '\\']) {
        return name.to_string();
    }
    let mut q = String::from("\"");
    for ch in name.chars() {
        match ch {
            '"' => q.push_str("\\\""),
            '\\' => q.push_str("\\\\"),
            '\n' => q.push_str("\\n"),
            c => q.push(c),
        }
    }
    q.push('"');
    q
}

/// Which file of the two a row is in, its section, and its lines.
fn row(texts: [&str; 2], id: u64) -> Option<(usize, Option<String>, Vec<String>)> {
    texts.iter().enumerate().find_map(|(f, text)| {
        let d = Doc::new(text);
        let (at, len) = d.block(id)?;
        let t = queue::parse(text).into_iter().find(|t| t.id == id)?;
        Some((f, t.section, d.lines[at..at + len].to_vec()))
    })
}

/// Refuse where the trunk checkout's index holds the queue or its archive in
/// conflict stages: a write would collapse them into one entry and leave the
/// markers in the working file.
pub fn refuse_unmerged(repo: &Repo, checkout: &Path) -> Res<()> {
    for name in [&repo.cfg.file, &repo.cfg.archive] {
        if git::opt(checkout, &["ls-files", "-u", "--", name]).is_some_and(|l| !l.is_empty()) {
            bail!(
                "{name} has an unresolved conflict in {}; resolve it (git add) first",
                checkout.display()
            )
        }
    }
    Ok(())
}

/// Whether any checkout of the repository is part way through a merge, rebase,
/// cherry-pick or revert — read from the git dirs, so a clean repository costs
/// no git command to rule out unmerged queue stages.
pub fn mid_operation(repo: &Repo) -> bool {
    let mut dirs = vec![repo.common.clone()];
    if let Ok(rd) = fs::read_dir(repo.common.join("worktrees")) {
        dirs.extend(rd.flatten().map(|e| e.path()));
    }
    dirs.iter().any(|d| {
        [
            "MERGE_HEAD",
            "CHERRY_PICK_HEAD",
            "REVERT_HEAD",
            "AUTO_MERGE",
            "rebase-merge",
            "rebase-apply",
        ]
        .iter()
        .any(|f| d.join(f).exists())
    })
}

/// Refuse a queue write where the trunk tracks `name` as a symlink: a commit to
/// the link's path would turn it into a file, and one to the path it names would
/// let a retargeted link aim queue writes at any file in the repository.
fn refuse_link(repo: &Repo, checkout: Option<&Path>, old: &str, name: &str) -> Res<()> {
    let Some((_, blob)) = entry(repo, old, name).filter(|(m, _)| m == "120000") else {
        return Ok(());
    };
    let to = git::git(&repo.primary, &["cat-file", "blob", &blob])?;
    let at = normalize(&Path::new(name).parent().unwrap_or(Path::new("")).join(&to));
    let (git_c, name_p) = in_checkout(checkout, name);
    let name_w = shell_word(name);
    let trunk = &repo.trunk;
    if at.extension().is_none_or(|e| e != "md") {
        // Copying a source file over the queue would not restore it; the queue
        // file as it was before the commit that set this link does, and only it.
        // First parent: a merge that took the link from a side branch is where
        // the trunk's own queue ends, not the side commit, whose parent predates
        // rows the trunk added meanwhile.
        let by = git::opt(
            &repo.primary,
            &[
                "log",
                "-1",
                "--first-parent",
                "--format=%h",
                old,
                "--",
                name,
            ],
        )
        .unwrap_or_default();
        // `~1`, not `^`: a shell with extended globbing reads `^` itself.
        let before = format!("{by}~1");
        let restorable = !by.is_empty()
            && entry(repo, &before, name).is_some_and(|(m, _)| m != "120000" && m != "040000");
        if !restorable {
            bail!(
                "{name} is a symlink on {trunk} to {}, not a queue file, and no commit before it holds the file; replace the link with the queue file by hand and commit",
                at.display()
            )
        }
        bail!(
            "{name} is a symlink on {trunk} to {}, not a queue file; restore the file from before {by}, which set the link (`{git_c} checkout {before} -- {name_w}`), and commit",
            at.display()
        )
    }
    // A copy, not `git mv`: the commit then touches the queue file alone, as lint
    // asks of a queue edit; the file the link named can go in a commit of its own.
    let at_p = match checkout {
        Some(w) => shell_word(&w.join(&at).to_string_lossy()),
        None => shell_word(&at.to_string_lossy()),
    };
    let fix = format!("{git_c} rm -q {name_w} && cp {at_p} {name_p} && {git_c} add {name_w}");
    bail!(
        "{name} is a symlink on {trunk}; replace it with the file (`{fix}`) and commit — 5w edits the queue file itself"
    )
}

/// Refuse a queue write where the trunk checkout holds `name` as a symlink the
/// trunk does not track: writing through it would put the queue wherever the link
/// points, possibly outside the repository.
fn refuse_working_link(repo: &Repo, checkout: &Path, old: &str, name: &str) -> Res<()> {
    let path = checkout.join(name);
    let Ok(to) = fs::read_link(&path) else {
        return Ok(());
    };
    let (git_c, name_p) = in_checkout(Some(checkout), name);
    let fix = if git::ok(&repo.primary, &["cat-file", "-e", &format!("{old}:{name}")]) {
        format!(
            "rm {name_p} && {git_c} checkout {} -- {}",
            shell_word(&repo.trunk),
            shell_word(name)
        )
    } else {
        format!("rm {name_p}")
    };
    bail!(
        "{name} in {} is a symlink to {}; replace the link with the file (`{fix}`) — rows edited through it live there",
        checkout.display(),
        to.display()
    )
}

/// Run `edits` — ordinary queue commands, each calling `transact` — as one
/// batch: one lock for all of them, each guarded against the queue as the trunk
/// and the edits before it leave it, and one commit at the end. If `edits`
/// fails, nothing is committed or written. The subject names every edit,
/// `<prefix>: accept #4, reject #5`, and the body holds each edit's own message;
/// a batch of one commits under that edit's own message.
pub fn batch(repo: &Repo, edits: impl FnOnce() -> Res<()>) -> Res<()> {
    let (lock, old, checkout, q, a) = begin(repo)?;
    BATCH.with(|b| {
        *b.borrow_mut() = Some(Batch {
            _lock: lock,
            old,
            checkout,
            cur: [q.clone(), a.clone()],
            orig: [q, a],
            messages: Vec::new(),
            edited: HashSet::new(),
        })
    });
    let result = edits();
    let Some(b) = BATCH.with(|b| b.borrow_mut().take()) else {
        bail!("batch ended early")
    };
    result?;
    let message = match b.messages.as_slice() {
        [] if (0..2).all(|i| {
            (b.cur[i].working == b.orig[i].working) && (b.cur[i].staged == b.orig[i].staged)
        }) =>
        {
            println!("  nothing to commit: every edit leaves its row as it is");
            return Ok(());
        }
        [] => String::new(),
        [one] => one.clone(),
        many => {
            let head = format!("{}: ", repo.cfg.commit_prefix);
            let parts: Vec<String> = many
                .iter()
                .map(|m| {
                    let rest = m.strip_prefix(&head).unwrap_or(m);
                    rest.split(' ').take(2).collect::<Vec<_>>().join(" ")
                })
                .collect();
            format!("{head}{}\n\n{}", parts.join(", "), many.join("\n"))
        }
    };
    let [q, a] = &b.orig;
    let [cq, ca] = &b.cur;
    let plan = Plan {
        message,
        new_q: cq.committed.clone(),
        new_a: ca.committed.clone(),
        working: q.working.is_some().then(|| {
            (
                cq.working.clone().unwrap_or_default(),
                ca.working.clone().unwrap_or_else(|| ca.committed.clone()),
            )
        }),
        staged: (q.staged.is_some() || a.staged.is_some()).then(|| {
            (
                cq.staged.clone().unwrap_or_else(|| cq.committed.clone()),
                ca.staged.clone().unwrap_or_else(|| ca.committed.clone()),
            )
        }),
    };
    write(repo, &b.old, b.checkout.as_deref(), q, a, &plan)
}

/// Apply `op` to the queue (and its archive) and commit the result to the trunk
/// as one commit that holds nothing else — or, inside `batch`, add it to the
/// batch's commit.
///
/// `guard` sees the task as the *committed* queue has it, under the lock — state
/// checks made against a working copy outside the lock can be stale or hand-made.
/// `ids` are the tasks the op touches; the first is the one guarded.
///
/// Three copies of each file move together, all computed before anything is
/// written: the committed file; the trunk checkout's staged entry, which gets the
/// same edit on top of whatever is staged there (left as it was, a staged edit
/// would sit against the new HEAD as a revert of this commit); and that
/// checkout's working file.
pub fn transact(
    repo: &Repo,
    message: impl Fn(&Ctx) -> String,
    ids: &[u64],
    guard: impl Fn(Option<&queue::Task>) -> Res<()>,
    op: impl Fn(&mut Files, &Ctx) -> Res<()>,
) -> Res<()> {
    if batching() {
        let [q, a] = BATCH
            .with(|b| b.borrow().as_ref().map(|b| b.cur.clone()))
            .ok_or("batch ended early")?;
        let plan = plan(repo, &q, &a, message, ids, guard, op)?;
        // Lint judges a commit row by row, before against after: a second edit
        // of one row would reach it as one transition, which may be no legal one.
        let changed = rows_changed([&q.committed, &a.committed], [&plan.new_q, &plan.new_a]);
        let no_op = no_op(&q, &a, &plan);
        BATCH.with(|b| {
            let mut b = b.borrow_mut();
            let Some(b) = b.as_mut() else {
                bail!("batch ended early")
            };
            if let Some(id) = changed.iter().find(|id| b.edited.contains(id)) {
                bail!("#{id} is already edited in this batch; edit it again in the next one");
            }
            // An edit that changes nothing is not one: the subject names only rows it changes.
            if no_op {
                return Ok(());
            }
            {
                let [cq, ca] = &mut b.cur;
                // One that changes only the checkout's copies is carried there, unnamed.
                if !changed.is_empty() {
                    cq.committed = plan.new_q;
                    ca.committed = plan.new_a;
                    b.messages.push(plan.message);
                }
                if let Some((wq, wa)) = plan.working {
                    cq.working = Some(wq);
                    ca.working = ca.working.is_some().then_some(wa);
                }
                if let Some((sq, sa)) = plan.staged {
                    cq.staged = cq.staged.is_some().then_some(sq);
                    ca.staged = ca.staged.is_some().then_some(sa);
                }
            }
            b.edited.extend(changed);
            Ok(())
        })?;
        return Ok(());
    }
    let (_lock, old, checkout, q, a) = begin(repo)?;
    let plan = plan(repo, &q, &a, message, ids, guard, op)?;
    // An edit of a named row that leaves every row as it was (`set` to the value
    // a row has) commits nothing: its subject would name a row it does not change.
    if let Some(id) = ids.first()
        && no_op(&q, &a, &plan)
    {
        println!("  nothing to commit: #{id} is already so");
        return Ok(());
    }
    write(repo, &old, checkout.as_deref(), &q, &a, &plan)
}

/// The rows an edit changes or moves to another section between two copies of
/// the queue and its archive, as lint reads a batch: a line rewritten to say
/// the same is no change.
fn rows_changed(old: [&str; 2], new: [&str; 2]) -> Vec<u64> {
    let (old, new) = (queue::parse_all(old), queue::parse_all(new));
    let (old, new) = (queue::by_id(&old), queue::by_id(&new));
    let mut ids = queue::changed_ids(&old, &new);
    ids.extend(queue::moved_ids(&old, &new));
    ids.into_iter().collect()
}

/// Whether a plan leaves every copy — committed, working and staged — reading
/// as it did: no row changed or moved.
fn no_op(q: &Copies, a: &Copies, plan: &Plan) -> bool {
    let same = |old: [&str; 2], new: [&str; 2]| rows_changed(old, new).is_empty();
    same([&q.committed, &a.committed], [&plan.new_q, &plan.new_a])
        && plan.working.as_ref().is_none_or(|(wq, wa)| {
            same(
                [
                    q.working.as_deref().unwrap_or_default(),
                    a.working.as_deref().unwrap_or(&a.committed),
                ],
                [wq, wa],
            )
        })
        && plan.staged.as_ref().is_none_or(|(sq, sa)| {
            same(
                [
                    q.staged.as_deref().unwrap_or(&q.committed),
                    a.staged.as_deref().unwrap_or(&a.committed),
                ],
                [sq, sa],
            )
        })
}

/// Refuse to pull a named row from the working copy into a commit when its id is
/// not above every id committed before that commit: minting skips uncommitted
/// rows, so a later id may be committed already, and lint would call this one reused.
fn refuse_reuse(repo: &Repo, q: &Copies, a: &Copies, ids: &[u64], next_id: u64) -> Res<()> {
    let Some(w) = q.working.as_deref() else {
        return Ok(());
    };
    // Lint judges the commit against the trunk as it was before any edit of a batch.
    let max = |q: &Copies, a: &Copies| queue::max_id(&q.committed).max(queue::max_id(&a.committed));
    let floor = BATCH
        .with(|b| b.borrow().as_ref().map(|b| max(&b.orig[0], &b.orig[1])))
        .unwrap_or_else(|| max(q, a));
    let (queue_doc, archive_doc, working_doc) =
        (Doc::new(&q.committed), Doc::new(&a.committed), Doc::new(w));
    for &id in ids {
        if id <= floor
            && queue_doc.block(id).is_none()
            && archive_doc.block(id).is_none()
            && working_doc.block(id).is_some()
        {
            bail!(
                "#{id} is uncommitted in {} and below #{floor} on {}, so committing it would reuse an id; renumber it #{next_id} there and retry",
                repo.cfg.file,
                repo.trunk
            );
        }
    }
    Ok(())
}

/// An edit of a row the trunk has archived, which the checkout's queue still
/// holds: carried onto the trunk, the row would sit in both files.
fn refuse_archived(repo: &Repo, q: &Copies, a: &Copies, ids: &[u64]) -> Res<()> {
    let (queue_doc, archive_doc) = (Doc::new(&q.committed), Doc::new(&a.committed));
    match ids
        .iter()
        .find(|&&id| queue_doc.block(id).is_none() && archive_doc.block(id).is_some())
    {
        Some(&id) => Err(archived(repo, id)),
        None => Ok(()),
    }
}

/// The refusal for an edit of an archived row: only a hand edit brings it back.
pub fn archived(repo: &Repo, id: u64) -> String {
    format!(
        "#{id} is archived; move its block from {} back to {} by hand",
        repo.cfg.archive, repo.cfg.file
    )
}

/// An edit worked out on every copy, before anything is written.
struct Plan {
    message: String,
    new_q: String,
    new_a: String,
    working: Option<(String, String)>,
    staged: Option<(String, String)>,
}

fn plan(
    repo: &Repo,
    q: &Copies,
    a: &Copies,
    message: impl Fn(&Ctx) -> String,
    ids: &[u64],
    guard: impl Fn(Option<&queue::Task>) -> Res<()>,
    op: impl Fn(&mut Files, &Ctx) -> Res<()>,
) -> Res<Plan> {
    let texts = [&q.committed, &a.committed].into_iter().chain(
        [&q.working, &q.staged, &a.working, &a.staged]
            .into_iter()
            .flatten(),
    );
    let ctx = Ctx {
        next_id: texts.map(|t| queue::max_id(t)).max().unwrap_or(0) + 1,
    };
    refuse_reuse(repo, q, a, ids, ctx.next_id)?;
    refuse_archived(repo, q, a, ids)?;

    let carried = carry(repo, &q.committed, q.working.as_deref(), ids);
    if let Some(&id) = ids.first() {
        let text = carried.text();
        let tasks = queue::parse(&text);
        guard(tasks.iter().find(|t| t.id == id))?;
    }

    // Committed.
    let mut fc = Files {
        queue: carried,
        archive: Doc::new(&a.committed),
    };
    op(&mut fc, &ctx)?;
    let (new_q, new_a) = (fc.queue.text(), fc.archive.text());

    // Working, when the checkout has the queue file at all.
    let working = match &q.working {
        Some(w) => {
            let mut fw = Files {
                queue: carry(repo, w, None, &[]),
                archive: Doc::new(a.working.as_deref().unwrap_or(&a.committed)),
            };
            op(&mut fw, &ctx).map_err(|e| {
                format!(
                    "{} working copy disagrees with {} ({e}); nothing written",
                    repo.cfg.file, repo.trunk
                )
            })?;
            Some((fw.queue.text(), fw.archive.text()))
        }
        None => None,
    };

    // Staged, only where something staged differs from the commit.
    let staged = if q.staged.is_some() || a.staged.is_some() {
        let mut fs_ = Files {
            queue: carry(
                repo,
                q.staged.as_deref().unwrap_or(&q.committed),
                q.working.as_deref(),
                ids,
            ),
            archive: Doc::new(a.staged.as_deref().unwrap_or(&a.committed)),
        };
        op(&mut fs_, &ctx).map_err(|e| {
            format!(
                "staged {} cannot take this change ({e}); commit or unstage it",
                repo.cfg.file
            )
        })?;
        let sa = fs_.archive.text();
        // An archive file on neither the trunk nor the index is staged new with
        // the staged copy's rows (see `mirror_index`) — never over an untracked
        // one with content beyond the header, which that would leave half-tracked.
        if a.old_blob.is_none()
            && a.staged_blob.is_none()
            && !sa.is_empty()
            && sa != new_a
            && a.working.as_deref().is_some_and(|w| {
                !w.trim().is_empty() && w.trim() != crate::tasks::ARCHIVE_HEADER.trim()
            })
        {
            bail!(
                "untracked {} has content of its own; `git add -f {} && git add -p {}`, or move it aside, then retry",
                repo.cfg.archive,
                repo.cfg.archive,
                repo.cfg.file
            );
        }
        Some((fs_.queue.text(), sa))
    } else {
        None
    };
    Ok(Plan {
        message: message(&ctx),
        new_q,
        new_a,
        working,
        staged,
    })
}

/// Commit a plan onto `old` with a compare-and-swap, then carry it into the
/// trunk checkout's index entry and working file.
fn write(
    repo: &Repo,
    old: &str,
    checkout: Option<&Path>,
    q: &Copies,
    a: &Copies,
    plan: &Plan,
) -> Res<()> {
    let trunk_ref = format!("refs/heads/{}", repo.trunk);
    let message = &plan.message;
    let subject = message.lines().next().unwrap_or_default();
    let (new_q, new_a, working, staged) = (&plan.new_q, &plan.new_a, &plan.working, &plan.staged);
    let changes: Vec<Change> = [
        (
            q,
            new_q,
            working.as_ref().map(|w| &w.0),
            staged.as_ref().map(|s| &s.0),
        ),
        (
            a,
            new_a,
            working.as_ref().map(|w| &w.1),
            staged.as_ref().map(|s| &s.1),
        ),
    ]
    .into_iter()
    .collect();
    // Every working file is written beside its target before anything moves, so
    // a file that cannot be written refuses with the trunk and both files as
    // they were.
    let pending = match checkout {
        Some(w) => Pending::prepare(&repo.common, w, &changes)?,
        None => Pending::default(),
    };

    let changed = changes
        .iter()
        .any(|(c, new, _, _)| *new != &c.committed && !(c.old_blob.is_none() && new.is_empty()));
    if changed {
        // Under the queue lock, a private index already there is one a killed
        // run (say, at the pinentry during commit-tree) left behind: it goes.
        remove_stale(&repo.common, INDEX, "", 1);
        let index = repo.common.join(format!("{INDEX}{}", std::process::id()));
        let idx = index.to_string_lossy().into_owned();
        let env = [("GIT_INDEX_FILE", idx.as_str())];
        let run = |args: &[&str]| -> Res<String> {
            let o = git::raw(&repo.primary, args, &env, None)?;
            if !o.ok {
                bail!("git {}: {}", args.join(" "), o.stderr.trim());
            }
            Ok(o.stdout.trim().to_string())
        };
        let mut blobs = Vec::new();
        let result = (|| -> Res<String> {
            run(&["read-tree", old])?;
            for (c, new, _, _) in &changes {
                if c.old_blob.is_none() && new.is_empty() {
                    blobs.push(None);
                    continue;
                }
                let b = hash_blob(repo, new)?;
                run(&[
                    "update-index",
                    "--add",
                    "--cacheinfo",
                    &format!("100644,{b},{}", c.name),
                ])?;
                blobs.push(Some(b));
            }
            let tree = run(&["write-tree"])?;
            git::commit_tree(&repo.primary, &tree, old, message, &[])
        })();
        let _ = fs::remove_file(&index);
        let commit = result?;
        git::git(
            &repo.primary,
            &["update-ref", "-m", subject, &trunk_ref, &commit, old],
        )
        .map_err(|e| {
            format!(
                "{} moved while this ran; nothing written, retry ({e})",
                repo.trunk
            )
        })?;

        println!("  committed: {subject}");
        if let Some(w) = checkout {
            // The trunk has moved: a failure from here on says so, and the fix
            // applies the commit's own diff, keeping anything else in the checkout.
            // Per file, so a row moving between them counts.
            let mut ids = rows_changed([&q.committed, ""], [new_q, ""]);
            ids.extend(rows_changed(["", &a.committed], ["", new_a]));
            ids.sort_unstable();
            ids.dedup();
            let rows: Vec<String> = ids.iter().map(|id| format!("#{id}")).collect();
            let landed = |e: String, names: &[&str], index: bool| {
                let g = format!("git -C {}", shell_word(&w.to_string_lossy()));
                let diff = format!(
                    "{g} diff {}..{} -- {}",
                    &old[..old.len().min(12)],
                    &commit[..commit.len().min(12)],
                    names.join(" ")
                );
                let mut fix = format!("{diff} | {g} apply");
                if index {
                    fix.push_str(&format!(" && {diff} | {g} apply --cached"));
                }
                let what = match rows.is_empty() {
                    true => changes
                        .iter()
                        .map(|(c, ..)| c.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", "),
                    false => rows.join(", "),
                };
                format!(
                    "committed {what} to {}, but the checkout was not updated ({e}); `{fix}` (add --3way to each apply where a hand edit is in the way)",
                    repo.trunk
                )
            };
            let names: Vec<&str> = changes
                .iter()
                .zip(&blobs)
                .filter(|((c, ..), b)| **b != c.old_blob)
                .map(|((c, ..), _)| c.name.as_str())
                .collect();
            mirror_index(repo, w, &changes, &blobs).map_err(|e| landed(e, &names, true))?;
            return pending.finish().map_err(|(e, left)| {
                let left: Vec<&str> = left
                    .iter()
                    .map(String::as_str)
                    .filter(|n| names.contains(n))
                    .collect();
                landed(e, &left, false)
            });
        }
    } else if let Some(w) = checkout {
        // Nothing to commit, but the checkout's staged copy still takes the
        // edit: left as it was, the next ordinary commit would commit it.
        let blobs: Vec<Option<String>> = changes.iter().map(|(c, ..)| c.old_blob.clone()).collect();
        let restaged = mirror_index(repo, w, &changes, &blobs)?;
        let rewritten = changes.iter().any(|(c, _, nw, _)| {
            nw.is_some_and(|nw| {
                Some(nw) != c.working.as_ref() && !(c.working.is_none() && nw.is_empty())
            })
        });
        if restaged || rewritten {
            let mut ids = Vec::new();
            let before = [(&q.working, &a.working), (&q.staged, &a.staged)];
            for (copy, (bq, ba)) in [&plan.working, &plan.staged].into_iter().zip(before) {
                let Some((nq, na)) = copy else { continue };
                let (bq, ba) = (
                    bq.as_ref().unwrap_or(&q.committed),
                    ba.as_ref().unwrap_or(&a.committed),
                );
                ids.extend(rows_changed([bq, ba], [nq, na]));
            }
            ids.sort_unstable();
            ids.dedup();
            // Only claim the checkout matches the trunk where its rows do.
            let matches = [&plan.working, &plan.staged]
                .into_iter()
                .flatten()
                .all(|(nq, na)| {
                    !rows_changed([new_q, new_a], [nq, na])
                        .iter()
                        .any(|id| ids.is_empty() || ids.contains(id))
                });
            let rows: Vec<String> = ids.iter().map(|id| format!("#{id}")).collect();
            let what = match rows.as_slice() {
                [] => q.name.clone(),
                _ => rows.join(", "),
            };
            match (matches, rows.len()) {
                (true, 2..) => println!("  checkout fixed: {what} match {}", repo.trunk),
                (true, _) => println!("  checkout fixed: {what} matches {}", repo.trunk),
                (false, _) => println!("  checkout updated: {what}"),
            }
        }
    } else {
        return Ok(());
    }
    if checkout.is_some() {
        pending.finish().map_err(|(e, _)| format!("{e}; retry"))?;
    } else {
        eprintln!(
            "  (no worktree has {} checked out; committed to the ref)",
            repo.trunk
        );
    }
    Ok(())
}

/// Working files whose new text is written aside before anything moves, and
/// renamed into place only once the trunk has taken the change; dropped
/// unfinished, it removes its temporary files and any directory it created.
#[derive(Default)]
struct Pending {
    /// In the order the files were given.
    files: Vec<Tmp>,
    dirs: Vec<PathBuf>,
    done: bool,
}

struct Tmp {
    /// Where the new text is written.
    at: PathBuf,
    /// A name beside the target, for when `at` is elsewhere and cannot be
    /// renamed across.
    beside: PathBuf,
    target: PathBuf,
    /// The target's file name.
    file: String,
    name: String,
}

/// Temporary files are named `5w-write-<pid>-<n>-<file>` in the git dir when it
/// shares the target's filesystem (a rename there is atomic, and nothing in the
/// checkout can sweep one into a commit), else `.<file>.5w-write-<pid>-<n>`
/// beside the target.
const TMP: &str = "5w-write-";

/// The private index a queue commit is built in: `5w-index-<pid>` in the git dir.
const INDEX: &str = "5w-index-";

impl Pending {
    /// Called under the queue lock, so a temporary file already there is one a
    /// killed run left behind: it goes first.
    fn prepare(common: &Path, w: &Path, changes: &[Change]) -> Res<Pending> {
        let mut p = Pending::default();
        for (c, _, new_working, _) in changes {
            let Some(nw) = new_working else { continue };
            if Some(*nw) == c.working.as_ref() || (c.working.is_none() && nw.is_empty()) {
                continue;
            }
            let fail = |e: &dyn std::fmt::Display| {
                format!(
                    "cannot write {} ({e}); move what is in its way aside, then retry; nothing written",
                    c.name
                )
            };
            let target = resolve(&w.join(&c.name));
            if target.is_dir() {
                bail!("{}", fail(&"a directory is in its place"));
            }
            let (Some(dir), Some(file)) = (target.parent(), target.file_name()) else {
                bail!("{}", fail(&"not a file path"));
            };
            // The archive may name a directory the checkout lacks: the commit
            // holds it, so the checkout gets it too.
            let mut d = dir;
            while !d.exists() {
                p.dirs.push(d.to_path_buf());
                match d.parent() {
                    Some(up) => d = up,
                    None => break,
                }
            }
            fs::create_dir_all(dir).map_err(|e| fail(&e))?;
            let file = file.to_string_lossy().into_owned();
            let id = format!("{}-{}", std::process::id(), p.files.len());
            let beside = dir.join(format!(".{file}.{TMP}{id}"));
            let at = if same_fs(common, dir) {
                remove_stale(common, TMP, &format!("-{file}"), 2);
                common.join(format!("{TMP}{id}-{file}"))
            } else {
                remove_stale(dir, &format!(".{file}.{TMP}"), "", 2);
                beside.clone()
            };
            let mode = fs::metadata(&target).ok().map(|m| m.permissions());
            p.files.push(Tmp {
                at: at.clone(),
                beside,
                target,
                file,
                name: c.name.clone(),
            });
            fs::write(&at, nw).map_err(|e| fail(&e))?;
            if let Some(mode) = mode {
                fs::set_permissions(&at, mode).map_err(|e| fail(&e))?;
            }
        }
        Ok(p)
    }

    /// Renames every file into place, the archive (which gains rows) before the
    /// queue (which loses them): a failure between leaves a moved row in both
    /// working files, never in neither. On failure, the error and the names not
    /// yet in place.
    fn finish(mut self) -> Result<(), (String, Vec<String>)> {
        self.done = true;
        let files = std::mem::take(&mut self.files);
        let mut left = files.iter().rev();
        while let Some(t) = left.next() {
            if let Err(e) = t.place() {
                let names = std::iter::once(&t.name)
                    .chain(left.clone().map(|t| &t.name))
                    .cloned()
                    .collect();
                for t in left {
                    let _ = fs::remove_file(&t.at);
                }
                return Err((format!("cannot write {}: {e}", t.name), names));
            }
        }
        Ok(())
    }
}

impl Tmp {
    fn place(&self) -> std::io::Result<()> {
        let r = match rename(&self.at, &self.target) {
            // One filesystem bind-mounted twice shares a device id yet refuses
            // the rename: copy beside the target and rename that.
            Err(e) if e.kind() == std::io::ErrorKind::CrossesDevices && self.at != self.beside => {
                if let Some(dir) = self.beside.parent() {
                    remove_stale(dir, &format!(".{}.{TMP}", self.file), "", 2);
                }
                let r = fs::copy(&self.at, &self.beside)
                    .and_then(|_| rename(&self.beside, &self.target));
                if r.is_err() {
                    let _ = fs::remove_file(&self.beside);
                }
                r
            }
            r => r,
        };
        let _ = fs::remove_file(&self.at);
        r
    }
}

/// `fs::rename`; FIVEW_TEST_EXDEV (test-only) makes a rename between two
/// directories fail as one across filesystems does.
fn rename(from: &Path, to: &Path) -> std::io::Result<()> {
    if std::env::var_os("FIVEW_TEST_EXDEV").is_some() && from.parent() != to.parent() {
        return Err(std::io::ErrorKind::CrossesDevices.into());
    }
    fs::rename(from, to)
}

impl Drop for Pending {
    fn drop(&mut self) {
        for t in &self.files {
            let _ = fs::remove_file(&t.at);
        }
        if !self.done {
            // Deepest first; `remove_dir` leaves any that is not empty.
            self.dirs
                .sort_by_key(|d| std::cmp::Reverse(d.components().count()));
            for d in &self.dirs {
                let _ = fs::remove_dir(d);
            }
        }
    }
}

/// Where a write to `path` lands: through a symlink, dangling or not, to the
/// file it names, as a plain write would go.
fn resolve(path: &Path) -> PathBuf {
    let mut p = path.to_path_buf();
    for _ in 0..40 {
        match fs::read_link(&p) {
            Ok(to) => p = p.parent().map_or(to.clone(), |d| d.join(&to)),
            Err(_) => break,
        }
    }
    p
}

/// Remove the temporary files a killed run left in `dir`: exactly
/// `<prefix><pid>-<n><suffix>` (`parts` 2) or `<prefix><pid><suffix>` (`parts`
/// 1), all digits, so a file of the user's that merely looks alike stays.
fn remove_stale(dir: &Path, prefix: &str, suffix: &str, parts: usize) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let stale = name
            .strip_prefix(prefix)
            .and_then(|n| n.strip_suffix(suffix))
            .is_some_and(|n| {
                let fields: Vec<&str> = n.split('-').collect();
                fields.len() == parts && fields.iter().all(|f| digits(f))
            });
        if stale {
            let _ = fs::remove_file(e.path());
        }
    }
}

#[cfg(unix)]
fn same_fs(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    matches!((fs::metadata(a), fs::metadata(b)), (Ok(a), Ok(b)) if a.dev() == b.dev())
}

#[cfg(not(unix))]
fn same_fs(_: &Path, _: &Path) -> bool {
    false
}

/// A path as one shell word.
fn shell_word(s: &str) -> String {
    if s.chars()
        .all(|c| c.is_ascii_alphanumeric() || "/._-+:@,".contains(c))
    {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

type Change<'a> = (
    &'a Copies,
    &'a String,
    Option<&'a String>,
    Option<&'a String>,
);

/// Carry a plan into the trunk checkout's index: each file's entry follows its
/// new committed blob — or, where something else was staged, becomes that staged
/// text with the edit applied. Only entries that differ are touched; returns
/// whether any was.
fn mirror_index(repo: &Repo, w: &Path, changes: &[Change], blobs: &[Option<String>]) -> Res<bool> {
    let mut entries = Vec::new();
    for ((c, _, _, new_staged), blob) in changes.iter().zip(blobs) {
        // A planned staged copy is staged whole, even where the entry matched the
        // commit: a row moving between the files moves in both entries or neither.
        // A file the trunk lacks is staged new once the plan gives it rows.
        let entry = match (new_staged, blob) {
            (Some(ns), _)
                if c.staged.is_some()
                    || c.staged_blob.is_some()
                    || (c.old_blob.is_none() && !ns.is_empty()) =>
            {
                hash_blob(repo, ns)?
            }
            (_, Some(b)) if c.staged_blob.is_some() || c.old_blob.is_none() => b.clone(),
            _ => continue,
        };
        if c.staged_blob.as_ref() == Some(&entry) {
            continue;
        }
        entries.push((c.name.as_str(), entry));
    }
    let entries: Vec<(&str, &str)> = entries.iter().map(|(n, b)| (*n, b.as_str())).collect();
    update_index(w, &entries)?;
    Ok(!entries.is_empty())
}

fn hash_blob(repo: &Repo, content: &str) -> Res<String> {
    let o = git::raw(
        &repo.primary,
        &["hash-object", "-w", "--stdin"],
        &[],
        Some(content),
    )?;
    if !o.ok {
        bail!("git hash-object: {}", o.stderr.trim());
    }
    Ok(o.stdout.trim().to_string())
}

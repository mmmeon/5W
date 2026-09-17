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
use std::collections::{HashMap, HashSet};
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
    Ok((lock, old, checkout, q, a))
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
        [] => return Ok(()),
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
        BATCH.with(|b| {
            let mut b = b.borrow_mut();
            let Some(b) = b.as_mut() else {
                bail!("batch ended early")
            };
            if let Some(id) = changed.iter().find(|id| b.edited.contains(id)) {
                bail!("#{id} is already edited in this batch; edit it again in the next one");
            }
            // An edit that changes nothing is not one: the subject names only rows it changes.
            if changed.is_empty() {
                return Ok(());
            }
            b.edited.extend(changed);
            {
                let [cq, ca] = &mut b.cur;
                cq.committed = plan.new_q;
                ca.committed = plan.new_a;
                if let Some((wq, wa)) = plan.working {
                    cq.working = Some(wq);
                    ca.working = ca.working.is_some().then_some(wa);
                }
                if let Some((sq, sa)) = plan.staged {
                    cq.staged = cq.staged.is_some().then_some(sq);
                    ca.staged = ca.staged.is_some().then_some(sa);
                }
                b.messages.push(plan.message);
            }
            Ok(())
        })?;
        return Ok(());
    }
    let (_lock, old, checkout, q, a) = begin(repo)?;
    let plan = plan(repo, &q, &a, message, ids, guard, op)?;
    write(repo, &old, checkout.as_deref(), &q, &a, &plan)
}

/// The rows whose lines differ between two copies of the queue and its archive.
fn rows_changed(old: [&str; 2], new: [&str; 2]) -> Vec<u64> {
    let rows = |texts: [&str; 2]| {
        let mut m: HashMap<u64, Vec<String>> = HashMap::new();
        for text in texts {
            let d = Doc::new(text);
            for t in queue::parse(text) {
                if let Some((at, len)) = d.block(t.id) {
                    m.insert(t.id, d.lines[at..at + len].to_vec());
                }
            }
        }
        m
    };
    let (old, new) = (rows(old), rows(new));
    let mut ids: Vec<u64> = new
        .iter()
        .filter(|(id, lines)| old.get(id) != Some(lines))
        .map(|(id, _)| *id)
        .chain(old.keys().filter(|id| !new.contains_key(id)).copied())
        .collect();
    ids.sort_unstable();
    ids
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
        Some((fs_.queue.text(), fs_.archive.text()))
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
    let changes: Vec<(&Copies, &String, Option<&String>, Option<&String>)> = [
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

    let changed = changes
        .iter()
        .any(|(c, new, _, _)| *new != &c.committed && !(c.old_blob.is_none() && new.is_empty()));
    if changed {
        let index = repo.common.join(format!("5w-index-{}", std::process::id()));
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

        if let Some(w) = checkout {
            for ((c, _, _, new_staged), blob) in changes.iter().zip(&blobs) {
                let Some(blob) = blob else { continue };
                // The index entry follows the commit — or, where something else was
                // staged, becomes that staged text with this edit applied.
                let entry = match (&c.staged, new_staged) {
                    (Some(_), Some(ns)) => hash_blob(repo, ns)?,
                    _ if c.staged_blob.is_some() || c.old_blob.is_none() => blob.clone(),
                    _ => continue,
                };
                git::git(
                    w,
                    &[
                        "update-index",
                        "--add",
                        "--cacheinfo",
                        &format!("100644,{entry},{}", c.name),
                    ],
                )?;
            }
        }
        println!("  committed: {subject}");
    } else if checkout.is_none() {
        return Ok(());
    }
    if let Some(w) = checkout {
        for (c, new, new_working, _) in &changes {
            if let Some(nw) = new_working
                && Some(*nw) != c.working.as_ref()
                && !(c.working.is_none() && nw.is_empty())
            {
                let _ = new;
                fs::write(w.join(&c.name), nw)
                    .map_err(|e| format!("cannot write {}: {e}", c.name))?;
            }
        }
    } else {
        eprintln!(
            "  (no worktree has {} checked out; committed to the ref)",
            repo.trunk
        );
    }
    Ok(())
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

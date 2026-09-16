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

impl Repo {
    pub fn open() -> Res<Repo> {
        let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
        let common = git::git(
            &cwd,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        )
        .map_err(|_| "not inside a git repository".to_string())?;
        let common = PathBuf::from(common);
        let primary = common
            .parent()
            .ok_or("git common dir has no parent")?
            .to_path_buf();
        // The config belongs to the trunk: read it from the trunk's checkout,
        // else the trunk's commit, else the primary (before `init` commits it).
        // The trunk's own name is needed to find it, so resolve that first from
        // git config, and let the config override it.
        let guess = std::env::var("FIVEW_TRUNK")
            .ok()
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

    pub fn committed(&self) -> Res<Option<String>> {
        let spec = format!("refs/heads/{}:{}", self.trunk, self.cfg.file);
        let o = git::raw(&self.primary, &["show", &spec], &[], None)?;
        Ok(o.ok.then_some(o.stdout))
    }

    /// What reads see: the trunk checkout's working copy when there is one (so a
    /// just-added row is visible), otherwise the trunk's committed copy.
    pub fn load(&self) -> Res<String> {
        if let Some(w) = self.trunk_checkout()?
            && let Ok(s) = fs::read_to_string(w.join(&self.cfg.file))
        {
            return Ok(s);
        }
        match self.committed()? {
            Some(s) => Ok(s),
            None => bail!(
                "no {} on {} — run `{} init` to create one",
                self.cfg.file,
                self.trunk,
                self.cfg.cmd_tasks
            ),
        }
    }

    /// Worktree root for new branches.
    pub fn wt_root(&self) -> PathBuf {
        if let Some(r) = std::env::var_os("FIVEW_WT_ROOT") {
            return PathBuf::from(r);
        }
        match &self.cfg.wt_root {
            Some(r) if Path::new(r).is_absolute() => PathBuf::from(r),
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
    /// The next free id, computed under the lock from every copy.
    pub next_id: u64,
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

/// Apply `op` to one copy of the queue. A task the op touches that this copy
/// lacks but `donor` has (a row someone added and never committed) is carried in
/// first, so the op does not fail on it.
fn apply(
    repo: &Repo,
    base: &str,
    donor: Option<&str>,
    ids: &[u64],
    ctx: &Ctx,
    op: &impl Fn(&mut Doc, &Ctx) -> Res<()>,
) -> Res<String> {
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
    op(&mut d, ctx)?;
    Ok(d.text())
}

/// Apply `op` to the queue and commit it to the trunk as one commit that holds
/// nothing else.
///
/// `guard` sees the task as the *committed* queue has it, under the lock — state
/// checks made against a working copy outside the lock can be stale or hand-made.
/// `ids` are the tasks the op touches.
///
/// Three copies move together, all computed before anything is written:
/// the committed file; the trunk checkout's staged entry, which gets the same
/// edit on top of whatever is staged there (left as it was, a staged edit would
/// sit against the new HEAD as a revert of this commit); and that checkout's
/// working file.
pub fn transact(
    repo: &Repo,
    message: impl Fn(&Ctx) -> String,
    ids: &[u64],
    guard: impl Fn(Option<&queue::Task>) -> Res<()>,
    op: impl Fn(&mut Doc, &Ctx) -> Res<()>,
) -> Res<()> {
    let _lock = lock(repo)?;
    let file = repo.cfg.file.as_str();
    let trunk_ref = format!("refs/heads/{}", repo.trunk);
    let Some(old) = git::rev(&repo.primary, &trunk_ref) else {
        bail!("no trunk branch {}", repo.trunk)
    };
    let Some(committed) = repo.committed()? else {
        bail!(
            "{file} is not committed on {} — run `{} init`",
            repo.trunk,
            repo.cfg.cmd_tasks
        )
    };
    let checkout = repo.trunk_checkout()?;
    let working = checkout
        .as_ref()
        .and_then(|w| fs::read_to_string(w.join(file)).ok());
    let old_blob = git::opt(&repo.primary, &["rev-parse", &format!("{old}:{file}")]);
    let staged_blob = checkout.as_ref().and_then(|w| {
        git::opt(w, &["ls-files", "-s", "--", file])
            .and_then(|l| l.split_whitespace().nth(1).map(String::from))
    });
    let staged = match (&staged_blob, &old_blob) {
        (Some(s), Some(o)) if s != o => Some(git::git(&repo.primary, &["cat-file", "blob", s])?),
        _ => None,
    };

    let ctx = Ctx {
        next_id: [Some(&committed), working.as_ref(), staged.as_ref()]
            .into_iter()
            .flatten()
            .map(|t| queue::max_id(t))
            .max()
            .unwrap_or(0)
            + 1,
    };

    let carried = apply(repo, &committed, working.as_deref(), ids, &ctx, &|_, _| {
        Ok(())
    })?;
    if let Some(&id) = ids.first() {
        let tasks = queue::parse(&carried);
        guard(tasks.iter().find(|t| t.id == id))?;
    }
    let new_committed = apply(repo, &committed, working.as_deref(), ids, &ctx, &op)?;
    let new_working = match &working {
        Some(w) => Some(apply(repo, w, None, ids, &ctx, &op).map_err(|e| {
            format!(
                "the {file} working copy disagrees with {} ({e}) — nothing was written",
                repo.trunk
            )
        })?),
        None => None,
    };
    let new_staged = match &staged {
        Some(s) => Some(apply(repo, s, working.as_deref(), ids, &ctx, &op).map_err(|e| {
            format!("the staged {file} cannot take this change ({e}) — commit or unstage it; nothing was written")
        })?),
        None => None,
    };

    let message = message(&ctx);
    if new_committed != committed {
        // Commit with a private index, so no checkout's own index is read.
        let blob = hash_blob(repo, &new_committed)?;
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
        let result = (|| -> Res<String> {
            run(&["read-tree", &old])?;
            run(&[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("100644,{blob},{file}"),
            ])?;
            let tree = run(&["write-tree"])?;
            let o = git::raw(
                &repo.primary,
                &["commit-tree", &tree, "-p", &old, "-F", "-"],
                &[],
                Some(&format!("{message}\n")),
            )?;
            if !o.ok {
                bail!("git commit-tree: {}", o.stderr.trim());
            }
            Ok(o.stdout.trim().to_string())
        })();
        let _ = fs::remove_file(&index);
        let commit = result?;
        git::git(
            &repo.primary,
            &["update-ref", "-m", &message, &trunk_ref, &commit, &old],
        )
        .map_err(|e| {
            format!(
                "{} moved while this ran; nothing was written. Retry. ({e})",
                repo.trunk
            )
        })?;

        if let Some(w) = &checkout {
            let entry = match &new_staged {
                Some(s) => Some(hash_blob(repo, s)?),
                None if staged_blob.is_some() => Some(blob),
                None => None,
            };
            if let Some(b) = entry {
                git::git(
                    w,
                    &["update-index", "--cacheinfo", &format!("100644,{b},{file}")],
                )?;
            }
        }
        println!("  committed: {message}");
    }
    if let (Some(w), Some(nw)) = (&checkout, &new_working)
        && Some(nw) != working.as_ref()
    {
        fs::write(w.join(file), nw).map_err(|e| format!("cannot write {file}: {e}"))?;
    }
    if checkout.is_none() && new_committed != committed {
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

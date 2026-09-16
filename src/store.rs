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
//! The same edit is then applied to the primary worktree's file, so a checkout of
//! the trunk shows it without anyone running `git checkout`.

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
        let cfg_path = primary.join(CONFIG_FILE);
        let cfg = match fs::read_to_string(&cfg_path) {
            Ok(s) => Config::from_toml(&s)?,
            Err(_) => Config::default(),
        };
        let trunk = cfg
            .trunk
            .clone()
            .or_else(|| {
                git::opt(&primary, &["config", "git-town.main-branch"]).filter(|s| !s.is_empty())
            })
            .unwrap_or_else(|| "main".into());
        Ok(Repo {
            cwd,
            primary,
            common,
            cfg,
            trunk,
        })
    }

    pub fn file_path(&self) -> PathBuf {
        self.primary.join(&self.cfg.file)
    }

    pub fn primary_on_trunk(&self) -> bool {
        git::current_branch(&self.primary).as_deref() == Some(self.trunk.as_str())
    }

    pub fn committed(&self) -> Res<Option<String>> {
        let spec = format!("refs/heads/{}:{}", self.trunk, self.cfg.file);
        let o = git::raw(&self.primary, &["show", &spec], &[], None)?;
        Ok(o.ok.then_some(o.stdout))
    }

    /// What reads see: the primary's working copy when it has the trunk checked
    /// out (so a just-added row is visible), otherwise the trunk's committed copy.
    pub fn load(&self) -> Res<String> {
        if self.primary_on_trunk()
            && let Ok(s) = fs::read_to_string(self.file_path())
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
    /// The next free id, computed under the lock from both copies.
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

/// Apply `op` to the queue and commit it to the trunk as one commit that holds
/// nothing else. `ids` are the tasks the op touches: one present only in the
/// working copy (added but never committed by someone else's older tool) is
/// carried into the commit rather than failing.
pub fn transact(
    repo: &Repo,
    message: impl Fn(&Ctx) -> String,
    ids: &[u64],
    op: impl Fn(&mut Doc, &Ctx) -> Res<()>,
) -> Res<()> {
    let _lock = lock(repo)?;
    let trunk_ref = format!("refs/heads/{}", repo.trunk);
    let Some(old) = git::rev(&repo.primary, &trunk_ref) else {
        bail!("no trunk branch {}", repo.trunk)
    };
    let Some(committed) = repo.committed()? else {
        bail!(
            "{} is not committed on {} — run `{} init`",
            repo.cfg.file,
            repo.trunk,
            repo.cfg.cmd_tasks
        )
    };
    let on_trunk = repo.primary_on_trunk();
    let working = if on_trunk {
        fs::read_to_string(repo.file_path()).ok()
    } else {
        None
    };

    let ctx = Ctx {
        next_id: queue::max_id(&committed).max(working.as_deref().map(queue::max_id).unwrap_or(0))
            + 1,
    };

    let mut c = Doc::new(&committed);
    if let Some(w) = &working {
        let wdoc = Doc::new(w);
        let wtasks = queue::parse(w);
        for &id in ids {
            if c.block(id).is_none()
                && let (Some((at, len)), Some(t)) =
                    (wdoc.block(id), wtasks.iter().find(|t| t.id == id))
            {
                let section = t
                    .section
                    .clone()
                    .unwrap_or_else(|| repo.cfg.open_section.clone());
                c.insert(
                    wdoc.lines[at..at + len].to_vec(),
                    &section,
                    Some(&repo.cfg.done_section),
                );
            }
        }
    }
    op(&mut c, &ctx)?;
    let new_committed = c.text();

    let new_working = match &working {
        Some(w) => {
            let mut d = Doc::new(w);
            op(&mut d, &ctx)?;
            Some(d.text())
        }
        None => None,
    };

    if new_committed == committed {
        return Ok(());
    }

    // Commit with a private index, so the primary's own index — and whatever is
    // staged in it — is never read or written by a bookkeeping commit.
    let message = message(&ctx);
    let message = message.as_str();
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
            &format!("100644,{blob},{}", repo.cfg.file),
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
        &["update-ref", "-m", message, &trunk_ref, &commit, &old],
    )
    .map_err(|e| {
        format!(
            "{} moved while this ran; nothing was written. Retry. ({e})",
            repo.trunk
        )
    })?;

    if on_trunk {
        // The primary's HEAD followed the ref. Bring its index entry and file
        // along, but only where they still matched the old commit — a staged or
        // hand-made edit stays exactly as it was, now visible as a diff.
        let old_blob = git::opt(
            &repo.primary,
            &["rev-parse", &format!("{old}:{}", repo.cfg.file)],
        );
        let staged = git::opt(&repo.primary, &["ls-files", "-s", "--", &repo.cfg.file])
            .and_then(|l| l.split_whitespace().nth(1).map(String::from));
        if staged.is_some() && staged == old_blob {
            git::git(
                &repo.primary,
                &[
                    "update-index",
                    "--cacheinfo",
                    &format!("100644,{blob},{}", repo.cfg.file),
                ],
            )?;
        }
        if let Some(w) = new_working {
            fs::write(repo.file_path(), w)
                .map_err(|e| format!("cannot write {}: {e}", repo.cfg.file))?;
        }
    } else {
        eprintln!(
            "  (the primary worktree is not on {}; committed there, working copy untouched)",
            repo.trunk
        );
    }
    println!("  committed: {message}");
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

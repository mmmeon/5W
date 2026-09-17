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
use crate::config::{Config, Val};
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
    /// A bare repository: a git server, where pushes are judged.
    pub bare: bool,
    /// On a server, the trunk its admin names (`FIVEW_TRUNK` or `5w.trunk`) and
    /// how: it wins over a committed `trunk`, which `ci` refuses to disagree with.
    pub pin: Option<(String, String)>,
    /// Opened for `ci` over a trunk config that does not parse: its error. The
    /// config is then the default, and `ci` judges only a push that repairs it.
    pub broken: Option<String>,
    /// The trunk as committed state names it: what `trunk` would be if the config
    /// read were the one committed where it was read. Queue rules are judged on
    /// it, so an uncommitted `trunk` edit in a checkout moves no gate.
    pub committed_trunk: String,
}

pub const CONFIG_FILE: &str = ".5w.toml";

/// The branch HEAD names, even unborn: a bare repository's default branch.
pub fn head_branch(dir: &Path) -> Option<String> {
    git::opt(dir, &["symbolic-ref", "-q", "HEAD"])?
        .strip_prefix("refs/heads/")
        .filter(|b| !b.is_empty())
        .map(String::from)
}

/// The trunk a committed `.5w.toml` names, for a checkout with no pin and no
/// file: the branch `origin/HEAD` names first, then `main` and `master`, local
/// then remote. Counts only a `trunk` naming a branch that exists.
fn committed_trunk(dir: &Path) -> Option<String> {
    let exists = |b: &str| {
        [
            format!("refs/heads/{b}"),
            format!("refs/remotes/origin/{b}"),
        ]
        .iter()
        .find(|r| git::rev(dir, r).is_some())
        .cloned()
    };
    let head = git::opt(dir, &["symbolic-ref", "-q", "refs/remotes/origin/HEAD"])
        .and_then(|r| r.strip_prefix("refs/remotes/origin/").map(String::from))
        .filter(|b| !b.is_empty())
        .and_then(|b| exists(&b));
    let refs = head.into_iter().chain(
        [
            "refs/heads/main",
            "refs/heads/master",
            "refs/remotes/origin/main",
            "refs/remotes/origin/master",
        ]
        .map(String::from),
    );
    refs.filter_map(|r| git::opt(dir, &["show", &format!("{r}:{CONFIG_FILE}")]))
        .filter_map(|text| Config::from_toml(&text).ok()?.trunk)
        .find(|t| exists(t).is_some())
}

/// The newest `.5w.toml` that parses on `commit`'s first-parent line, from the
/// commit itself back, past commits that deleted it. None: no such commit.
pub fn last_readable_config(dir: &Path, commit: &str) -> Option<String> {
    last_config_where(dir, commit, |t| crate::config::parse_toml(t).is_ok())
}

/// The newest `.5w.toml` on `commit`'s first-parent line that `ok` accepts, as
/// `last_readable_config` walks it.
fn last_config_where(dir: &Path, commit: &str, ok: impl Fn(&str) -> bool) -> Option<String> {
    let line = git::opt(
        dir,
        &["rev-list", "--first-parent", commit, "--", CONFIG_FILE],
    )?;
    // A commit that deleted the file has no config to read: walk on past it.
    line.lines()
        .filter_map(|c| git::opt(dir, &["show", &format!("{c}:{CONFIG_FILE}")]))
        .find(|text| ok(text))
}

/// The newest config on `commit`'s first-parent line that `from_toml` accepts.
pub(crate) fn last_accepted_config(dir: &Path, commit: &str) -> Option<Config> {
    let text = last_config_where(dir, commit, |c| Config::from_toml(c).is_ok())?;
    Config::from_toml(&text).ok()
}

/// The refusal naming the repair of a break (`err`) of `trunk`'s config, whose tip
/// commit is `tip` and whose broken config gives the names in `broken`, when it
/// renamed `key`'s file in place: every name that repair restores
/// (`lint::names_to_restore`), pushed past the server's hook under `gate_trunk`.
/// None: the break did not rename `key` in place.
pub(crate) fn restore_fix(
    dir: &Path,
    trunk: &str,
    tip: &str,
    broken: &Config,
    gate_trunk: bool,
    err: &str,
    key: &str,
) -> Option<String> {
    let restores = crate::lint::names_to_restore(dir, tip, broken);
    if !restores.iter().any(|(k, ..)| *k == key) {
        return None;
    }
    let (what, names) = crate::lint::describe_restore(&restores);
    let t = trunk;
    let fix = match gate_trunk {
        true => format!(
            "an admin commits a {CONFIG_FILE} that parses with {names} on {t} and pushes it past the server's hook"
        ),
        false => format!("commit a {CONFIG_FILE} that parses with {names} on {t} and push it"),
    };
    Some(format!(
        "{CONFIG_FILE} on {t} is broken ({err}) and names {what} — {fix}"
    ))
}

/// A name a trunk config that `from_toml` refuses gives `key` (the trunk, the
/// queue's file and archive, the commit prefix), read as the config reads it: the
/// last string `kv` gives it. One holding a control character, which no config
/// takes, names nothing: the newest config on `tip`'s first-parent line whose
/// value is one line says it instead. None: neither says one.
pub(crate) fn said(
    dir: &Path,
    kv: &[(String, Val)],
    tip: Option<&str>,
    key: &str,
) -> Option<String> {
    let last = |kv: &[(String, Val)]| {
        kv.iter().rev().find_map(|(k, v)| match v {
            Val::Str(t) if k == key => Some(t.clone()),
            _ => None,
        })
    };
    let v = last(kv)?;
    if crate::config::is_one_line(&v) {
        return Some(v);
    }
    let says = |t: &str| crate::config::parse_toml(t).ok().map(|kv| last(&kv));
    let text = last_config_where(dir, tip?, |t| {
        says(t).is_some_and(|v| v.as_deref().is_none_or(crate::config::is_one_line))
    })?;
    says(&text).flatten()
}

/// The trunk and the names the gate tells queue edits and landings by, for a
/// trunk config `text` that `from_toml` refuses on commit `tip`: one resolution,
/// so a server judges by the names a checkout's repair records under. What the
/// text says where it parses, as the config reads it (the last key winning);
/// text that does not parse says nothing, and the trunk's last config that does
/// speaks for it (even one 5w rejects); a name no such text gives is the default.
/// The rest of the returned config is the defaults.
pub(crate) fn broken_names(dir: &Path, text: &str, tip: Option<&str>) -> Config {
    let kv = crate::config::parse_toml(text)
        .ok()
        .or_else(|| {
            tip.and_then(|t| last_readable_config(dir, t))
                .and_then(|t| crate::config::parse_toml(&t).ok())
        })
        .unwrap_or_default();
    let said = |key: &str| said(dir, &kv, tip, key);
    let d = Config::default();
    Config {
        trunk: said("trunk"),
        file: said("file").unwrap_or(d.file.clone()),
        archive: said("archive").unwrap_or(d.archive.clone()),
        commit_prefix: said("commit_prefix").unwrap_or(d.commit_prefix.clone()),
        ..d
    }
}

/// How `open` meets a trunk config that does not parse.
#[derive(Clone, Copy, PartialEq)]
enum Broken {
    /// Refuse: the error is the command's.
    Refuse,
    /// `ci` on a server: the names the trunk says, over the defaults.
    Judge,
    /// A checkout's queue and ship: the trunk's last config that `from_toml`
    /// accepts, told by the names and gate settings the server reads, so the
    /// reviewed repair can be accepted, landed and recorded.
    Repair,
}

/// The fix a broken trunk config names in a checkout: its keys may be a newer 5w's.
pub fn repair_fix() -> &'static str {
    "upgrade 5w, or ship the repair"
}

/// A checkout's config over a trunk commit whose `.5w.toml` does not parse: the
/// newest one on its first-parent line that does, with the names (`broken_names`,
/// the server's own resolution) and the `gate_trunk` and `require_task` settings
/// read as the server's gate reads them (failing closed). None: a server, no
/// such trunk commit, a working copy that differs from it (fixed in place), a
/// `requires` newer than this 5w, or no config on the line that reads.
fn repair_config(primary: &Path, trunk: &str, bare: bool) -> Option<Config> {
    if bare {
        return None;
    }
    let tip = [
        format!("refs/heads/{trunk}"),
        format!("refs/remotes/origin/{trunk}"),
    ]
    .iter()
    .find_map(|r| git::rev(primary, r))?;
    let text = git::opt(primary, &["show", &format!("{tip}:{CONFIG_FILE}")])?;
    if Config::from_toml(&text).is_ok() || crate::config::requires_newer(&text).is_some() {
        return None;
    }
    if let Some(w) = git::worktree_of(primary, trunk).ok().flatten()
        && fs::read_to_string(w.join(CONFIG_FILE))
            .ok()
            .as_deref()
            .map(|t| t.trim_end_matches('\n'))
            != Some(text.as_str())
    {
        return None;
    }
    let base = last_config_where(primary, &tip, |t| Config::from_toml(t).is_ok())?;
    let names = broken_names(primary, &text, Some(&tip));
    let mut cfg = Config {
        trunk: names.trunk,
        file: names.file,
        archive: names.archive,
        commit_prefix: names.commit_prefix,
        ..Config::from_toml(&base).ok()?
    };
    cfg.gate_trunk = crate::ci::setting_on(primary, Some(&tip), "gate_trunk");
    cfg.require_task = crate::ci::setting_on(primary, Some(&tip), "require_task");
    Some(cfg)
}

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
        Repo::open_with(Broken::Refuse)
    }

    /// Open even when the trunk's config does not parse, noting its error: a
    /// server must still judge the push that fixes it (`ci`).
    pub fn open_lenient() -> Res<Repo> {
        Repo::open_with(Broken::Judge)
    }

    /// Open a checkout whose trunk commits a config that does not parse by the
    /// trunk's last one that does, noting the error: its repair is reviewed and
    /// shipped like any change (a gated server takes it only with its landing).
    pub fn open_for_repair() -> Res<Repo> {
        Repo::open_with(Broken::Repair)
    }

    fn open_with(mode: Broken) -> Res<Repo> {
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
        // git config, and let the config override it (not a server's pin).
        let pin = std::env::var("FIVEW_TRUNK")
            .ok()
            .filter(|s| !s.is_empty())
            .map(|t| (t.clone(), format!("FIVEW_TRUNK={t}")))
            .or_else(|| {
                git::opt(&primary, &["config", "5w.trunk"])
                    .filter(|s| !s.is_empty())
                    .map(|t| (t.clone(), format!("5w.trunk = {t}")))
            });
        let mut guess = pin
            .as_ref()
            .map(|(t, _)| t.clone())
            // A server has no checkout to ask; its HEAD names the default branch.
            .or_else(|| bare.then(|| head_branch(&primary)).flatten())
            .or_else(|| {
                git::opt(&primary, &["config", "git-town.main-branch"]).filter(|s| !s.is_empty())
            })
            .unwrap_or_else(|| "main".into());
        let checkout = std::cell::Cell::new(false);
        let on_trunk = |t: &str| {
            let text = git::worktree_of(&primary, t)
                .ok()
                .flatten()
                .and_then(|w| fs::read_to_string(w.join(CONFIG_FILE)).ok());
            checkout.set(text.is_some());
            text.or_else(|| {
                git::opt(
                    &primary,
                    &["show", &format!("refs/heads/{t}:{CONFIG_FILE}")],
                )
            })
            // A CI checkout often has the trunk only as a remote-tracking ref.
            .or_else(|| {
                git::opt(
                    &primary,
                    &["show", &format!("refs/remotes/origin/{t}:{CONFIG_FILE}")],
                )
            })
        };
        let mut src = on_trunk(&guess);
        let mut from_primary = src.is_none();
        if from_primary {
            src = fs::read_to_string(primary.join(CONFIG_FILE)).ok();
        }
        // `5w.trunk` is local: a clone has no pin, and its primary checkout may
        // be on a branch without the file. The committed config names the trunk.
        if src.is_none()
            && !bare
            && pin.is_none()
            && let Some(t) = committed_trunk(&primary)
        {
            src = on_trunk(&t);
            from_primary = false;
            guess = t;
        }
        let mut broken = None;
        let in_checkout = checkout.get() && !from_primary;
        let cfg = match src {
            Some(s) => match Config::from_toml(&s) {
                Ok(c) => Config {
                    checkout_text: in_checkout.then_some(s),
                    ..c
                },
                Err(e) if mode == Broken::Repair => match repair_config(&primary, &guess, bare) {
                    Some(c) => {
                        broken = Some(e);
                        c
                    }
                    None => return Err(e),
                },
                Err(e) if mode == Broken::Judge => {
                    broken = Some(e);
                    let tip = [
                        format!("refs/heads/{guess}"),
                        format!("refs/remotes/origin/{guess}"),
                    ]
                    .iter()
                    .find_map(|r| git::rev(&primary, r));
                    broken_names(&primary, &s, tip.as_deref())
                }
                Err(e) => return Err(e),
            },
            None => Config::default(),
        };
        // A committed rename must not move a server's gate off the branch its
        // admin pinned: there the pin wins, and `ci` refuses the disagreement.
        let pin = pin.filter(|_| bare);
        let trunk = match &pin {
            Some((t, _)) => t.clone(),
            None => cfg.trunk.clone().unwrap_or(guess.clone()),
        };
        // The same resolution over the committed copy of what was read. A broken
        // config or a pin already reads committed text.
        let committed_trunk = if broken.is_some() || pin.is_some() {
            trunk.clone()
        } else {
            let text = if from_primary {
                git::opt(&primary, &["show", &format!("HEAD:{CONFIG_FILE}")])
            } else {
                [
                    format!("refs/heads/{guess}"),
                    format!("refs/remotes/origin/{guess}"),
                ]
                .iter()
                .find_map(|r| git::opt(&primary, &["show", &format!("{r}:{CONFIG_FILE}")]))
            };
            text.and_then(|t| crate::config::parse_toml(&t).ok())
                .and_then(|kv| said(&primary, &kv, None, "trunk"))
                .unwrap_or(guess)
        };
        Ok(Repo {
            cwd,
            primary,
            common,
            cfg,
            trunk,
            bare,
            pin,
            broken,
            committed_trunk,
        })
    }

    /// Why a ship of `tip` onto a trunk whose committed config does not parse is
    /// refused: what would land, `tip` merged onto the trunk, must commit a config
    /// that parses (or none) and keeps the queue names the trunk's gate reads.
    /// A repair restoring names renamed in place (`restore_fix`) is refused too,
    /// naming the admin's push. None: the trunk reads, or `tip` lands such a repair.
    pub fn unrepaired(&self, tip: &str) -> Option<String> {
        let e = self.broken.as_ref()?;
        let (p, t) = (&self.primary, &self.trunk);
        let trunk = git::rev(p, &format!("refs/heads/{t}"))?;
        let show = |at: &str| git::opt(p, &["show", &format!("{at}:{CONFIG_FILE}")]);
        if show(&trunk).is_none_or(|text| Config::from_toml(&text).is_ok()) {
            return None;
        }
        let fix = format!("{CONFIG_FILE} on {t} is broken ({e}) — {}", repair_fix());
        // A conflicting merge lands nothing: a repair must merge cleanly, so one
        // that conflicts (typically forked before the break) is rebased first.
        let Ok(merged) = git::raw(p, &["merge-tree", "--write-tree", &trunk, tip], &[], None)
        else {
            return Some(fix);
        };
        // A conflict still writes a tree; any other failure (a git before 2.38) does not.
        let tree = merged.stdout.lines().next().unwrap_or("");
        if !(matches!(tree.len(), 40 | 64) && tree.bytes().all(|b| b.is_ascii_hexdigit())) {
            return Some(fix);
        }
        if !merged.ok {
            let repairs = git::opt(p, &["merge-base", &trunk, tip]).is_some_and(|base| {
                !git::ok(p, &["diff", "--quiet", &base, tip, "--", CONFIG_FILE])
            });
            return Some(if repairs {
                format!(
                    "{CONFIG_FILE} on {t} is broken ({e}) — the repair conflicts with {t}; rebase it onto {t}"
                )
            } else {
                fix
            });
        }
        let cfg = match show(tree) {
            Some(text) => match Config::from_toml(&text) {
                Ok(c) => c,
                Err(_) => return Some(fix),
            },
            None => Config::default(),
        };
        let was = &self.cfg;
        // One restoring the names renamed in place (#113) keeps none of the broken
        // names, and no landing covers it: its record would go in a file the trunk
        // lacks. Refuse naming the admin's push past the hook, as reads do.
        if let Some(restores) = crate::lint::restored_name(p, &trunk, was, &cfg) {
            return restore_fix(p, t, &trunk, was, was.gate_trunk, e, restores[0].0);
        }
        [
            ("file", &was.file, &cfg.file),
            ("archive", &was.archive, &cfg.archive),
            ("commit_prefix", &was.commit_prefix, &cfg.commit_prefix),
        ]
        .into_iter()
        .find(|(_, old, new)| old != new)
        .map(|(key, old, _)| {
            format!(
                "a repair of {t}'s {CONFIG_FILE} keeps {key} = {old:?}; rename in a later commit"
            )
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
        if self.local_trunk() {
            return self.committed_file(name);
        }
        // A fresh clone may have the trunk only as origin's: read the queue there.
        let spec = format!("refs/remotes/origin/{}:{name}", self.trunk);
        Ok(git::opt(&self.primary, &["show", &spec]))
    }

    fn local_trunk(&self) -> bool {
        git::rev(&self.primary, &format!("refs/heads/{}", self.trunk)).is_some()
    }

    /// The fix for a trunk that exists only as origin's, when it does.
    pub fn origin_only_fix(&self) -> Option<String> {
        let t = &self.trunk;
        (!self.local_trunk()
            && git::rev(&self.primary, &format!("refs/remotes/origin/{t}")).is_some())
        .then(|| format!("no local branch {t} — `git branch {t} origin/{t}`"))
    }

    pub fn load(&self) -> Res<String> {
        match self.load_file(&self.cfg.file)? {
            Some(s) => Ok(s),
            None if let Some(fix) = self.origin_only_fix() => bail!("{fix}"),
            None if let Some(e) = &self.broken => bail!("{}", self.unmoved_queue(e)),
            None => bail!("{}", self.no_queue()),
        }
    }

    /// The refusal for a queue file the trunk does not have: `init`, unless only
    /// the trunk checkout's uncommitted config renames it.
    fn no_queue(&self) -> String {
        let (name, t) = (&self.cfg.file, &self.trunk);
        match crate::lint::committed_rules(self) {
            Some(c) if self.cfg.checkout_text.is_some() && c.file != *name => format!(
                "no {name} on {t}: only the uncommitted {CONFIG_FILE} names it ({t} commits {}) — commit {CONFIG_FILE} on {t} with the queue renamed, or revert it",
                c.file
            ),
            _ => format!("no {name} on {t} — `{} init`", self.cfg.cmd_tasks),
        }
    }

    /// Why a trunk whose committed config is broken (`e`) has no queue file by the
    /// name the server reads: the break renamed or dropped `file` and left the queue
    /// where the trunk's last accepted config keeps it. No queue commit lands the
    /// repair then — the server wants its landing record in a file the trunk lacks
    /// — so the fix is that name back, which the pre-commit hook takes
    /// (`lint::restores_accepted_names`), pushed past a gated server's hook.
    fn unmoved_queue(&self, e: &str) -> String {
        let (t, new) = (&self.trunk, &self.cfg.file);
        self.restore_fix(e, "file").unwrap_or_else(|| {
            format!(
                "no {new} on {t}, the queue its broken {CONFIG_FILE} names ({e}) — fix {CONFIG_FILE} on {t}"
            )
        })
    }

    /// `restore_fix` over the trunk's committed tip.
    fn restore_fix(&self, e: &str, key: &str) -> Option<String> {
        let tip = crate::lint::trunk_tip(&self.primary, &self.trunk)?;
        restore_fix(
            &self.primary,
            &self.trunk,
            &tip,
            &self.cfg,
            self.cfg.gate_trunk,
            e,
            key,
        )
    }

    /// Closed tasks moved out of the queue by `archive`. Empty when there is none.
    pub fn load_archive(&self) -> Res<String> {
        match self.load_file(&self.cfg.archive)? {
            Some(s) => Ok(s),
            None => self.no_archive(),
        }
    }

    /// The archive as the trunk's tip commits it, read as `load_archive` reads a
    /// missing one: what ship's gate counts.
    pub fn committed_archive(&self) -> Res<String> {
        match self.committed_file(&self.cfg.archive)? {
            Some(s) => Ok(s),
            None => self.no_archive(),
        }
    }

    /// A trunk without the archive has none yet — unless its committed config is
    /// broken and renamed the archive in place, leaving the closed tasks under the
    /// name the trunk's last accepted config keeps. Read as empty, they would drop
    /// out of every read, their ids would be reused and their rows stop authorising
    /// a ship: refuse with the name back as the fix, which the pre-commit hook takes
    /// (`lint::restores_accepted_names`), pushed past a gated server's hook.
    fn no_archive(&self) -> Res<String> {
        let Some(e) = &self.broken else {
            return Ok(String::new());
        };
        match self.restore_fix(e, "archive") {
            Some(fix) => bail!("{fix}"),
            None => Ok(String::new()),
        }
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
        match repo.origin_only_fix() {
            Some(fix) => bail!("{fix}"),
            None => bail!("no trunk branch {}", repo.trunk),
        }
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
        clear_missed(repo);
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

/// The marker a queue commit leaves when the trunk checkout's index could not
/// take it: `5w-missed-<trunk>` in the git common dir (a `/` in the trunk's name
/// as `%`), holding the trunk's tip before the first commit missed and after the
/// last. Only while it stands does `lint --staged` read an archived row back in
/// the queue as that missed commit rather than a deliberate unarchive (see
/// `missed_marker`).
const MISSED: &str = "5w-missed-";

fn missed_path(repo: &Repo) -> PathBuf {
    repo.common
        .join(format!("{MISSED}{}", repo.trunk.replace('/', "%")))
}

/// Record a commit from `old` to `new` the checkout's index missed, keeping the
/// earliest `old` of a marker already there.
fn record_missed(repo: &Repo, old: &str, new: &str) {
    let path = missed_path(repo);
    let first = fs::read_to_string(&path)
        .ok()
        .and_then(|t| t.split_whitespace().next().map(String::from))
        .filter(|f| f.bytes().all(|b| b.is_ascii_hexdigit()) && !f.is_empty());
    let _ = fs::write(
        &path,
        format!("{} {new}\n", first.as_deref().unwrap_or(old)),
    );
}

/// The checkout caught up: the marker goes.
fn clear_missed(repo: &Repo) {
    let _ = fs::remove_file(missed_path(repo));
}

/// What the marker says of the trunk checkout now.
pub enum Missed {
    /// The index still lacks the missed commits: the fix that catches it up.
    Live(String),
    /// The index has taken them, or they cannot be read: the marker is left over.
    Stale,
}

/// The marker, read without touching it. It stands for the checkout while any
/// row the missed commits changed or moved (from the marker's first sha to its
/// last) reads, in the checkout's staged queue and archive, otherwise than the
/// last one has it — so a hand fix (the named diff, or the files restored) makes
/// it stale, whatever else is staged, and a hand edit that leaves one of those
/// rows behind (a new row staged over the missed archive) keeps it standing.
/// Its limit: while it stands, a deliberate unarchive of a row the missed
/// commits themselves archived reads as the miss; any write or `lint --staged`
/// with the checkout caught up clears it first.
pub fn missed_marker(repo: &Repo) -> Option<Missed> {
    let text = fs::read_to_string(missed_path(repo)).ok()?;
    let mut shas = text.split_whitespace();
    let (Some(from), Some(last)) = (shas.next(), shas.next()) else {
        return Some(Missed::Stale);
    };
    let w = repo.trunk_checkout().ok()??;
    let commit = |c: &str| {
        git::ok(
            &repo.primary,
            &["cat-file", "-e", &format!("{c}^{{commit}}")],
        )
    };
    let Some(tip) = git::rev(&repo.primary, &format!("refs/heads/{}", repo.trunk)) else {
        return Some(Missed::Stale);
    };
    if !commit(from) || !commit(last) {
        return Some(Missed::Stale);
    }
    let names = [repo.cfg.file.as_str(), repo.cfg.archive.as_str()];
    // A file a side lacks reads as empty.
    let blob = |dir: &Path, spec: String| {
        git::raw(dir, &["cat-file", "blob", &spec], &[], None)
            .ok()
            .filter(|o| o.ok)
            .map(|o| o.stdout)
            .unwrap_or_default()
    };
    let at = |rev: &str| names.map(|n| blob(&repo.primary, format!("{rev}:{n}")));
    let (before, after) = (at(from), at(last));
    let staged = names.map(|n| blob(&w, format!(":{n}")));
    let (before, after, staged) = (
        [before[0].as_str(), before[1].as_str()],
        [after[0].as_str(), after[1].as_str()],
        [staged[0].as_str(), staged[1].as_str()],
    );
    // Per file, so a row moving between them counts.
    let mut ids = rows_changed([before[0], ""], [after[0], ""]);
    ids.extend(rows_changed(["", before[1]], ["", after[1]]));
    let behind = ids.into_iter().any(|id| row(staged, id) != row(after, id));
    if !behind {
        return Some(Missed::Stale);
    }
    let g = format!("git -C {}", shell_word(&w.to_string_lossy()));
    let diff = format!(
        "{g} diff {}..{} -- {}",
        &from[..from.len().min(12)],
        &tip[..tip.len().min(12)],
        names.map(shell_word).join(" ")
    );
    Some(Missed::Live(format!(
        "{diff} | {g} apply && {diff} | {g} apply --cached"
    )))
}

/// The marker's fix while it stands; a stale marker is removed here, and `None`.
/// For `lint --staged` only: reads that report (doctor, audit) use
/// `missed_marker`, and write nothing.
pub fn missed_marker_fix(repo: &Repo) -> Option<String> {
    match missed_marker(repo)? {
        Missed::Live(fix) => Some(fix),
        Missed::Stale => {
            clear_missed(repo);
            None
        }
    }
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

/// The refusal for an edit of an archived row: only an unarchive commit, made by
/// hand, brings it back.
pub fn archived(repo: &Repo, id: u64) -> String {
    format!(
        "#{id} is archived; to reopen it, move its block from {} back to {} unchanged and commit it as `{}: unarchive #{id}`, then `{} reopen {id}`",
        repo.cfg.archive, repo.cfg.file, repo.cfg.commit_prefix, repo.cfg.cmd_tasks
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
        crate::lint::say_note();
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
            mirror_index(repo, w, &changes, &blobs).map_err(|e| {
                record_missed(repo, old, &commit);
                landed(e, &names, true)
            })?;
            clear_missed(repo);
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

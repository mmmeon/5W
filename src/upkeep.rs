//! Keeping a project's 5W current: the version it requires, and the files 5W
//! installed into it.
//!
//! A project pins the oldest 5W it works with (`requires` in `.5w.toml`); a
//! binary older than that refuses with the version to get, instead of failing
//! on some config key it does not know. Files `init` and `hook install` write
//! carry the version that wrote them, so `doctor` can say they are stale and
//! `update-files` can refresh them — as an ordinary change to review.

use crate::bail;
use crate::git;
use crate::store::Repo;
use crate::util::Res;
use std::fs;
use std::path::{Path, PathBuf};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
const PROTOCOL_TEMPLATE: &str = include_str!("../templates/PROTOCOL.md");
const PRE_RECEIVE: &str = include_str!("../ci/pre-receive");
pub const HOOK_MARK: &str = "# installed by 5w";
const RELEASES: &str = "https://github.com/mmmeon/5W/releases";

pub fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let mut it = s.trim().trim_start_matches('v').split('.');
    let v = (
        it.next()?.parse().ok()?,
        it.next()?.parse().ok()?,
        it.next()?.parse().ok()?,
    );
    it.next().is_none().then_some(v)
}

/// Refuse to run against a project that needs a newer 5w.
pub fn check_requires(required: &str) -> Res<()> {
    let Some(want) = parse_version(required) else {
        bail!("config: requires must be a version like \"0.1.2\", not {required:?}")
    };
    let have = parse_version(VERSION).expect("own version parses");
    if have < want {
        bail!(
            "this project requires 5w {required} or later; this is {VERSION}.\n  Get it: {RELEASES}/tag/v{required}"
        );
    }
    Ok(())
}

/// PROTOCOL.md as this version writes it: the template, stamped.
pub fn protocol_text() -> String {
    format!("<!-- 5w {VERSION} protocol -->\n{PROTOCOL_TEMPLATE}")
}

/// The version stamped in a file 5w wrote, if any.
fn stamp(text: &str, marker: &str) -> Option<String> {
    text.lines().take(3).find_map(|l| {
        let rest = l.split("5w ").nth(1)?;
        let mut words = rest.split_whitespace();
        let v = words.next()?;
        (words.next() == Some(marker)).then(|| v.to_string())
    })
}

pub fn hook_script(repo: &Repo, kind: &str) -> String {
    if kind == "pre-receive" {
        let (shebang, rest) = PRE_RECEIVE.split_once('\n').unwrap_or((PRE_RECEIVE, ""));
        return format!("{shebang}\n# 5w {VERSION} hook\n{rest}");
    }
    format!(
        "#!/bin/sh\n{HOOK_MARK} — checks queue edits against PROTOCOL.md\n# 5w {VERSION} hook\n\
         if command -v 5w >/dev/null 2>&1; then\n  exec 5w lint --staged\nfi\n\
         if git diff --cached --name-only | grep -qx -e '{}' -e '{}'; then\n  \
         echo '5w is not installed: this queue edit is unchecked. Follow PROTOCOL.md.' >&2\nfi\nexit 0\n",
        repo.cfg.file, repo.cfg.archive
    )
}

pub fn hook_path(repo: &Repo, kind: &str) -> Res<PathBuf> {
    let dir = git::opt(
        &repo.primary,
        &["rev-parse", "--path-format=absolute", "--git-path", "hooks"],
    )
    .ok_or("cannot find the hooks directory")?;
    Ok(PathBuf::from(dir).join(kind))
}

/// Notes for `doctor`: what is older than this binary, or missing a pin.
pub fn notes(repo: &Repo) -> Res<Vec<String>> {
    let mut out = Vec::new();
    match &repo.cfg.requires {
        None => out.push(format!(
            "no `requires` in .5w.toml — `5w update-files --pin` pins {VERSION}, so an older 5w refuses clearly"
        )),
        Some(r) if parse_version(r) < parse_version(VERSION) => out.push(format!(
            "requires = \"{r}\"; this is {VERSION} — raise the pin once the project relies on {VERSION} (`5w update-files --pin`)"
        )),
        _ => {}
    }
    if let Some(p) = repo.committed_file("PROTOCOL.md")?
        && p != protocol_text()
    {
        let from = stamp(&p, "protocol").unwrap_or_else(|| "an unstamped version".into());
        out.push(if from == VERSION {
            "PROTOCOL.md was edited by hand; `5w update-files` restores 5w's text".into()
        } else {
            format!("PROTOCOL.md is from {from}; this is {VERSION} — `5w update-files`")
        });
    }
    for kind in ["pre-commit", "pre-receive"] {
        let path = hook_path(repo, kind)?;
        if let Ok(t) = fs::read_to_string(&path)
            && t.contains(HOOK_MARK)
            && t != hook_script(repo, kind)
        {
            out.push(format!(
                "the {kind} hook is from an older 5w — `5w update-files` refreshes it"
            ));
        }
    }
    Ok(out)
}

/// `5w update-files [--pin]`: rewrite what 5w installed, in the worktree you
/// stand in, and leave it for you to review and commit.
pub fn update_files(repo: &Repo, args: &[String]) -> Res<()> {
    let pin = args.iter().any(|a| a == "--pin");
    if let Some(a) = args.iter().find(|a| *a != "--pin") {
        bail!("unexpected {a:?}\nusage: 5w update-files [--pin]");
    }
    let top = git::git(&repo.cwd, &["rev-parse", "--show-toplevel"])
        .map_err(|_| "update-files writes into a worktree; run it inside one".to_string())?;
    let top = Path::new(&top);
    let mut changed = Vec::new();

    let proto = top.join("PROTOCOL.md");
    if fs::read_to_string(&proto).ok().as_deref() != Some(protocol_text().as_str()) {
        fs::write(&proto, protocol_text()).map_err(|e| e.to_string())?;
        changed.push("PROTOCOL.md".to_string());
    }

    if pin {
        let cfg = top.join(crate::store::CONFIG_FILE);
        let text = fs::read_to_string(&cfg)
            .map_err(|_| format!("no {} in this worktree", crate::store::CONFIG_FILE))?;
        let line = format!("requires = \"{VERSION}\"");
        let mut found = false;
        let mut lines: Vec<String> = text
            .lines()
            .map(|l| {
                if l.trim_start().starts_with("requires") && l.contains('=') && !found {
                    found = true;
                    line.clone()
                } else {
                    l.to_string()
                }
            })
            .collect();
        if !found {
            // Before the first key, after any leading comments.
            let at = lines
                .iter()
                .position(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
                .unwrap_or(lines.len());
            lines.insert(at, line.clone());
            lines.insert(
                at,
                "# The oldest 5w this project works with; an older one refuses.".into(),
            );
        }
        let new = lines.join("\n") + "\n";
        if new != text {
            fs::write(&cfg, new).map_err(|e| e.to_string())?;
            changed.push(format!("{} ({line})", crate::store::CONFIG_FILE));
        }
    }

    for kind in ["pre-commit", "pre-receive"] {
        let path = hook_path(repo, kind)?;
        if let Ok(t) = fs::read_to_string(&path)
            && t.contains(HOOK_MARK)
            && t != hook_script(repo, kind)
        {
            fs::write(&path, hook_script(repo, kind)).map_err(|e| e.to_string())?;
            changed.push(format!("{kind} hook"));
        }
    }

    if changed.is_empty() {
        println!("  up to date with 5w {VERSION}");
    } else {
        for c in &changed {
            println!("  updated {c}");
        }
        println!(
            "  review and commit the tracked files like any change (hooks are local and need no commit)"
        );
    }
    Ok(())
}

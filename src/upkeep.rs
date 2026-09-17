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
const RELEASES: &str = concat!(env!("CARGO_PKG_REPOSITORY"), "/releases");
/// The key release sums are signed with, as the repository ships it.
const SIGNING_KEY: &str = include_str!("../SIGNING_KEY.asc");

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

// --- version --latest and self-update ----------------------------------------------------
//
// Releases are read from `<repository>/releases` (FIVEW_RELEASES_URL overrides it,
// for a mirror): `latest/download/SHA256SUMS` names the newest version in its
// asset names, and `download/v<version>/` holds the binaries, SHA256SUMS and
// SHA256SUMS.asc. curl fetches, gpg checks the signature against the key built
// into this binary (FIVEW_RELEASE_KEY names another armored key), and the hash
// is computed here.

fn releases_url() -> String {
    std::env::var("FIVEW_RELEASES_URL")
        .ok()
        .filter(|s| !s.is_empty())
        .map(|s| s.trim_end_matches('/').to_string())
        .unwrap_or_else(|| RELEASES.to_string())
}

fn asset_name(version: &str) -> String {
    format!("5w-{version}-{}-unknown-linux-musl", std::env::consts::ARCH)
}

/// Fail with one line if a tool is not on PATH.
fn need(tool: &str, what: &str) -> Res<()> {
    match std::process::Command::new(tool)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
    {
        Ok(_) => Ok(()),
        Err(_) => {
            bail!("{what} needs {tool} on PATH; install {tool}, or get 5w by hand: {RELEASES}")
        }
    }
}

/// Download `url` to `to`; `Ok(false)` when the server has no such file.
fn fetch(url: &str, to: &Path) -> Res<bool> {
    let o = std::process::Command::new("curl")
        .args([
            "-fsSL",
            "--proto",
            "=https,file",
            "--proto-redir",
            "=https",
            "-o",
        ])
        .arg(to)
        .arg(url)
        .output()
        .map_err(|e| format!("curl: {e}"))?;
    if o.status.success() {
        return Ok(true);
    }
    // 22: an HTTP error (404); 37: a file:// path that does not exist.
    if matches!(o.status.code(), Some(22) | Some(37)) {
        return Ok(false);
    }
    let err = String::from_utf8_lossy(&o.stderr);
    bail!(
        "cannot download {url}: {}",
        err.lines().next().unwrap_or("curl failed").trim()
    )
}

/// A private scratch directory, removed when dropped.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Res<Scratch> {
        use std::os::unix::fs::DirBuilderExt;
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let p = std::env::temp_dir().join(format!("5w-update-{}-{nanos}", std::process::id()));
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&p)
            .map_err(|e| format!("cannot create {}: {e}", p.display()))?;
        Ok(Scratch(p))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let gnupg = self.0.join("gnupg");
        if gnupg.exists() {
            let _ = std::process::Command::new("gpgconf")
                .args(["--kill", "all"])
                .env("GNUPGHOME", &gnupg)
                .output();
        }
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// The newest release's version, from the asset names in its SHA256SUMS.
fn latest(tmp: &Path) -> Res<String> {
    let url = format!("{}/latest/download/SHA256SUMS", releases_url());
    let to = tmp.join("latest-SHA256SUMS");
    if !fetch(&url, &to)? {
        bail!("no release found at {url}");
    }
    let text = fs::read_to_string(&to).map_err(|e| e.to_string())?;
    text.lines()
        .filter_map(|l| l.split_whitespace().nth(1)?.strip_prefix("5w-"))
        .filter_map(|rest| rest.split('-').next())
        .find(|v| parse_version(v).is_some())
        .map(str::to_string)
        .ok_or_else(|| format!("{url} names no 5w version"))
}

pub fn own_version_line() -> String {
    match option_env!("FIVEW_COMMIT") {
        Some(c) => format!("5w {VERSION} ({c})"),
        None => format!("5w {VERSION}"),
    }
}

/// `5w version [--latest]`: this binary's version; with `--latest`, the newest
/// release too. Changes nothing.
pub fn version(args: &[String]) -> Res<()> {
    let mut want_latest = false;
    for a in args {
        match a.as_str() {
            "--latest" => want_latest = true,
            f if f.starts_with('-') => bail!("unknown flag {f} for version (5w version --help)"),
            a => bail!("unexpected {a:?} (5w version --help)"),
        }
    }
    if !want_latest {
        println!("{}", own_version_line());
        return Ok(());
    }
    need("curl", "version --latest")?;
    let tmp = Scratch::new()?;
    let newest = latest(&tmp.0)?;
    let (have, new) = (parse_version(VERSION), parse_version(&newest));
    if new > have {
        println!("5w {VERSION}; newest release {newest} → 5w self-update --latest");
    } else if new == have {
        println!("5w {VERSION} is the newest release");
    } else {
        println!("5w {VERSION}; newest release {newest} is older");
    }
    Ok(())
}

/// `requires` from the `.5w.toml` of the worktree you stand in, read without
/// loading the config — which refuses when the pin is newer than this binary,
/// the very case self-update is for.
fn pinned() -> Option<String> {
    let cwd = std::env::current_dir().ok()?;
    let top = git::opt(&cwd, &["rev-parse", "--show-toplevel"])?;
    let text = fs::read_to_string(Path::new(&top).join(crate::store::CONFIG_FILE)).ok()?;
    text.lines().find_map(|l| {
        let (k, v) = l.split_once('=')?;
        (k.trim() == "requires").then(|| v.trim().trim_matches('"').to_string())
    })
}

/// `5w self-update [--latest]`: install the version the project pins, or the
/// newest, after checking SHA256SUMS is signed by the release key and the
/// binary matches it. Nothing is written unless both hold.
pub fn self_update(args: &[String]) -> Res<()> {
    let mut want_latest = false;
    for a in args {
        match a.as_str() {
            "--latest" => want_latest = true,
            f if f.starts_with('-') => {
                bail!("unknown flag {f} for self-update (5w self-update --help)")
            }
            a => bail!("unexpected {a:?} (5w self-update --help)"),
        }
    }
    if std::env::consts::OS != "linux" {
        bail!("self-update installs Linux binaries; get 5w by hand: {RELEASES}");
    }
    need("curl", "self-update")?;
    need("gpg", "self-update")?;
    let tmp = Scratch::new()?;
    let have = parse_version(VERSION);
    let target = if want_latest {
        let v = latest(&tmp.0)?;
        if parse_version(&v) <= have {
            println!("5w {VERSION} is the newest release; nothing to do");
            return Ok(());
        }
        v
    } else {
        let Some(v) = pinned() else {
            bail!(
                "no `requires` in {} here to install; run in a project that pins one, or 5w self-update --latest",
                crate::store::CONFIG_FILE
            )
        };
        if parse_version(&v).is_none() {
            bail!(
                "requires = {v:?} is not a version like \"0.1.3\"; fix {}",
                crate::store::CONFIG_FILE
            );
        }
        if parse_version(&v) <= have {
            println!(
                "5w {VERSION} satisfies requires = \"{v}\"; nothing to do (5w self-update --latest for the newest)"
            );
            return Ok(());
        }
        v
    };

    let exe = std::env::current_exe().map_err(|e| format!("cannot find this binary: {e}"))?;
    let base = format!("{}/download/v{target}", releases_url());
    let asset = asset_name(&target);
    let sums = tmp.0.join("SHA256SUMS");
    let sig = tmp.0.join("SHA256SUMS.asc");
    let bin = tmp.0.join(&asset);
    if !fetch(&format!("{base}/SHA256SUMS"), &sums)? {
        bail!("no release v{target} at {base}");
    }
    if !fetch(&format!("{base}/SHA256SUMS.asc"), &sig)? {
        bail!("release v{target} has no SHA256SUMS.asc (not signed yet); nothing installed");
    }
    if !fetch(&format!("{base}/{asset}"), &bin)? {
        bail!("release v{target} has no {asset}; nothing installed");
    }

    verify_signature(&tmp.0, &sums, &sig)
        .map_err(|e| format!("v{target}: {e}; nothing installed"))?;
    let listed = fs::read_to_string(&sums)
        .map_err(|e| e.to_string())?
        .lines()
        .find_map(|l| {
            let mut w = l.split_whitespace();
            let h = w.next()?;
            (w.next()?.trim_start_matches('*') == asset).then(|| h.to_ascii_lowercase())
        });
    let Some(listed) = listed else {
        bail!("SHA256SUMS for v{target} does not list {asset}; nothing installed");
    };
    let bytes = fs::read(&bin).map_err(|e| e.to_string())?;
    if hex(&sha256(&bytes)) != listed {
        bail!("{asset} does not match SHA256SUMS; nothing installed");
    }

    // Beside the target, so the rename is atomic; synced before and after.
    let dir = exe.parent().ok_or("this binary has no directory")?;
    let staged = dir.join(format!(".5w-update-{}", std::process::id()));
    let written = (|| -> std::io::Result<()> {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o755)
            .open(&staged)?;
        f.write_all(&bytes)?;
        f.sync_all()?;
        drop(f);
        fs::rename(&staged, &exe)?;
        fs::File::open(dir)?.sync_all()
    })();
    if let Err(e) = written {
        let _ = fs::remove_file(&staged);
        bail!(
            "cannot replace {}: {e}; rerun as a user who can write {}",
            exe.display(),
            dir.display()
        );
    }
    println!("5w {VERSION} → {target} ({})", exe.display());
    Ok(())
}

/// Check `sig` is a good signature over `sums` by the release key, in a
/// keyring holding that key alone.
fn verify_signature(tmp: &Path, sums: &Path, sig: &Path) -> Res<()> {
    use std::os::unix::fs::DirBuilderExt;
    let key = match std::env::var_os("FIVEW_RELEASE_KEY") {
        Some(p) => fs::read_to_string(&p)
            .map_err(|e| format!("FIVEW_RELEASE_KEY {}: {e}", Path::new(&p).display()))?,
        None => SIGNING_KEY.to_string(),
    };
    let home = tmp.join("gnupg");
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&home)
        .map_err(|e| e.to_string())?;
    let key_file = tmp.join("key.asc");
    fs::write(&key_file, key).map_err(|e| e.to_string())?;
    let gpg = |args: &[&std::ffi::OsStr]| {
        std::process::Command::new("gpg")
            .env("GNUPGHOME", &home)
            .args(["--batch", "--no-tty", "--status-fd", "1"])
            .args(args)
            .output()
            .map_err(|e| format!("gpg: {e}"))
    };
    let imported = gpg(&["--import".as_ref(), key_file.as_os_str()])?;
    if !imported.status.success() {
        bail!("cannot import the release key into gpg");
    }
    let o = gpg(&["--verify".as_ref(), sig.as_os_str(), sums.as_os_str()])?;
    let status = String::from_utf8_lossy(&o.stdout);
    let good = o.status.success()
        && status.lines().any(|l| l.starts_with("[GNUPG:] VALIDSIG "))
        && !status.lines().any(|l| l.starts_with("[GNUPG:] REVKEYSIG "));
    if !good {
        bail!("SHA256SUMS is not signed by the 5W release key");
    }
    Ok(())
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// SHA-256 (FIPS 180-4), so checking a download needs no sha256sum.
fn sha256(data: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&((data.len() as u64).wrapping_mul(8)).to_be_bytes());
    for block in msg.as_chunks::<64>().0 {
        let mut w = [0u32; 64];
        for (i, c) in block.as_chunks::<4>().0.iter().enumerate() {
            w[i] = u32::from_be_bytes([c[0], c[1], c[2], c[3]]);
        }
        for i in 16..64 {
            w[i] = w[i - 16]
                .wrapping_add(
                    w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3),
                )
                .wrapping_add(w[i - 7])
                .wrapping_add(
                    w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10),
                );
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let t1 = hh
                .wrapping_add(e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25))
                .wrapping_add((e & f) ^ (!e & g))
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let t2 = (a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22))
                .wrapping_add((a & b) ^ (a & c) ^ (b & c));
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (x, y) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *x = x.wrapping_add(y);
        }
    }
    let mut out = [0u8; 32];
    for (i, x) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&x.to_be_bytes());
    }
    out
}

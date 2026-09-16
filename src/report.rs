//! `5w report` — feedback about 5W itself, usually from the agents using it.
//!
//! A report is written locally first and sent only when someone says so: it can
//! carry task text, branch names and error messages from a private repository,
//! so nothing leaves the machine by default. 5W keeps its most recent failure,
//! and any crash, so the reporter does not have to reconstruct what happened.

use crate::bail;
use crate::git;
use crate::util::Res;
use std::fs;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

pub const USAGE: &str = "\
usage: 5w report <what happened> [--expected <what you expected>] [--no-last]
       5w report list | show <n> | send <n> [--gh | --print] | rm <n>

  Saves a report about 5W itself — a refusal that looks wrong, a crash, a
  confusing message — under .git/5w/reports/. It attaches the last failure 5W
  recorded (command, message, version) unless --no-last. Nothing is sent.

  send opens a prefilled issue in a browser; --gh creates it with the gh CLI;
  --print only prints the URL (what an agent without a browser should use).
  Read the report first: it may quote task text or branch names.

Issues go to https://github.com/mmmeon/5W unless FIVEW_ISSUES names another
owner/repo.";

const DEFAULT_REPO: &str = "mmmeon/5W";

fn dir() -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    let common = git::opt(
        &cwd,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    Some(PathBuf::from(common).join("5w"))
}

fn now() -> (u64, String) {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // UTC, from the epoch, without a date library.
    let days = secs / 86400;
    let (h, m, s) = (secs % 86400 / 3600, secs % 3600 / 60, secs % 60);
    let z = days as i64 + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if mo <= 2 { 1 } else { 0 };
    (secs, format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z"))
}

fn environment() -> String {
    let version = match option_env!("FIVEW_COMMIT") {
        Some(c) => format!("{} ({c})", env!("CARGO_PKG_VERSION")),
        None => env!("CARGO_PKG_VERSION").to_string(),
    };
    let gitv = Command::new("git")
        .arg("--version")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "git unknown".into());
    let who = if std::env::var_os("FIVEW_AGENT").is_some() || !std::io::stdin().is_terminal() {
        "non-interactive (likely an agent)"
    } else {
        "interactive"
    };
    format!(
        "- 5w {version}\n- {} {}\n- {gitv}\n- session: {who}\n",
        std::env::consts::OS,
        std::env::consts::ARCH
    )
}

/// Record a failure for a later report. Best effort: never fails the command.
pub fn record_failure(args: &[String], message: &str) {
    let Some(d) = dir() else { return };
    if fs::create_dir_all(&d).is_err() {
        return;
    }
    let (_, stamp) = now();
    let quoted: Vec<String> = args.iter().map(|a| shell_quote(a)).collect();
    let body = format!(
        "When: {stamp}\n\nCommand:\n\n```\n5w {}\n```\n\nMessage:\n\n```\n{}\n```\n",
        quoted.join(" "),
        message.trim_end()
    );
    let _ = fs::write(d.join("last-failure.md"), body);
}

/// Install a panic hook that records the crash and says how to report it.
pub fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let args: Vec<String> = std::env::args().skip(1).collect();
        record_failure(&args, &format!("panic: {info}"));
        default(info);
        eprintln!(
            "5w crashed — this is a bug. `5w report \"<what you were doing>\"` saves it with the crash attached."
        );
    }));
}

fn shell_quote(a: &str) -> String {
    if !a.is_empty()
        && a.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./:@#=,+".contains(c))
    {
        a.to_string()
    } else {
        format!("'{}'", a.replace('\'', "'\\''"))
    }
}

fn reports(d: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = fs::read_dir(d.join("reports"))
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x == "md"))
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

fn pick(d: &Path, n: Option<&String>) -> Res<PathBuf> {
    let all = reports(d);
    let n: usize = n
        .ok_or("which report? `5w report list`")?
        .trim_start_matches('#')
        .parse()
        .map_err(|_| "a report number, from `5w report list`".to_string())?;
    all.get(n.wrapping_sub(1))
        .cloned()
        .ok_or_else(|| format!("no report {n}"))
}

fn title_of(text: &str) -> String {
    text.lines()
        .next()
        .unwrap_or("")
        .trim_start_matches("# ")
        .to_string()
}

fn url_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

pub fn run(args: &[String]) -> Res<()> {
    let Some(d) = dir() else {
        bail!("5w report keeps reports in the repository's .git — run it inside one")
    };
    match args.first().map(|s| s.as_str()) {
        None | Some("-h" | "--help" | "help") => {
            println!("{USAGE}");
            Ok(())
        }
        Some("list" | "ls") => {
            let all = reports(&d);
            if all.is_empty() {
                println!("(no reports)");
            }
            for (i, p) in all.iter().enumerate() {
                let text = fs::read_to_string(p).unwrap_or_default();
                let sent = if text.contains("\nSent: ") {
                    " (sent)"
                } else {
                    ""
                };
                println!("{} {}{sent}", i + 1, title_of(&text));
            }
            Ok(())
        }
        Some("show") => {
            print!(
                "{}",
                fs::read_to_string(pick(&d, args.get(1))?).map_err(|e| e.to_string())?
            );
            Ok(())
        }
        Some("rm") => {
            let p = pick(&d, args.get(1))?;
            fs::remove_file(&p).map_err(|e| e.to_string())?;
            println!("removed {}", p.display());
            Ok(())
        }
        Some("send") => send(&d, &args[1..]),
        Some(_) => create(&d, args),
    }
}

fn create(d: &Path, args: &[String]) -> Res<()> {
    let mut what = Vec::new();
    let mut expected = None;
    let mut attach_last = true;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--expected" => {
                expected = Some(args.get(i + 1).ok_or("--expected needs text")?.clone());
                i += 1;
            }
            "--no-last" => attach_last = false,
            a => what.push(a.to_string()),
        }
        i += 1;
    }
    let what = what.join(" ");
    if what.trim().is_empty() {
        bail!("say what happened: 5w report \"<what happened>\"");
    }
    let (secs, stamp) = now();
    let title = crate::util::truncate(what.lines().next().unwrap_or(""), 80);
    let mut body = format!("# {title}\n\n## What happened\n\n{what}\n");
    if let Some(e) = &expected {
        body += &format!("\n## Expected\n\n{e}\n");
    }
    let last = d.join("last-failure.md");
    if attach_last && let Ok(l) = fs::read_to_string(&last) {
        body += &format!("\n## Last failure 5w recorded\n\n{l}");
    }
    body += &format!("\n## Environment\n\n{}- reported: {stamp}\n", environment());
    let rdir = d.join("reports");
    fs::create_dir_all(&rdir).map_err(|e| e.to_string())?;
    // Named to sort by time and never collide: two agents can report in one second.
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let mut path = rdir.join(format!("{secs}-{nanos:09}.md"));
    let mut k = 1;
    while path.exists() {
        path = rdir.join(format!("{secs}-{nanos:09}-{k}.md"));
        k += 1;
    }
    fs::write(&path, &body).map_err(|e| e.to_string())?;
    let n = reports(d)
        .iter()
        .position(|p| *p == path)
        .map(|i| i + 1)
        .unwrap_or(0);
    println!("saved report {n}: {}", path.display());
    println!(
        "nothing was sent. Read it (`5w report show {n}`) — it may quote task text — then `5w report send {n}`."
    );
    Ok(())
}

fn send(d: &Path, args: &[String]) -> Res<()> {
    let path = pick(d, args.first())?;
    let text = fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let repo = std::env::var("FIVEW_ISSUES").unwrap_or_else(|_| DEFAULT_REPO.to_string());
    let title = title_of(&text);
    let body = text
        .split_once('\n')
        .map(|(_, b)| b.trim_start())
        .unwrap_or("");
    let mode = args.get(1).map(|s| s.as_str());
    let url = format!(
        "https://github.com/{repo}/issues/new?labels=report&title={}&body={}",
        url_encode(&title),
        url_encode(body)
    );
    let sent = match mode {
        Some("--gh") => {
            let o = Command::new("gh")
                .args([
                    "issue", "create", "--repo", &repo, "--title", &title, "--body", body,
                ])
                .output()
                .map_err(|e| format!("cannot run gh: {e}"))?;
            if !o.status.success() {
                bail!(
                    "gh issue create failed: {}",
                    String::from_utf8_lossy(&o.stderr).trim()
                );
            }
            let link = String::from_utf8_lossy(&o.stdout).trim().to_string();
            println!("created {link}");
            link
        }
        Some("--print") => {
            println!("{url}");
            return Ok(());
        }
        None => {
            let opener = if cfg!(target_os = "macos") {
                "open"
            } else {
                "xdg-open"
            };
            let opened = std::io::stdout().is_terminal()
                && Command::new(opener)
                    .arg(&url)
                    .status()
                    .map(|s| s.success())
                    .unwrap_or(false);
            if !opened {
                println!("{url}");
                println!(
                    "(open that URL to file the issue; or `5w report send {} --gh`)",
                    args[0]
                );
                return Ok(());
            }
            println!("opened a prefilled issue for {repo} — submit it in the browser");
            url
        }
        Some(m) => bail!("unknown send mode {m} (--gh or --print)"),
    };
    let (_, stamp) = now();
    let _ = fs::write(&path, format!("{text}\nSent: {stamp} {sent}\n"));
    Ok(())
}

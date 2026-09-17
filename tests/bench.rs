//! What 5w's output costs a reader: bytes, estimated tokens and wall time per
//! read command, on generated queues of 10, 100 and 1000 tasks. Bytes and
//! tokens are compared with `tests/bench.baseline`; time is only reported.
//! README.md, *Measuring output*, has the reasoning and the knobs:
//!
//! - `FIVEW_BENCH_UPDATE=1` rewrites the baseline (a deliberate, reviewed change)
//! - `FIVEW_BENCH_TOLERANCE=<percent>` loosens the check (default 2)
//! - `FIVEW_TOKENIZER=<command>` adds an exact count: text on stdin, a number out
//! - `FIVEW_TEST_BIN` measures a release artifact instead of the cargo build

#[path = "../src/tokens.rs"]
mod tokens;

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

const SIZES: [usize; 3] = [10, 100, 1000];
const BASELINE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/bench.baseline");
/// Growth under this many bytes or tokens never fails, whatever the percentage.
const FLOOR: usize = 2;

fn bin5w() -> String {
    std::env::var("FIVEW_TEST_BIN").unwrap_or_else(|_| env!("CARGO_BIN_EXE_5w").to_string())
}

/// Same environment for every run, and fixed dates, so every sha — and so every
/// byte of output — is the same on every machine.
fn env(c: &mut Command, root: &Path) {
    c.env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .env("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z")
        .env("NO_COLOR", "1")
        .env("FIVEW_WT_ROOT", root.join("wt"))
        .env_remove("FIVEW_AGENT")
        .env_remove("FIVEW_ISSUES");
}

fn run(root: &Path, cwd: &Path, prog: &str, args: &[&str]) -> (bool, String, u128) {
    let mut c = Command::new(prog);
    c.args(args).current_dir(cwd).stdin(Stdio::null());
    env(&mut c, root);
    let t = Instant::now();
    let o = c.output().unwrap();
    let us = t.elapsed().as_micros();
    let mut out = String::from_utf8_lossy(&o.stdout).into_owned();
    out.push_str(&String::from_utf8_lossy(&o.stderr));
    (o.status.success(), out, us)
}

fn git(root: &Path, cwd: &Path, args: &[&str]) -> String {
    let (ok, out, _) = run(root, cwd, "git", args);
    assert!(ok, "git {args:?}: {out}");
    out.trim_end().to_string()
}

fn five(root: &Path, cwd: &Path, args: &[&str]) {
    let (ok, out, _) = run(root, cwd, &bin5w(), args);
    assert!(ok, "5w {args:?}: {out}");
}

const AREAS: [&str; 5] = ["api", "cli", "docs", "parser", "store"];
const VERBS: [&str; 6] = ["Decode", "Validate", "Document", "Cache", "Retry", "Split"];
const NOUNS: [&str; 7] = [
    "the header",
    "each record",
    "the index file",
    "a stale lock",
    "the config loader",
    "every archived row",
    "the export format",
];
const WHY: [&str; 4] = [
    "so a partial write is never read back",
    "before the next release",
    "when the input is empty",
    "and report what was skipped",
];

/// One generated queue, in its own scratch repository, and the ids the cases use.
struct Queue {
    repo: PathBuf,
    first: usize,
}

/// `n` tasks in the queue and `n / 2` closed and archived: open, submitted and
/// blocked tasks, bodies, rework notes and every lane kind, in fixed proportions.
fn generate(root: &Path, n: usize) -> Queue {
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(root, &repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("README.md"), "bench\n").unwrap();
    git(root, &repo, &["add", "README.md"]);
    git(root, &repo, &["commit", "-qm", "init"]);
    five(root, &repo, &["init"]);
    // The work every submitted task points at: one commit on a side branch.
    git(root, &repo, &["checkout", "-qb", "bench/work"]);
    std::fs::write(repo.join("work.txt"), "one\ntwo\nthree\n").unwrap();
    git(root, &repo, &["add", "work.txt"]);
    git(root, &repo, &["commit", "-qm", "work"]);
    git(root, &repo, &["checkout", "-q", "main"]);
    let sha = git(root, &repo, &["rev-parse", "bench/work"]);

    let archived = n / 2;
    let first = archived + 1;
    let mut open = String::new();
    let mut done = String::new();
    let mut refs = String::new();
    for id in 1..first + n {
        let j = id.wrapping_sub(first);
        let area = AREAS[id % AREAS.len()];
        let title = format!(
            "{} {} {}",
            VERBS[id % VERBS.len()],
            NOUNS[id % NOUNS.len()],
            WHY[id % WHY.len()]
        );
        let level = 1 + id % 4;
        if id < first {
            let _ = writeln!(
                done,
                "- [x] #{id} {title}  @{area} !{level} branch:{area}/task-{id} via:review submitted:{sha} reviewed:{sha}"
            );
            continue;
        }
        let lane = match () {
            _ if j % 11 == 5 => " >manual",
            _ if j % 13 == 6 => " >owner",
            _ if j % 7 == 3 => " >restricted",
            _ => "",
        };
        let mut fields = format!("@{area} !{level}{lane}");
        if j % 4 == 1 {
            let _ = write!(fields, " needs:#{}", id - 1);
        } else if j % 8 == 2 {
            let _ = write!(fields, " needs:#{}", 1 + j % archived);
        }
        if j % 17 == 8 {
            fields.push_str(" rework:\"the parser drops the | case\"");
        }
        let mark = if j % 10 == 9 {
            let _ = write!(fields, " branch:{area}/task-{id} submitted:{sha}");
            let _ = writeln!(refs, "create refs/heads/{area}/task-{id} {sha}");
            "~"
        } else {
            " "
        };
        let _ = writeln!(open, "- [{mark}] #{id} {title}  {fields}");
        if j % 3 == 0 {
            let _ = writeln!(
                open,
                "  Fixtures for {} live under tests/{area}/.\n  Keep the change inside @{area}; anything wider is a new task.",
                NOUNS[j % NOUNS.len()]
            );
        }
    }
    let tasks = std::fs::read_to_string(repo.join("TASKS.md")).unwrap();
    let tasks = tasks
        .replace("## Open\n", &format!("## Open\n\n{open}"))
        .replace("## Done", &format!("## Done\n\n{done}"));
    std::fs::write(repo.join("TASKS.md"), tasks).unwrap();
    git(root, &repo, &["add", "TASKS.md"]);
    git(
        root,
        &repo,
        &["commit", "-qm", "chore(tasks): generated queue"],
    );
    let mut c = Command::new("git");
    c.args(["update-ref", "--stdin"])
        .current_dir(&repo)
        .stdin(Stdio::piped());
    env(&mut c, root);
    let mut child = c.spawn().unwrap();
    std::io::Write::write_all(&mut child.stdin.take().unwrap(), refs.as_bytes()).unwrap();
    assert!(child.wait().unwrap().success());
    five(root, &repo, &["archive"]);
    // A queue commit made by 5w, for `lint` to judge.
    git(root, &repo, &["branch", "bench/submit", "bench/work"]);
    five(
        root,
        &repo,
        &["submit", &(first + 2).to_string(), "bench/submit"],
    );
    Queue { repo, first }
}

/// The measured cases: every read command off a terminal, with and without
/// `--json` where it takes it, and the refusals an agent commonly meets.
fn cases(q: &Queue) -> Vec<(String, Vec<String>, bool)> {
    let id = |j: usize| (q.first + j).to_string();
    let read = |s: &str| (s.to_string(), split(s), true);
    let mut v = Vec::new();
    for c in ["ready", "next", "ls", "all", "review"] {
        v.push(read(c));
        v.push(read(&format!("{c} --json")));
    }
    let with = |label: &str, cmd: String, ok: bool| (label.to_string(), split(&cmd), ok);
    v.push(with("show <id>", format!("show {}", id(0)), true));
    v.push(with(
        "show <id> --json",
        format!("show {} --json", id(0)),
        true,
    ));
    v.push(with("delegate <id>", format!("delegate {}", id(0)), true));
    v.push(read("doctor"));
    v.push(read("lint HEAD"));
    v.push(with("refuse: show <missing>", "show 99999".into(), false));
    v.push(with(
        "refuse: delegate <manual>",
        format!("delegate {}", id(5)),
        false,
    ));
    v.push(with(
        "refuse: accept <open>",
        format!("accept {}", id(0)),
        false,
    ));
    v.push(with(
        "refuse: done <id> (no flag)",
        format!("done {}", id(0)),
        false,
    ));
    v
}

fn split(s: &str) -> Vec<String> {
    s.split(' ').map(String::from).collect()
}

struct Row {
    size: usize,
    case: String,
    bytes: usize,
    tokens: usize,
    exact: Option<usize>,
    us: u128,
}

fn exact_tokens(text: &str) -> Option<usize> {
    let cmd = std::env::var("FIVEW_TOKENIZER").ok()?;
    let mut child = Command::new("sh")
        .args(["-c", &cmd])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    std::io::Write::write_all(&mut child.stdin.take().unwrap(), text.as_bytes()).unwrap();
    let o = child.wait_with_output().unwrap();
    let n = String::from_utf8_lossy(&o.stdout).trim().parse().ok();
    assert!(
        n.is_some(),
        "FIVEW_TOKENIZER must print a count: {:?}",
        String::from_utf8_lossy(&o.stdout)
    );
    n
}

fn measure() -> Vec<Row> {
    let mut rows = Vec::new();
    for size in SIZES {
        let root = std::env::temp_dir().join(format!("5w-bench-{}-{size}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let q = generate(&root, size);
        for (case, args, want_ok) in cases(&q) {
            let args: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
            let (ok, out, us) = run(&root, &q.repo, &bin5w(), &args);
            assert_eq!(ok, want_ok, "5w {args:?} at {size}:\n{out}");
            // The scratch path is this machine's, not 5w's, and the shas are
            // the generated history's: price fixed ones.
            let out = out.replace(&root.display().to_string(), "/bench");
            let out = unsha(&out);
            rows.push(Row {
                size,
                case,
                bytes: out.len(),
                tokens: tokens::estimate(&out),
                exact: exact_tokens(&out),
                us,
            });
        }
        let _ = std::fs::remove_dir_all(&root);
    }
    rows
}

/// A fixed stand-in for a commit sha, cut to the sha's length: a hex string
/// whose letters and digits alternate as irregularly as a real sha's do.
const SHA: &str = "3f9a0c2e71b4d58f6a0e93c17d2b84f5e0a6c39d18b7f2e4a5c0d96b3e71f804";

/// `text` with every commit sha replaced by [`SHA`] cut to the same length. A
/// sha is a word of 7 to 64 lowercase hex characters holding at least one
/// digit and one letter, so ids, dates, counts and words stay. Shas change
/// with any unrelated commit (a template `init` commits, say), and a sha's
/// estimated tokens change with its digits; bytes are unchanged.
fn unsha(text: &str) -> String {
    let word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let b = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < b.len() {
        if !word(b[i]) {
            out.push(text[i..].chars().next().unwrap());
            i += text[i..].chars().next().unwrap().len_utf8();
            continue;
        }
        let end = (i..b.len()).find(|&j| !word(b[j])).unwrap_or(b.len());
        let w = &text[i..end];
        let hex = w.bytes().all(|x| matches!(x, b'0'..=b'9' | b'a'..=b'f'));
        let sha = (7..=SHA.len()).contains(&w.len())
            && hex
            && w.bytes().any(|x| x.is_ascii_digit())
            && w.bytes().any(|x| x.is_ascii_alphabetic());
        out.push_str(if sha { &SHA[..w.len()] } else { w });
        i = end;
    }
    out
}

fn table(rows: &[Row]) -> String {
    let mut s = String::from("tasks  case                          bytes  ~tokens  exact     ms\n");
    for r in rows {
        let exact = r.exact.map_or("-".to_string(), |n| n.to_string());
        let _ = writeln!(
            s,
            "{:>5}  {:<28} {:>6} {:>8} {:>6} {:>6.1}",
            r.size,
            r.case,
            r.bytes,
            r.tokens,
            exact,
            r.us as f64 / 1000.0
        );
    }
    s
}

fn baseline_text(rows: &[Row]) -> String {
    let mut s = String::from(
        "# 5w output cost: tasks, case, bytes, estimated tokens. Tab-separated.\n\
         # Checked by tests/bench.rs; FIVEW_BENCH_UPDATE=1 cargo test --test bench rewrites it.\n",
    );
    for r in rows {
        let _ = writeln!(s, "{}\t{}\t{}\t{}", r.size, r.case, r.bytes, r.tokens);
    }
    s
}

/// Rows that grew past the tolerance, and rows missing on either side.
fn compare(rows: &[Row], baseline: &str, pct: usize) -> (Vec<String>, Vec<String>) {
    let mut fail = Vec::new();
    let mut note = Vec::new();
    let mut base = std::collections::BTreeMap::new();
    for l in baseline
        .lines()
        .filter(|l| !l.starts_with('#') && !l.is_empty())
    {
        let f: Vec<&str> = l.split('\t').collect();
        let [size, case, bytes, tokens] = f[..] else {
            fail.push(format!("baseline line not understood: {l:?}"));
            continue;
        };
        let n = |s: &str| s.parse::<usize>().unwrap_or(0);
        base.insert((n(size), case.to_string()), (n(bytes), n(tokens)));
    }
    for r in rows {
        let Some((b, t)) = base.remove(&(r.size, r.case.clone())) else {
            fail.push(format!("{} {:?}: not in the baseline", r.size, r.case));
            continue;
        };
        for (unit, was, now) in [("bytes", b, r.bytes), ("tokens", t, r.tokens)] {
            let slack = (was * pct / 100).max(FLOOR);
            let line = format!("{} {:?}: {was} → {now} {unit}", r.size, r.case);
            if now > was + slack {
                fail.push(line);
            } else if now + slack < was {
                note.push(line);
            }
        }
    }
    for (size, case) in base.keys() {
        fail.push(format!(
            "{size} {case:?}: in the baseline, no longer measured"
        ));
    }
    (fail, note)
}

#[test]
fn output_cost_stays_within_the_baseline() {
    let rows = measure();
    println!("{}", table(&rows));
    if std::env::var("FIVEW_BENCH_UPDATE").is_ok_and(|v| v == "1") {
        std::fs::write(BASELINE, baseline_text(&rows)).unwrap();
        println!("wrote {BASELINE}");
        return;
    }
    let pct = std::env::var("FIVEW_BENCH_TOLERANCE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(2);
    let baseline = std::fs::read_to_string(BASELINE).unwrap_or_default();
    let (fail, note) = compare(&rows, &baseline, pct);
    for n in &note {
        println!("smaller than the baseline: {n}");
    }
    assert!(
        fail.is_empty(),
        "output cost differs from tests/bench.baseline (tolerance {pct}%):\n  {}\n\
         if the change is intended: FIVEW_BENCH_UPDATE=1 cargo test --test bench",
        fail.join("\n  ")
    );
}

#[test]
fn shas_are_priced_the_same_whatever_their_digits() {
    let a = r#"{"id":8,"tip":"3916130eab88379b3c0d0e1f2a3b4c5d6e7f8091","behind":3} submitted:230785eab883 #1024 2026-01-01 deadbeefcafe 1234567 v0.1.3"#;
    let b = r#"{"id":8,"tip":"ffe1d2c3b4a5968778695a4b3c2d1e0f00112233","behind":3} submitted:9a8b7c6d5e4f #1024 2026-01-01 deadbeefcafe 1234567 v0.1.3"#;
    assert_ne!(tokens::estimate(a), tokens::estimate(b));
    let (na, nb) = (unsha(a), unsha(b));
    assert_eq!(na, nb);
    assert_eq!(na.len(), a.len());
    assert!(na.contains(r#""tip":"3f9a0c2e71b4d58f6a0e93c17d2b84f5e0a6c39d""#));
    assert!(na.contains("submitted:3f9a0c2e71b4 #1024 2026-01-01 deadbeefcafe 1234567 v0.1.3"));
    assert_eq!(unsha("héllo abc1234 — x"), "héllo 3f9a0c2 — x");
}

#[test]
fn the_check_fails_on_growth_and_on_a_changed_case_set() {
    let row = |case: &str, bytes, tokens| Row {
        size: 10,
        case: case.into(),
        bytes,
        tokens,
        exact: None,
        us: 0,
    };
    let base = "# comment\n10\tready\t1000\t250\n10\tgone\t5\t1\n";
    let rows = [row("ready", 1020, 250), row("new", 1, 1)];
    let (fail, note) = compare(&rows, base, 2);
    assert!(!fail.iter().any(|f| f.contains("\"ready\"")));
    assert!(
        fail.iter()
            .any(|f| f.contains("\"new\": not in the baseline"))
    );
    assert!(fail.iter().any(|f| f.contains("\"gone\": in the baseline")));
    assert!(note.is_empty());
    let (fail, _) = compare(&[row("ready", 1021, 250)], base, 2);
    assert!(fail.iter().any(|f| f.contains("1000 → 1021 bytes")));
    let (_, note) = compare(&[row("ready", 900, 250)], base, 2);
    assert!(note.iter().any(|f| f.contains("1000 → 900 bytes")));
}

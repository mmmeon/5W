//! `5w audit` — how a repository has used 5W, from what 5W already records.
//!
//! The queue's history is the record: every state change is a commit on the
//! trunk. One `git log -p -U0` over the queue and its archive, along the trunk's
//! first-parent line, rebuilds both files at every commit by applying the hunks;
//! the rows a hunk touched are compared by id with the row as it was, which gives
//! each transition and its date without running git per commit. Fences are
//! respected as `parse` respects them. Read-only: nothing is written anywhere.
//!
//! The sections and why each exists are in the README (*Auditing how a
//! repository uses 5W*), with the one that was cut and why.

use crate::bail;
use crate::git;
use crate::queue::{self, State, Task};
use crate::report;
use crate::store::{self, Repo};
use crate::tasks::{self, js};
use crate::util::{Res, Sty, short, truncate};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write;

pub const USAGE: &str = "\
usage: 5w audit [--since <rev|YYYY-MM-DD>] [--json] [--full]

  How this repository has used 5W, from the queue's history and 5W's records:
  tasks by lane, level and area; submit→accept time; rework and its reasons;
  reopened tasks; time blocked on needs; queue edits made outside 5w, with
  lint's findings; doctor; recorded failures and reports; the cost of each
  open task's brief. Reads only. Sections: README, *Auditing how a repository
  uses 5W*.

  --since   only what happened after a commit (<rev>..trunk) or a date (UTC)
  --full    longer lists (the default on a terminal)
  --json    everything, one object";

// --- time ---------------------------------------------------------------------------------

/// Days since the epoch for a civil date (proleptic Gregorian).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn date(secs: i64) -> String {
    let z = secs.div_euclid(86400) + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!("{y:04}-{m:02}-{d:02}")
}

/// `YYYY-MM-DD`, optionally followed by `THH:MM:SS` and a `Z`, as UTC seconds.
fn parse_date(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    let num = |r: std::ops::Range<usize>| -> Option<i64> {
        let t = s.get(r)?;
        t.bytes()
            .all(|c| c.is_ascii_digit())
            .then(|| t.parse().ok())?
    };
    if b.len() < 10 || b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    let (y, m, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let mut secs = days_from_civil(y, m, d) * 86400;
    if b.len() > 10 {
        if b[10] != b'T' || b.len() < 19 || b[13] != b':' || b[16] != b':' {
            return None;
        }
        secs += num(11..13)? * 3600 + num(14..16)? * 60 + num(17..19)?;
        if !matches!(&s[19..], "" | "Z") {
            return None;
        }
    }
    Some(secs)
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// `45s`, `12m`, `3h05m`, `2d04h`.
fn dur(s: i64) -> String {
    let s = s.max(0);
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m", s / 60),
        3600..86400 => format!("{}h{:02}m", s / 3600, s % 3600 / 60),
        _ => format!("{}d{:02}h", s / 86400, s % 86400 / 3600),
    }
}

// --- replay -------------------------------------------------------------------------------

/// A file as of some commit, and where its fence lines are.
#[derive(Default)]
struct File {
    lines: Vec<String>,
    fences: Vec<usize>,
}

impl File {
    fn refence(&mut self) {
        self.fences = (0..self.lines.len())
            .filter(|&i| self.lines[i].starts_with("```"))
            .collect();
    }
    fn fenced(&self, i: usize) -> bool {
        self.fences.partition_point(|&f| f < i) % 2 == 1
    }
    fn text(&self) -> String {
        if self.lines.is_empty() {
            return String::new();
        }
        self.lines.join("\n") + "\n"
    }
}

struct Hunk {
    old_start: usize,
    old_count: usize,
    new_start: usize,
    new_count: usize,
    removed: Vec<String>,
    added: Vec<String>,
}

fn range(s: &str) -> Option<(usize, usize)> {
    match s.split_once(',') {
        Some((a, b)) => Some((a.parse().ok()?, b.parse().ok()?)),
        None => Some((s.parse().ok()?, 1)),
    }
}

/// `@@ -a,b +c,d @@`
fn hunk_header(l: &str) -> Option<Hunk> {
    let mut w = l.strip_prefix("@@ ")?.split(' ');
    let (old_start, old_count) = range(w.next()?.strip_prefix('-')?)?;
    let (new_start, new_count) = range(w.next()?.strip_prefix('+')?)?;
    Some(Hunk {
        old_start,
        old_count,
        new_start,
        new_count,
        removed: Vec::new(),
        added: Vec::new(),
    })
}

/// One commit that touched the queue.
struct Commit {
    sha: String,
    time: i64,
    subject: String,
    /// Hunks per file: 0 the queue, 1 the archive.
    hunks: [Vec<Hunk>; 2],
}

/// Starts each commit's header line in the log. Every line of a patch starts
/// with a character git chose (`+`, `-`, ` `, `\`, `d`, `i`, `@`…), so no queue
/// text can forge it — a NUL can, and queues hold NULs.
const MARK: &str = "\x01\x025w ";

fn parse_log(out: &str, names: [&str; 2]) -> Vec<Commit> {
    let heads = names.map(|n| format!("diff --git a/{n} b/{n}"));
    let mut commits = Vec::new();
    let sep = format!("\n{MARK}");
    let mut chunks = out.split(sep.as_str());
    let first = chunks.next().and_then(|c| c.strip_prefix(MARK));
    for chunk in first.into_iter().chain(chunks) {
        let mut lines = chunk.split('\n');
        let header = lines.next().unwrap_or("");
        let mut hw = header.splitn(3, ' ');
        let sha = hw.next().unwrap_or("").to_string();
        let time = hw.next().and_then(|t| t.parse().ok()).unwrap_or(0);
        let subject = hw.next().unwrap_or("").to_string();
        let mut c = Commit {
            sha,
            time,
            subject,
            hunks: [Vec::new(), Vec::new()],
        };
        let mut file: Option<usize> = None;
        while let Some(l) = lines.next() {
            if l.starts_with("diff --git ") {
                file = heads.iter().position(|h| h == l);
                continue;
            }
            let (Some(f), Some(mut h)) = (file, hunk_header(l)) else {
                continue;
            };
            // Counted, not sniffed: a queue line may begin with anything.
            let (mut old_left, mut new_left) = (h.old_count, h.new_count);
            while old_left + new_left > 0 {
                let Some(x) = lines.next() else { break };
                if let Some(t) = x.strip_prefix('-')
                    && old_left > 0
                {
                    h.removed.push(t.to_string());
                    old_left -= 1;
                } else if let Some(t) = x.strip_prefix('+')
                    && new_left > 0
                {
                    h.added.push(t.to_string());
                    new_left -= 1;
                } else if x.starts_with('\\') {
                    continue;
                } else {
                    break;
                }
            }
            c.hunks[f].push(h);
        }
        commits.push(c);
    }
    commits
}

/// Apply a commit's hunks to a file. False when the file did not match them —
/// history the log does not show, such as a merge.
fn apply(file: &mut File, hunks: &[Hunk]) -> bool {
    let old = std::mem::take(&mut file.lines);
    let len = old.len();
    let mut out = Vec::with_capacity(len);
    let mut it = old.into_iter();
    let mut pos = 0;
    let mut ok = true;
    for h in hunks {
        let start = if h.old_count == 0 {
            h.old_start
        } else {
            h.old_start.saturating_sub(1)
        };
        if start < pos || start > len {
            ok = false;
            continue;
        }
        while pos < start {
            out.extend(it.next());
            pos += 1;
        }
        for r in &h.removed {
            match it.next() {
                Some(l) if l == *r => {}
                _ => ok = false,
            }
            pos += 1;
        }
        out.extend(h.added.iter().cloned());
    }
    out.extend(it);
    file.lines = out;
    ok
}

fn row(line: &str) -> Option<Task> {
    queue::head(line.strip_suffix('\r').unwrap_or(line))?;
    queue::parse(line).into_iter().next()
}

/// The rows a commit changed in one file, before and after, fences respected.
fn changed(file: &mut File, hunks: &[Hunk], before: &mut Vec<Task>, after: &mut Vec<Task>) -> bool {
    let touches_fence = hunks
        .iter()
        .flat_map(|h| h.removed.iter().chain(&h.added))
        .any(|l| l.starts_with("```"));
    if touches_fence {
        // Which lines are rows may have changed anywhere: compare whole files.
        let old = queue::parse(&file.text());
        let ok = apply(file, hunks);
        file.refence();
        let new = queue::parse(&file.text());
        let key = |t: &Task| {
            format!(
                "{:?}",
                (
                    t.state,
                    &t.text,
                    &t.via,
                    &t.rework,
                    &t.needs,
                    &t.submitted,
                    &t.lane,
                    &t.level,
                    &t.area,
                    &t.branch,
                    &t.reviewed
                )
            )
        };
        let old_keys: HashMap<u64, String> = old.iter().map(|t| (t.id, key(t))).collect();
        let new_keys: HashMap<u64, String> = new.iter().map(|t| (t.id, key(t))).collect();
        before.extend(
            old.into_iter()
                .filter(|t| new_keys.get(&t.id) != Some(&old_keys[&t.id])),
        );
        after.extend(
            new.into_iter()
                .filter(|t| old_keys.get(&t.id) != Some(&new_keys[&t.id])),
        );
        return ok;
    }
    for h in hunks {
        let start = h.old_start.saturating_sub(1);
        for (k, l) in h.removed.iter().enumerate() {
            if !file.fenced(start + k)
                && let Some(t) = row(l)
            {
                before.push(t);
            }
        }
    }
    let ok = apply(file, hunks);
    file.refence();
    for h in hunks {
        let start = h.new_start.saturating_sub(1);
        for (k, l) in h.added.iter().enumerate() {
            if !file.fenced(start + k)
                && let Some(t) = row(l)
            {
                after.push(t);
            }
        }
    }
    ok
}

// --- what the replay collects ------------------------------------------------------------------

/// A queue commit 5w did not make: (sha, time, subject, why, lint findings).
type Outside = (String, i64, String, Vec<String>, Vec<String>);

#[derive(Default)]
struct Found {
    commits: usize,
    first: Option<i64>,
    last: Option<i64>,
    /// Tasks with an event in the window.
    touched: HashSet<u64>,
    /// (id, seconds from the submit before it)
    accepts: Vec<(u64, i64)>,
    /// (id, time, reason)
    rejects: Vec<(u64, i64, String)>,
    reopens: Vec<u64>,
    /// (id, seconds, needs, still blocked)
    blocked: Vec<(u64, i64, Vec<u64>, bool)>,
    outside: Vec<Outside>,
    /// The replay disagreed with the trunk's files.
    diverged: bool,
}

enum Since {
    All,
    Commits(HashSet<String>, i64),
    Date(i64),
}

impl Since {
    fn has(&self, sha: &str, time: i64) -> bool {
        match self {
            Since::All => true,
            Since::Commits(set, _) => set.contains(sha),
            Since::Date(t) => time >= *t,
        }
    }
    fn start(&self) -> Option<i64> {
        match self {
            Since::All => None,
            Since::Commits(_, t) | Since::Date(t) => Some(*t),
        }
    }
}

/// Would 5w itself have written this commit? Its messages are fixed (a batch's
/// subject names each edit, see `lint::batch_edits`), and it
/// touches nothing but the queue (and, for `init`, the files it installs).
fn outside_why(repo: &Repo, subject: &str, files: &[String]) -> Vec<String> {
    let cfg = &repo.cfg;
    let mut why = Vec::new();
    let adopt = subject == "chore(tasks): adopt 5W";
    let id = |s: &str| -> bool {
        s.strip_prefix('#')
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
    };
    let count = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let ours = adopt
        || crate::lint::batch_edits(&cfg.commit_prefix, subject).is_some()
        || subject
            .strip_prefix(&format!("{}: ", cfg.commit_prefix))
            .is_some_and(|rest| {
                let w: Vec<&str> = rest.split(' ').collect();
                match w.as_slice() {
                    ["add", n, "—", ..] => id(n),
                    ["set", n, _, ..] => id(n),
                    ["submit", n, "for", "review"] => id(n),
                    ["accept", n] | ["reject", n] | ["reopen", n] => id(n),
                    ["close", n, via] => id(n) && via.starts_with("via:"),
                    ["archive", n, "closed", "tasks"] => count(n),
                    ["split", n, "over-long", "titles"] => count(n),
                    _ => false,
                }
            });
    if !ours {
        why.push("a message 5w never writes".to_string());
    }
    let others: Vec<&String> = files
        .iter()
        .filter(|f| {
            **f != cfg.file
                && **f != cfg.archive
                && !(adopt && (*f == store::CONFIG_FILE || *f == "PROTOCOL.md"))
        })
        .collect();
    if !others.is_empty() {
        let mut s = others
            .iter()
            .take(3)
            .map(|f| f.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        if others.len() > 3 {
            let _ = write!(s, " +{}", others.len() - 3);
        }
        why.push(format!("also touches {s}"));
    }
    why
}

fn replay(repo: &Repo, trunk_ref: &str, since: &Since) -> Res<(Found, HashMap<u64, Task>)> {
    let names = [repo.cfg.file.as_str(), repo.cfg.archive.as_str()];
    let base = [
        "log",
        "--first-parent",
        "--diff-merges=first-parent",
        "--reverse",
        "--no-renames",
        "--no-color",
        "--no-ext-diff",
        // A queue with a NUL in it is still text; a binary diff has no lines.
        "--text",
    ];
    let mut args: Vec<&str> = base.to_vec();
    args.extend([
        "-p",
        "-U0",
        "--format=%x01%x025w %H %ct %s",
        trunk_ref,
        "--",
    ]);
    args.extend(names);
    let log = git::git(&repo.primary, &args)?;
    let mut args: Vec<&str> = base.to_vec();
    args.extend([
        "--full-diff",
        "--name-only",
        "--format=%x01%H",
        trunk_ref,
        "--",
    ]);
    args.extend(names);
    let touched_files = git::git(&repo.primary, &args)?;
    let mut list: Vec<(&str, Vec<String>)> = Vec::new();
    for l in touched_files.lines().filter(|l| !l.is_empty()) {
        match (l.strip_prefix('\x01'), list.last_mut()) {
            (Some(sha), _) => list.push((sha, Vec::new())),
            (None, Some(last)) => last.1.push(l.to_string()),
            (None, None) => {}
        }
    }
    let files: HashMap<&str, Vec<String>> = list.into_iter().collect();

    let default_lane = repo.cfg.default_lane.clone();
    let mut found = Found::default();
    let mut live: HashMap<u64, Task> = HashMap::new();
    let mut submitted_at: HashMap<u64, i64> = HashMap::new();
    let mut blocked_since: HashMap<u64, i64> = HashMap::new();
    let mut with_needs: HashSet<u64> = HashSet::new();
    let mut fs = [File::default(), File::default()];
    let window_start = since.start();

    for c in parse_log(&log, names) {
        let inside = since.has(&c.sha, c.time);
        let no_files = Vec::new();
        let mut why = outside_why(
            repo,
            &c.subject,
            files.get(c.sha.as_str()).unwrap_or(&no_files),
        );
        // A subject that names rows is 5w's only if the commit changes those rows.
        let prefix = &repo.cfg.commit_prefix;
        let names_rows = inside
            && why.is_empty()
            && (crate::lint::batch_edits(prefix, &c.subject).is_some()
                || crate::lint::single_edit(prefix, &c.subject).is_some());
        let lint_old =
            (inside && (names_rows || !why.is_empty())).then(|| [fs[0].text(), fs[1].text()]);

        let (mut before, mut after) = (Vec::new(), Vec::new());
        for (f, hunks) in fs.iter_mut().zip(&c.hunks) {
            if !hunks.is_empty() && !changed(f, hunks, &mut before, &mut after) {
                found.diverged = true;
            }
        }
        if inside {
            found.commits += 1;
            found.first = Some(found.first.map_or(c.time, |f| f.min(c.time)));
            found.last = Some(found.last.map_or(c.time, |l| l.max(c.time)));
        }
        if names_rows
            && let Some([q, a]) = &lint_old
            && crate::lint::subject_rows_texts(
                prefix,
                &c.subject,
                [q, a],
                [&fs[0].text(), &fs[1].text()],
            )
            .is_some()
        {
            why.push("its subject and the rows it changes disagree".to_string());
        }
        if let Some(old) = lint_old.filter(|_| !why.is_empty()) {
            let mut lint = Vec::new();
            crate::lint::check_texts(
                repo,
                old,
                [fs[0].text(), fs[1].text()],
                &c.subject,
                short(&c.sha),
                &mut lint,
            );
            found
                .outside
                .push((c.sha.clone(), c.time, c.subject.clone(), why, lint));
        }

        let after_ids: HashSet<u64> = after.iter().map(|t| t.id).collect();
        let mut ids: Vec<u64> = before.iter().map(|t| t.id).collect();
        ids.retain(|id| !after_ids.contains(id));
        ids.sort_unstable();
        ids.dedup();
        for id in ids {
            // Removed and not moved: a deleted row.
            live.remove(&id);
            with_needs.remove(&id);
            if let Some(s) = blocked_since.remove(&id) {
                let s = window_start.map_or(s, |w| s.max(w));
                if c.time > s && inside {
                    found.blocked.push((id, c.time - s, Vec::new(), false));
                }
            }
        }
        for n in after {
            let id = n.id;
            let prev = live.get(&id).map(|t| t.state);
            let resubmit = prev == Some(State::Review)
                && n.state == State::Review
                && live[&id].submitted != n.submitted;
            if n.state == State::Review && (prev != Some(State::Review) || resubmit) {
                submitted_at.insert(id, c.time);
            }
            if inside {
                match (prev, n.state) {
                    (Some(State::Review), State::Open) => {
                        found
                            .rejects
                            .push((id, c.time, n.rework.clone().unwrap_or_default()))
                    }
                    (Some(State::Review), State::Done) => {
                        if n.via.as_deref() == Some("review")
                            && let Some(s) = submitted_at.get(&id)
                        {
                            found.accepts.push((id, c.time - s));
                        }
                    }
                    (Some(State::Done), State::Open | State::Review) => found.reopens.push(id),
                    _ => {}
                }
                if resubmit || prev != Some(n.state) {
                    found.touched.insert(id);
                }
            }
            if n.needs.is_empty() {
                with_needs.remove(&id);
                if let Some(s) = blocked_since.remove(&id) {
                    let s = window_start.map_or(s, |w| s.max(w));
                    if inside && c.time > s {
                        let needs = live[&id].needs.clone();
                        found.blocked.push((id, c.time - s, needs, false));
                    }
                }
            } else {
                with_needs.insert(id);
            }
            let mut n = n;
            n.lane.get_or_insert_with(|| default_lane.clone());
            live.insert(id, n);
        }
        // Blocked: open with a need not closed. Checked after every commit.
        for &id in &with_needs {
            let t = &live[&id];
            let blocked = t.state == State::Open
                && t.needs
                    .iter()
                    .any(|d| live.get(d).is_none_or(|x| x.state != State::Done));
            match (blocked, blocked_since.get(&id).copied()) {
                (true, None) => {
                    blocked_since.insert(id, c.time);
                }
                (false, Some(s)) => {
                    blocked_since.remove(&id);
                    let s = window_start.map_or(s, |w| s.max(w));
                    if inside && c.time > s {
                        found.blocked.push((id, c.time - s, t.needs.clone(), false));
                    }
                }
                _ => {}
            }
        }
    }
    let t = now();
    for (id, s) in blocked_since {
        let s = window_start.map_or(s, |w| s.max(w));
        let needs = live
            .get(&id)
            .map(|x| {
                x.needs
                    .iter()
                    .copied()
                    .filter(|d| live.get(d).is_none_or(|y| y.state != State::Done))
                    .collect()
            })
            .unwrap_or_default();
        found.blocked.push((id, (t - s).max(0), needs, true));
    }
    // The replay must end where the trunk is; say so when it does not.
    for (f, name) in fs.iter().zip(names) {
        let real = repo.committed_file(name)?.unwrap_or_default();
        if real.trim_end_matches(['\n', '\r']) != f.text().trim_end_matches(['\n', '\r']) {
            found.diverged = true;
        }
    }
    Ok((found, live))
}

// --- the report ---------------------------------------------------------------------------

struct Failure {
    when: String,
    command: String,
    message: String,
}

/// (n, time, title, sent)
type Reports = Vec<(usize, i64, String, bool)>;

fn records(since: &Since) -> (Option<Failure>, Reports) {
    let Some(d) = report::dir() else {
        return (None, Vec::new());
    };
    let start = since.start().unwrap_or(i64::MIN);
    let failure = std::fs::read_to_string(d.join("last-failure.md"))
        .ok()
        .and_then(|text| {
            let when = text
                .lines()
                .find_map(|l| l.strip_prefix("When: "))?
                .to_string();
            if parse_date(&when).is_some_and(|t| t < start) {
                return None;
            }
            // The blocks after "Command:" and "Message:", fences stripped.
            let block = |label: &str| {
                text.split_once(label)
                    .map(|(_, r)| {
                        r.lines()
                            .skip_while(|l| !l.starts_with("```"))
                            .skip(1)
                            .take_while(|l| !l.starts_with("```"))
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .unwrap_or_default()
            };
            Some(Failure {
                when,
                command: block("Command:"),
                message: block("Message:"),
            })
        });
    let reports = report::reports(&d)
        .iter()
        .enumerate()
        .filter_map(|(i, p)| {
            let secs = p
                .file_stem()?
                .to_str()?
                .split('-')
                .next()?
                .parse::<i64>()
                .ok()?;
            let text = std::fs::read_to_string(p).unwrap_or_default();
            (secs >= start).then(|| {
                (
                    i + 1,
                    secs,
                    report::title_of(&text),
                    text.contains("\nSent: "),
                )
            })
        })
        .collect();
    (failure, reports)
}

fn counts<'a>(it: impl Iterator<Item = &'a str>) -> Vec<(String, usize)> {
    let mut m: BTreeMap<&str, usize> = BTreeMap::new();
    for k in it {
        *m.entry(k).or_default() += 1;
    }
    let mut v: Vec<(String, usize)> = m.into_iter().map(|(k, n)| (k.to_string(), n)).collect();
    // "-", the field missing, goes last.
    v.sort_by(|a, b| {
        (a.0 == "-")
            .cmp(&(b.0 == "-"))
            .then(b.1.cmp(&a.1))
            .then(a.0.cmp(&b.0))
    });
    v
}

fn pct(sorted: &[i64], p: usize) -> i64 {
    if sorted.is_empty() {
        return 0;
    }
    let i = (sorted.len() * p).div_ceil(100).max(1) - 1;
    sorted[i.min(sorted.len() - 1)]
}

fn ids(v: &[u64]) -> String {
    v.iter()
        .map(|i| format!("#{i}"))
        .collect::<Vec<_>>()
        .join(",")
}

/// History is read by the names the trunk commits, as lint reads it, and so is
/// the doctor block — which notes an uncommitted edit, as `5w doctor` does.
pub fn run(repo: &Repo, args: &[String]) -> Res<()> {
    let (judged, note) = crate::lint::under_committed_rules(repo);
    crate::lint::hold_note(note);
    crate::lint::noted(run_under(judged.as_ref().unwrap_or(repo), args))
}

fn run_under(repo: &Repo, args: &[String]) -> Res<()> {
    let mut json = false;
    let mut compact = tasks::compact_default();
    let mut since_arg = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--json" => json = true,
            "--full" => compact = false,
            "--since" => {
                since_arg = Some(
                    args.get(i + 1)
                        .ok_or("--since needs a commit or a date (YYYY-MM-DD)")?
                        .clone(),
                );
                i += 1;
            }
            "-h" | "--help" | "help" => {
                println!("{USAGE}");
                return Ok(());
            }
            a => bail!("unexpected {a:?} (5w audit [--since <rev|YYYY-MM-DD>] [--json] [--full])"),
        }
        i += 1;
    }

    let trunk_ref = [
        format!("refs/heads/{}", repo.trunk),
        format!("refs/remotes/origin/{}", repo.trunk),
    ]
    .into_iter()
    .find(|r| git::rev(&repo.primary, r).is_some());

    let since = match &since_arg {
        None => Since::All,
        Some(s) if parse_date(s).is_some() => Since::Date(parse_date(s).unwrap_or(0)),
        Some(s) => {
            let Some(sha) = git::rev(&repo.primary, s) else {
                bail!("--since {s}: not a commit or a date (YYYY-MM-DD)")
            };
            let time = git::git(&repo.primary, &["log", "-1", "--format=%ct", &sha])?
                .parse()
                .unwrap_or(0);
            let set = match &trunk_ref {
                Some(t) => git::git(
                    &repo.primary,
                    &["rev-list", "--first-parent", &format!("{sha}..{t}")],
                )?
                .lines()
                .map(String::from)
                .collect(),
                None => HashSet::new(),
            };
            Since::Commits(set, time)
        }
    };

    let (found, live) = match &trunk_ref {
        Some(t) => replay(repo, t, &since)?,
        None => (Found::default(), HashMap::new()),
    };
    let has_queue = repo.committed()?.is_some();
    let doctor = if has_queue {
        tasks::doctor_findings(repo).ok()
    } else {
        None
    };
    let briefs = if has_queue {
        tasks::brief_costs(repo).unwrap_or_default()
    } else {
        Vec::new()
    };
    let (failure, reports) = records(&since);

    // The population: every task, or those with an event in the window.
    let mut population: Vec<&Task> = live
        .values()
        .filter(|t| matches!(since, Since::All) || found.touched.contains(&t.id))
        .collect();
    population.sort_by_key(|t| t.id);
    let state_n = |s: State| population.iter().filter(|t| t.state == s).count();
    let via = counts(
        population
            .iter()
            .filter(|t| t.state == State::Done)
            .map(|t| t.via.as_deref().unwrap_or("-")),
    );
    let lanes = counts(population.iter().map(|t| t.lane.as_deref().unwrap_or("-")));
    let levels: Vec<(String, usize)> = ["1", "2", "3", "4", "-"]
        .iter()
        .map(|l| {
            let n = population
                .iter()
                .filter(|t| t.level.map(|x| x.to_string()).as_deref().unwrap_or("-") == *l)
                .count();
            (l.to_string(), n)
        })
        .filter(|(_, n)| *n > 0)
        .collect();
    let areas = counts(population.iter().map(|t| t.area.as_deref().unwrap_or("-")));

    let mut review: Vec<(u64, i64)> = found.accepts.clone();
    review.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let mut secs: Vec<i64> = review.iter().map(|r| r.1).collect();
    secs.sort_unstable();

    let mut per_task: BTreeMap<u64, usize> = BTreeMap::new();
    for r in &found.rejects {
        *per_task.entry(r.0).or_default() += 1;
    }
    let cycles = [
        per_task.values().filter(|n| **n == 1).count(),
        per_task.values().filter(|n| **n == 2).count(),
        per_task.values().filter(|n| **n >= 3).count(),
    ];
    let mut reasons = found.rejects.clone();
    reasons.sort_by(|a, b| b.1.cmp(&a.1).then(b.0.cmp(&a.0)));

    let reopened = counts_u64(&found.reopens);

    // Blocked, per task: the total, and whether it still is.
    let mut blocked: BTreeMap<u64, (i64, Vec<u64>, bool)> = BTreeMap::new();
    for (id, s, needs, still) in &found.blocked {
        let e = blocked.entry(*id).or_default();
        e.0 += s;
        if !needs.is_empty() {
            e.1 = needs.clone();
        }
        e.2 |= still;
    }
    for (id, e) in blocked.iter_mut() {
        if e.1.is_empty()
            && let Some(t) = live.get(id)
        {
            e.1 = t.needs.clone();
        }
    }
    let mut blocked: Vec<(u64, i64, Vec<u64>, bool)> = blocked
        .into_iter()
        .map(|(id, (s, n, still))| (id, s, n, still))
        .collect();
    blocked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let mut bsecs: Vec<i64> = blocked.iter().map(|b| b.1).collect();
    bsecs.sort_unstable();
    let still = blocked.iter().filter(|b| b.3).count();

    let lint_n: usize = found.outside.iter().map(|o| o.4.len()).sum();

    if json {
        let pairs = |v: &[(String, usize)]| {
            format!(
                "{{{}}}",
                v.iter()
                    .map(|(k, n)| format!("{}:{n}", js(k)))
                    .collect::<Vec<_>>()
                    .join(",")
            )
        };
        let list = |v: Vec<String>| format!("[{}]", v.join(","));
        let nums = |v: &[u64]| list(v.iter().map(|n| n.to_string()).collect());
        let mut o = String::new();
        let _ = write!(
            o,
            "{{\"trunk\":{},\"since\":{},\"commits\":{},\"first\":{},\"last\":{},\"replay_diverged\":{}",
            js(&repo.trunk),
            since_arg.as_deref().map(js).unwrap_or("null".into()),
            found.commits,
            found.first.map(|t| js(&date(t))).unwrap_or("null".into()),
            found.last.map(|t| js(&date(t))).unwrap_or("null".into()),
            found.diverged
        );
        let _ = write!(
            o,
            ",\"tasks\":{{\"total\":{},\"open\":{},\"review\":{},\"done\":{},\"via\":{},\"lanes\":{},\"levels\":{},\"areas\":{}}}",
            population.len(),
            state_n(State::Open),
            state_n(State::Review),
            state_n(State::Done),
            pairs(&via),
            pairs(&lanes),
            pairs(&levels),
            pairs(&areas)
        );
        let _ = write!(
            o,
            ",\"review\":{{\"accepted\":{},\"median_s\":{},\"p90_s\":{},\"max_s\":{},\"slowest\":{}}}",
            review.len(),
            pct(&secs, 50),
            pct(&secs, 90),
            secs.last().copied().unwrap_or(0),
            list(
                review
                    .iter()
                    .map(|(id, s)| format!("{{\"id\":{id},\"seconds\":{s}}}"))
                    .collect()
            )
        );
        let _ = write!(
            o,
            ",\"rework\":{{\"rejections\":{},\"tasks\":{},\"once\":{},\"twice\":{},\"thrice_or_more\":{},\"reasons\":{}}}",
            found.rejects.len(),
            per_task.len(),
            cycles[0],
            cycles[1],
            cycles[2],
            list(
                reasons
                    .iter()
                    .map(|(id, t, r)| format!(
                        "{{\"id\":{id},\"at\":{},\"reason\":{}}}",
                        js(&date(*t)),
                        js(r)
                    ))
                    .collect()
            )
        );
        let _ = write!(
            o,
            ",\"reopened\":{}",
            list(
                reopened
                    .iter()
                    .map(|(id, n)| format!("{{\"id\":{id},\"times\":{n}}}"))
                    .collect()
            )
        );
        let _ = write!(
            o,
            ",\"blocked\":{{\"tasks\":{},\"median_s\":{},\"max_s\":{},\"still\":{still},\"longest\":{}}}",
            blocked.len(),
            pct(&bsecs, 50),
            bsecs.last().copied().unwrap_or(0),
            list(
                blocked
                    .iter()
                    .map(|(id, s, n, st)| format!(
                        "{{\"id\":{id},\"seconds\":{s},\"needs\":{},\"still\":{st}}}",
                        nums(n)
                    ))
                    .collect()
            )
        );
        let _ = write!(
            o,
            ",\"outside\":{{\"commits\":{},\"lint_findings\":{lint_n},\"list\":{}}}",
            found.outside.len(),
            list(
                found
                    .outside
                    .iter()
                    .map(|(sha, t, subj, why, lint)| format!(
                        "{{\"sha\":{},\"at\":{},\"subject\":{},\"why\":{},\"lint\":{}}}",
                        js(sha),
                        js(&date(*t)),
                        js(subj),
                        list(why.iter().map(|w| js(w)).collect()),
                        list(lint.iter().map(|w| js(w)).collect())
                    ))
                    .collect()
            )
        );
        let _ = write!(
            o,
            ",\"doctor\":{}",
            match &doctor {
                Some(d) => format!(
                    "{{\"problems\":{},\"notes\":{}}}",
                    list(d.problems.iter().map(|p| js(p)).collect()),
                    list(d.notes.iter().map(|p| js(p)).collect())
                ),
                None => "null".into(),
            }
        );
        let _ = write!(
            o,
            ",\"failures\":{{\"last\":{},\"reports\":{}}}",
            match &failure {
                Some(f) => format!(
                    "{{\"when\":{},\"command\":{},\"message\":{}}}",
                    js(&f.when),
                    js(&f.command),
                    js(&f.message)
                ),
                None => "null".into(),
            },
            list(
                reports
                    .iter()
                    .map(|(n, t, title, sent)| format!(
                        "{{\"n\":{n},\"at\":{},\"title\":{},\"sent\":{sent}}}",
                        js(&date(*t)),
                        js(title)
                    ))
                    .collect()
            )
        );
        let _ = write!(
            o,
            ",\"briefs\":{{\"tasks\":{},\"bytes\":{},\"tokens\":{},\"largest\":{}}}}}",
            briefs.len(),
            briefs.iter().map(|b| b.1).sum::<usize>(),
            briefs.iter().map(|b| b.2).sum::<usize>(),
            list(
                briefs
                    .iter()
                    .map(|(id, b, t)| format!("{{\"id\":{id},\"bytes\":{b},\"tokens\":{t}}}"))
                    .collect()
            )
        );
        println!("{o}");
        return Ok(());
    }

    // --- text -----------------------------------------------------------------------------
    let sty = Sty::new();
    let cap = if compact { 3 } else { 10 };
    let width = if compact { 100 } else { 160 };
    let mut out = String::new();
    let head = |name: &str| sty.bold(name);
    let joined = |v: &[(String, usize)], sigil: &str, n: usize| {
        let mut s = v
            .iter()
            .take(n)
            .map(|(k, c)| {
                if k == "-" && sigil == "@" {
                    format!("no area {c}")
                } else {
                    format!("{sigil}{k} {c}")
                }
            })
            .collect::<Vec<_>>()
            .join(" · ");
        if v.len() > n {
            let _ = write!(s, " · +{} more", v.len() - n);
        }
        s
    };
    let more = |out: &mut String, total: usize| {
        if total > cap {
            let hint = if compact {
                " (--full, --json)"
            } else {
                " (--json)"
            };
            let _ = writeln!(out, "  … {} more{hint}", total - cap);
        }
    };

    let window = match (found.first, found.last) {
        (Some(a), Some(b)) => format!(
            "{} queue commit{}, {}..{}",
            found.commits,
            if found.commits == 1 { "" } else { "s" },
            date(a),
            date(b)
        ),
        _ => "no queue commits".into(),
    };
    let since_note = since_arg
        .as_deref()
        .map(|s| format!(" since {s}"))
        .unwrap_or_default();
    let _ = writeln!(
        out,
        "{} {}{since_note} · {window}",
        head("audit"),
        repo.trunk
    );
    if found.diverged {
        let _ = writeln!(
            out,
            "  note: replaying the queue's history did not end at {}'s files (a merge or a rewrite?); figures may be off",
            repo.trunk
        );
    }

    let mut closed = String::new();
    if !via.is_empty() {
        closed = format!(
            " ({})",
            via.iter()
                .map(|(k, n)| {
                    if k == "-" {
                        format!("{n} without via:")
                    } else {
                        format!("{n} via:{k}")
                    }
                })
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    let _ = writeln!(
        out,
        "{} {} · {} open · {} in review · {} closed{closed}",
        head("tasks"),
        population.len(),
        state_n(State::Open),
        state_n(State::Review),
        state_n(State::Done)
    );
    if !population.is_empty() {
        let n = if compact { 6 } else { 20 };
        let _ = writeln!(out, "  lanes {}", joined(&lanes, ">", n));
        let _ = writeln!(out, "  levels {}", joined(&levels, "!", 5));
        let _ = writeln!(out, "  areas {}", joined(&areas, "@", n));
    }

    let _ = write!(out, "{} {} accepted", head("review"), review.len());
    if let Some((id, max)) = review.first() {
        let _ = write!(
            out,
            " · submit→accept median {} · p90 {} · max {} #{id}",
            dur(pct(&secs, 50)),
            dur(pct(&secs, 90)),
            dur(*max)
        );
    }
    out.push('\n');

    let _ = write!(
        out,
        "{} {} rejections on {} tasks",
        head("rework"),
        found.rejects.len(),
        per_task.len()
    );
    if !per_task.is_empty() {
        let _ = write!(
            out,
            " · once {} · twice {} · 3+ {}",
            cycles[0], cycles[1], cycles[2]
        );
    }
    out.push('\n');
    for (id, t, r) in reasons.iter().take(cap) {
        let times = per_task.get(id).copied().unwrap_or(1);
        let times = if times > 1 {
            format!(" ×{times}")
        } else {
            String::new()
        };
        let _ = writeln!(out, "  #{id}{times} {} {}", date(*t), truncate(r, width));
    }
    more(&mut out, reasons.len());

    let _ = write!(out, "{} {}", head("reopened"), found.reopens.len());
    if !reopened.is_empty() {
        let list: Vec<String> = reopened
            .iter()
            .take(cap * 3)
            .map(|(id, n)| {
                if *n > 1 {
                    format!("#{id} ×{n}")
                } else {
                    format!("#{id}")
                }
            })
            .collect();
        let _ = write!(out, " · {}", list.join(" "));
        if reopened.len() > cap * 3 {
            let _ = write!(out, " +{} more", reopened.len() - cap * 3);
        }
    }
    out.push('\n');

    let _ = write!(
        out,
        "{} {} tasks waited on needs",
        head("blocked"),
        blocked.len()
    );
    if !blocked.is_empty() {
        let _ = write!(
            out,
            " · median {} · max {}",
            dur(pct(&bsecs, 50)),
            dur(bsecs.last().copied().unwrap_or(0))
        );
        if still > 0 {
            let _ = write!(out, " · {still} still blocked");
        }
    }
    out.push('\n');
    for (id, s, needs, st) in blocked.iter().take(cap) {
        let st = if *st { " (still)" } else { "" };
        let on = if needs.is_empty() {
            String::new()
        } else {
            format!(" on {}", ids(needs))
        };
        let _ = writeln!(out, "  #{id} {}{on}{st}", dur(*s));
    }
    more(&mut out, blocked.len());

    let _ = writeln!(
        out,
        "{} {} queue commits not made by 5w · {lint_n} lint findings",
        head("outside"),
        found.outside.len()
    );
    // Most recent first: the news is at the end of history.
    for (sha, t, subj, why, lint) in found.outside.iter().rev().take(cap) {
        let _ = writeln!(
            out,
            "  {} {} {} — {}",
            short(sha),
            date(*t),
            truncate(subj, 60),
            why.join("; ")
        );
        for l in lint.iter().take(if compact { 1 } else { 5 }) {
            let _ = writeln!(out, "    lint: {}", truncate(l, width));
        }
        let shown = if compact { 1 } else { 5 };
        if lint.len() > shown {
            let _ = writeln!(
                out,
                "    lint: +{} more — 5w lint {}",
                lint.len() - shown,
                short(sha)
            );
        }
    }
    more(&mut out, found.outside.len());

    match &doctor {
        None => {
            let _ = writeln!(
                out,
                "{} no {} on {}",
                head("doctor"),
                repo.cfg.file,
                repo.trunk
            );
        }
        Some(d) if d.problems.is_empty() && d.notes.is_empty() => {
            let _ = writeln!(out, "{} ok", head("doctor"));
        }
        Some(d) => {
            let _ = writeln!(
                out,
                "{} {} problems · {} notes",
                head("doctor"),
                d.problems.len(),
                d.notes.len()
            );
            for p in d.problems.iter().take(cap) {
                let _ = writeln!(out, "  {}", truncate(p, width));
            }
            more(&mut out, d.problems.len());
            for n in d.notes.iter().take(cap) {
                let _ = writeln!(out, "  note: {}", truncate(n, width));
            }
            more(&mut out, d.notes.len());
        }
    }

    let _ = write!(out, "{}", head("failures"));
    match &failure {
        Some(f) => {
            let _ = write!(
                out,
                " last {} `{}` — {}",
                f.when,
                truncate(&f.command, 60),
                truncate(f.message.lines().next().unwrap_or(""), 80)
            );
        }
        None => out.push_str(" none recorded"),
    }
    let unsent = reports.iter().filter(|r| !r.3).count();
    let _ = writeln!(out, " · {} reports, {unsent} unsent", reports.len());
    for (n, t, title, sent) in reports.iter().rev().take(cap) {
        let sent = if *sent { " (sent)" } else { "" };
        let _ = writeln!(
            out,
            "  report {n} {} {}{sent}",
            date(*t),
            truncate(title, 80)
        );
    }
    more(&mut out, reports.len());

    let _ = write!(out, "{} {} open delegable", head("briefs"), briefs.len());
    if !briefs.is_empty() {
        let _ = write!(
            out,
            " · {} bytes · ~{} tokens · median ~{} · largest",
            briefs.iter().map(|b| b.1).sum::<usize>(),
            briefs.iter().map(|b| b.2).sum::<usize>(),
            {
                let mut t: Vec<i64> = briefs.iter().map(|b| b.2 as i64).collect();
                t.sort_unstable();
                pct(&t, 50)
            }
        );
        for (id, b, t) in briefs.iter().take(cap) {
            let _ = write!(out, " #{id} {b}B ~{t}");
        }
    }
    out.push('\n');
    print!("{out}");
    Ok(())
}

/// (id, times) in first-seen order.
fn counts_u64(v: &[u64]) -> Vec<(u64, usize)> {
    let mut out: Vec<(u64, usize)> = Vec::new();
    for id in v {
        match out.iter_mut().find(|(i, _)| i == id) {
            Some(e) => e.1 += 1,
            None => out.push((*id, 1)),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_round_trip() {
        for s in ["1970-01-01", "2000-02-29", "2026-09-16", "2024-12-31"] {
            let t = parse_date(s).unwrap();
            assert_eq!(date(t), s);
        }
        assert_eq!(
            parse_date("2026-09-16T14:43:47Z"),
            Some(parse_date("2026-09-16").unwrap() + 53027)
        );
        assert_eq!(parse_date("main"), None);
        assert_eq!(parse_date("2026-13-01"), None);
    }

    #[test]
    fn hunks_apply_by_position() {
        let mut f = File {
            lines: vec!["a".into(), "b".into(), "c".into()],
            fences: vec![],
        };
        let log = "\x01\x025w abc 1 s\n\ndiff --git a/T b/T\n--- a/T\n+++ b/T\n@@ -2 +2,2 @@\n-b\n+B\n+-x\n@@ -3,0 +5 @@\n+d\n";
        let c = parse_log(log, ["T", "D"]);
        assert_eq!(c[0].hunks[0].len(), 2);
        assert!(apply(&mut f, &c[0].hunks[0]));
        assert_eq!(f.lines, ["a", "B", "-x", "c", "d"]);
    }

    #[test]
    fn durations_read_short() {
        assert_eq!(dur(5), "5s");
        assert_eq!(dur(125), "2m");
        assert_eq!(dur(3 * 3600 + 300), "3h05m");
        assert_eq!(dur(2 * 86400 + 4 * 3600), "2d04h");
    }
}

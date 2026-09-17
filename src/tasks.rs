use crate::bail;
use crate::git;
use crate::queue::{self, Kind, State, Task};
use crate::store::{self, Repo};
use crate::util::{Res, Sty, parse_id, short, truncate};
use std::collections::HashSet;
use std::io::{IsTerminal, Read};

pub const USAGE: &str = concat!(
    "\
usage: 5w <command> [args]
example  5w ready level:1

read
  ready [filters] [out]   unblocked delegable work, easiest first (default)
  next [filters] [out]    the first ready task, with its body
  ls [filters] [out]      open and submitted tasks
  blocked | levels | all  what waits on what · counts · every queued task
  show <id> [--json]      one task, archived ones included
  delegate <id>           the brief for whoever does it
  review [--checklist] [--json|--ids]   submitted work, with drift since submit
  doctor                  check the file
  audit [--since <rev|date>] [--json]   how this repository has used 5W, from its history

write (each commits itself to the trunk, and only itself)
  add <text> [fields] [--body <text>|-]   over-long text is split into title + body
  set <id> <area|level|lane|needs|branch> <value|->
  submit <id> [branch]    worker: hand it back
  accept <id>... [--at <rev>] [--force]   several ids: one commit each, stops at a refusal
  reject <id> <reason>
  done <id> --<close>     close without review; flag per lane (--self, --decided)
  open <id>
  archive                 move closed tasks to the archive file
  split [--all]           shorten over-long titles into title + body
  batch                   write commands from stdin, one per line: one commit, or none if any refuses

protocol (PROTOCOL.md — the rules, for editing without the tool)
  lint [--staged | <rev> | <a>..<b>]   check queue edits follow it
  hook install | uninstall [pre-commit | pre-receive]   `lint --staged` hook, or the pre-receive check
  update-files [--pin]                 refresh PROTOCOL.md and hooks to this 5w; --pin sets requires
  version [--latest]                   this 5w; --latest also the newest release (changes nothing)
  self-update [--latest]               install the 5w requires pins, signature and sum checked; or the newest
  ci --base --head --ref|--branch      the forge-neutral check for CI and pre-receive
  ci --event submit|accept --branch    a change request's events as queue edits, from CI

feedback about 5W itself
  report <what happened>               save a report locally (last failure attached); nothing is sent
  report list | show | send [--gh|--print] | rm

branches
  wt <new|add|ls|path|rm|prune|link|install|setup|discard-copy>
  ship [branch] [--sync] [--squash [-m msg]] [--discard-ignored] [--force]
  init

filters  @area !level >lane — or area:x level:n lane:x (no quoting)
out      --json  --ids  --limit N  --full  --no-color (any command; also NO_COLOR, TERM=dumb)
ids      14 or #14. Output is compact when not on a terminal or FIVEW_AGENT=1.
docs     ",
    env!("CARGO_PKG_REPOSITORY"),
    "#readme"
);

/// One command's help: its entry from `USAGE`, and the footer lines that entry
/// refers to (filters, out, ids), so the two can never drift. `None` for a
/// command `USAGE` does not list.
pub fn command_usage(cmd: &str) -> Option<String> {
    let cmd = match cmd {
        "list" => "ls",
        "reopen" => "open",
        c => c,
    };
    let (body, footer) = USAGE.split_once("\n\nfilters ")?;
    let entry = body.lines().find(|l| {
        let Some(spec) = l.strip_prefix("  ") else {
            return false;
        };
        let names = spec.split("  ").next().unwrap_or("");
        let parts: Vec<&str> = names.split(" | ").collect();
        if parts.iter().all(|p| !p.contains(' ')) {
            parts.contains(&cmd)
        } else {
            names.split(' ').next() == Some(cmd)
        }
    })?;
    let mut out = format!("usage: 5w {}", entry.trim());
    for f in format!("filters {footer}").lines() {
        let key = f.split(' ').next().unwrap_or("");
        let wanted = match key {
            "filters" => entry.contains("[filters]"),
            "out" => entry.contains("[out]"),
            "ids" => entry.contains("<id>"),
            _ => false,
        };
        if wanted {
            out.push('\n');
            out.push_str(f);
        }
    }
    Some(out)
}

// --- options ---------------------------------------------------------------------------

#[derive(Default)]
struct Opts {
    area: Option<String>,
    level: Option<u8>,
    lane: Option<String>,
    json: bool,
    ids: bool,
    limit: Option<usize>,
    compact: bool,
    /// `--full` given: JSON rows carry every field, not just the list's six.
    full: bool,
    flags: Vec<String>,
}

fn opts(args: &[String]) -> Res<Opts> {
    let mut o = Opts {
        compact: compact_default(),
        ..Default::default()
    };
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        match a.as_str() {
            "--json" => o.json = true,
            "--ids" => o.ids = true,
            "--full" => {
                o.compact = false;
                o.full = true;
            }
            "--limit" => {
                let n = args.get(i + 1).ok_or("--limit needs a number")?;
                o.limit = Some(n.parse().map_err(|_| format!("bad --limit {n}"))?);
                i += 1;
            }
            f if f.starts_with("--") => o.flags.push(f.to_string()),
            _ => {
                if let Some(v) = a.strip_prefix('@').or_else(|| a.strip_prefix("area:")) {
                    o.area = Some(v.into());
                } else if let Some(v) = a.strip_prefix('!').or_else(|| a.strip_prefix("level:")) {
                    o.level = Some(v.parse().map_err(|_| format!("bad level {a}"))?);
                } else if let Some(v) = a.strip_prefix('>').or_else(|| a.strip_prefix("lane:")) {
                    o.lane = Some(v.into());
                } else {
                    bail!("unknown filter {a:?} (want @area !level >lane)");
                }
            }
        }
        i += 1;
    }
    Ok(o)
}

/// Compact unless a person is looking: stdout a terminal and FIVEW_AGENT unset.
pub fn compact_default() -> bool {
    match std::env::var("FIVEW_AGENT").as_deref() {
        Ok("0") => false,
        Ok(_) => true,
        Err(_) => !std::io::stdout().is_terminal(),
    }
}

// --- the loaded queue --------------------------------------------------------------------

struct Q<'a> {
    repo: &'a Repo,
    tasks: Vec<Task>,
    archived: Vec<Task>,
    done: HashSet<u64>,
    sty: Sty,
}

impl<'a> Q<'a> {
    fn load(repo: &'a Repo) -> Res<Q<'a>> {
        let tasks = queue::parse(&repo.load()?);
        let archived = queue::parse(&repo.load_archive()?);
        let mut both = tasks.clone();
        both.extend(archived.iter().cloned());
        let dup = queue::duplicates(&tasks);
        let cross: Vec<u64> = archived
            .iter()
            .filter(|a| tasks.iter().any(|t| t.id == a.id))
            .map(|a| a.id)
            .collect();
        if !dup.is_empty() || !cross.is_empty() {
            let mut parts = Vec::new();
            for (id, a, b) in dup {
                parts.push(format!("#{id} lines {a} and {b} of {}", repo.cfg.file));
            }
            for id in cross {
                parts.push(format!(
                    "#{id} in both {} and {}",
                    repo.cfg.file, repo.cfg.archive
                ));
            }
            return Err(format!(
                "duplicate ids — fix before anything else: {}",
                parts.join("; ")
            ));
        }
        let done = both
            .iter()
            .filter(|t| t.state == State::Done)
            .map(|t| t.id)
            .collect();
        Ok(Q {
            repo,
            tasks,
            archived,
            done,
            sty: Sty::new(),
        })
    }

    fn get(&self, id: u64) -> Res<&Task> {
        self.tasks
            .iter()
            .chain(&self.archived)
            .find(|t| t.id == id)
            .ok_or_else(|| format!("no task #{id}"))
    }

    fn is_archived(&self, id: u64) -> bool {
        self.archived.iter().any(|t| t.id == id)
    }

    fn lane<'t>(&'t self, t: &'t Task) -> &'t str {
        t.lane.as_deref().unwrap_or(&self.repo.cfg.default_lane)
    }

    fn unmet(&self, t: &Task) -> Vec<u64> {
        t.needs
            .iter()
            .copied()
            .filter(|n| !self.done.contains(n))
            .collect()
    }

    fn delegable(&self, t: &Task) -> bool {
        self.repo
            .cfg
            .lane(self.lane(t))
            .map(|l| l.delegable)
            .unwrap_or(false)
    }

    fn matches(&self, t: &Task, o: &Opts) -> bool {
        o.area.as_ref().is_none_or(|a| t.area.as_ref() == Some(a))
            && o.level.is_none_or(|l| t.level == Some(l))
            && o.lane.as_ref().is_none_or(|l| self.lane(t) == l)
    }

    fn ready(&self, o: &Opts) -> Vec<&Task> {
        let mut v: Vec<&Task> = self
            .tasks
            .iter()
            .filter(|t| t.state == State::Open)
            .filter(|t| o.lane.is_some() || self.delegable(t))
            .filter(|t| self.matches(t, o) && self.unmet(t).is_empty())
            .collect();
        v.sort_by_key(|t| (t.level.unwrap_or(9), t.id));
        v
    }

    /// One line per task. Compact: `#14 !3 @faces title [branch] >lane — note`.
    fn row(&self, t: &Task, note: &str, o: &Opts) {
        let s = &self.sty;
        let lane = self.lane(t);
        let lane = if lane != self.repo.cfg.default_lane {
            format!(" >{lane}")
        } else {
            String::new()
        };
        let branch = t
            .branch
            .as_ref()
            .map(|b| format!(" [{b}]"))
            .unwrap_or_default();
        if o.compact {
            let level = t.level.map(|l| format!(" !{l}")).unwrap_or_default();
            let area = t
                .area
                .as_ref()
                .map(|a| format!(" @{a}"))
                .unwrap_or_default();
            let max = self.repo.cfg.title_max;
            let len = t.text.chars().count();
            let title = if max > 0 && len > max {
                format!(
                    "{}(+{} chars: {} show {})",
                    truncate(&t.text, max),
                    len - max,
                    self.repo.cfg.cmd_tasks,
                    t.id
                )
            } else {
                t.text.clone()
            };
            let body = if t.body.is_empty() { "" } else { " +" };
            let note = if note.is_empty() {
                String::new()
            } else {
                format!(" — {note}")
            };
            println!("#{}{level}{area} {title}{body}{branch}{lane}{note}", t.id);
            return;
        }
        let area = t.area.as_ref().map(|a| format!("@{a}")).unwrap_or_default();
        let mut out = format!(
            "  {} {:<9} {}",
            s.bold(&format!("#{:<3}", t.id)),
            area,
            t.text
        );
        out += &s.dim(&format!("{branch}{lane}"));
        if !t.body.is_empty() {
            out += &s.dim(" +body");
        }
        if !note.is_empty() {
            out += &format!(" {}", s.red(note));
        }
        println!("{out}");
    }

    /// One task as a JSON object. A list row (`full` false) carries what the
    /// text row shows — id, state, level, area, title, branch, unmet, rework;
    /// `full` adds lane, kind and needs, and `body` the body and the review fields.
    fn json(&self, t: &Task, full: bool, body: bool) -> String {
        let opt = |v: &Option<String>| v.as_ref().map(|s| js(s)).unwrap_or("null".into());
        let ids = |v: &[u64]| {
            format!(
                "[{}]",
                v.iter()
                    .map(|i| i.to_string())
                    .collect::<Vec<_>>()
                    .join(",")
            )
        };
        let state = match t.state {
            State::Open => "open",
            State::Review => "review",
            State::Done => "done",
        };
        let level = t.level.map(|l| l.to_string()).unwrap_or("null".into());
        if !full {
            return format!(
                "{{\"id\":{},\"state\":\"{state}\",\"level\":{level},\"area\":{},\"title\":{},\"branch\":{},\"unmet\":{},\"rework\":{}}}",
                t.id,
                opt(&t.area),
                js(&t.text),
                opt(&t.branch),
                ids(&self.unmet(t)),
                opt(&t.rework),
            );
        }
        let mut out = format!(
            "{{\"id\":{},\"state\":\"{state}\",\"level\":{level},\"area\":{},\"lane\":{},\"kind\":{},\"title\":{},\"branch\":{},\"needs\":{},\"unmet\":{},\"rework\":{}",
            t.id,
            opt(&t.area),
            js(self.lane(t)),
            js(self
                .repo
                .cfg
                .lane(self.lane(t))
                .map(|l| l.kind.name())
                .unwrap_or("unknown")),
            js(&t.text),
            opt(&t.branch),
            ids(&t.needs),
            ids(&self.unmet(t)),
            opt(&t.rework),
        );
        if body {
            out += &format!(
                ",\"body\":[{}],\"via\":{},\"submitted\":{},\"reviewed\":{},\"archived\":{}",
                t.body.iter().map(|l| js(l)).collect::<Vec<_>>().join(","),
                opt(&t.via),
                opt(&t.submitted),
                opt(&t.reviewed),
                self.is_archived(t.id)
            );
        }
        out + "}"
    }

    /// Rows, ids or JSON lines for a list, honouring --limit.
    fn list(&self, rows: Vec<(&Task, String)>, o: &Opts) -> usize {
        let n = rows.len();
        let shown = o.limit.unwrap_or(n).min(n);
        for (t, note) in rows.into_iter().take(shown) {
            if o.json {
                println!("{}", self.json(t, o.full, false));
            } else if o.ids {
                println!("{}", t.id);
            } else {
                self.row(t, &note, o);
            }
        }
        if shown < n && !o.json && !o.ids {
            println!("… {} more (--limit)", n - shown);
        }
        n
    }
}

pub fn js(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out += "\\\"",
            '\\' => out += "\\\\",
            '\n' => out += "\\n",
            '\r' => out += "\\r",
            '\t' => out += "\\t",
            c if (c as u32) < 0x20 => out += &format!("\\u{:04x}", c as u32),
            c => out.push(c),
        }
    }
    out + "\""
}

/// Submit and accept cannot tell who is typing, so on a lane whose work only a
/// person (manual) or the owner (decision) can do, they say so rather than
/// refuse: an owner decision can legitimately arrive as a branch.
fn warn_not_delegable(repo: &Repo, t: &Task, what: &str) {
    let name = t.lane.as_deref().unwrap_or(&repo.cfg.default_lane);
    if let Some(l) = repo.cfg.lane(name)
        && !l.kind.delegable()
    {
        let who = if l.kind == crate::config::LaneKind::Manual {
            "a person, by hand,"
        } else {
            "the owner"
        };
        eprintln!(
            "note: #{} is >{} ({}): only {who} can have done this — {what} only if that is so",
            t.id,
            l.name,
            l.kind.name()
        );
    }
}

/// The task as the committed queue has it, under the lock.
fn committed(t: Option<&Task>, id: u64) -> Res<&Task> {
    t.ok_or_else(|| format!("#{id} is not on the trunk's queue"))
}

fn open_only(t: Option<&Task>, id: u64, tasks: &str) -> Res<()> {
    if committed(t, id)?.state == State::Done {
        bail!("#{id} is closed; `{tasks} open {id}` first");
    }
    Ok(())
}

fn ids_str(v: &[u64]) -> String {
    v.iter()
        .map(|i| format!("#{i}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The out flags a list command takes.
const OUT: &[&str] = &["--json", "--ids", "--limit", "--full"];

/// The flags each command takes, for the commands whose arguments are never
/// free text; `None` for those that check their own (`add`, `accept`, `done`)
/// or take text that may start with `--` (`reject`, `set`, `submit`).
fn flags_of(cmd: &str) -> Option<&'static [&'static str]> {
    Some(match cmd {
        "ready" | "next" | "ls" | "list" | "blocked" | "all" => OUT,
        "show" => &["--json"],
        "review" => &["--checklist", "--json", "--ids", "--full"],
        "split" => &["--all"],
        "delegate" | "branch" | "doctor" | "levels" | "archive" | "open" | "reopen" | "batch" => {
            &[]
        }
        _ => return None,
    })
}

/// A refusal for a `--flag` the command does not take: a flag that did nothing
/// must not look as if it worked.
pub fn unknown_flag(repo: &Repo, cmd: &str, flag: &str) -> String {
    format!(
        "unknown flag {flag} for {cmd} ({} {cmd} --help)",
        repo.cfg.cmd_tasks
    )
}

pub fn run(repo: &Repo, cmd: &str, args: &[String]) -> Res<()> {
    if let Some(takes) = flags_of(cmd)
        && let Some(f) = args
            .iter()
            .find(|a| a.starts_with("--") && !takes.contains(&a.as_str()))
    {
        return Err(unknown_flag(repo, cmd, f));
    }
    match cmd {
        "ready" => ready(repo, args),
        "next" => next(repo, args),
        "ls" | "list" => ls(repo, args),
        "blocked" => blocked(repo, args),
        "levels" => levels(repo),
        "all" => all(repo, args),
        "show" => show(repo, args),
        "delegate" => delegate(repo, arg(args, 0, "delegate <id>")?),
        "branch" => branch(repo, arg(args, 0, "branch <id>")?),
        "doctor" => doctor(repo),
        "add" => add(repo, args),
        "set" => set(repo, args),
        "submit" => submit(repo, args),
        "review" => review(repo, args),
        "accept" => accept(repo, args),
        "reject" => reject(repo, args),
        "done" => done(repo, args),
        "open" | "reopen" => reopen(repo, arg(args, 0, "open <id>")?),
        "archive" => archive(repo),
        "split" => split(repo, args),
        "batch" => batch(repo, args),
        _ => bail!("unknown command: {cmd} (5w help)"),
    }
}

fn arg<'a>(args: &'a [String], i: usize, usage: &str) -> Res<&'a str> {
    args.get(i)
        .map(|s| s.as_str())
        .ok_or_else(|| format!("usage: 5w {usage}"))
}

// --- reading ----------------------------------------------------------------------------

fn ready(repo: &Repo, args: &[String]) -> Res<()> {
    let o = opts(args)?;
    let q = Q::load(repo)?;
    let ready = q.ready(&o);
    let note = |t: &Task| {
        if t.rework.is_some() {
            "sent back".to_string()
        } else {
            String::new()
        }
    };
    if o.json || o.ids || o.compact {
        q.list(ready.iter().map(|t| (*t, note(t))).collect(), &o);
    } else {
        let mut last = None::<Option<u8>>;
        let limit = o.limit.unwrap_or(usize::MAX);
        for t in ready.iter().take(limit) {
            if last != Some(t.level) {
                let label = t.level.map(|l| format!("!{l}")).unwrap_or("!-".into());
                println!("\n{}  {}", q.sty.bold(&label), repo.cfg.tier(t.level));
                last = Some(t.level);
            }
            q.row(t, &note(t), &o);
        }
    }
    if o.json || o.ids {
        return Ok(());
    }
    if ready.is_empty() {
        println!("(nothing ready)");
    }
    let open: Vec<&Task> = q.tasks.iter().filter(|t| t.state == State::Open).collect();
    let mut parts = vec![format!("{} ready", ready.len())];
    for l in repo.cfg.lanes.iter().filter(|l| !l.delegable) {
        let n = open.iter().filter(|t| q.lane(t) == l.name).count();
        if n > 0 {
            parts.push(format!("{n} >{}", l.name));
        }
    }
    let blocked = open.iter().filter(|t| !q.unmet(t).is_empty()).count();
    parts.push(format!("{blocked} blocked"));
    let review = q.tasks.iter().filter(|t| t.state == State::Review).count();
    if review > 0 {
        parts.push(format!("{review} in review"));
    }
    let mut line = parts.join(" · ");
    if let Some(t) = ready.first() {
        line.push_str(&format!(" → {} delegate {}", repo.cfg.cmd_tasks, t.id));
    }
    if o.compact {
        println!("{line}")
    } else {
        println!("\n{}", q.sty.dim(&line))
    }
    Ok(())
}

fn next(repo: &Repo, args: &[String]) -> Res<()> {
    let o = opts(args)?;
    let q = Q::load(repo)?;
    let Some(t) = q.ready(&o).into_iter().next() else {
        bail!("nothing ready under those filters");
    };
    if o.json {
        println!("{}", q.json(t, true, true));
    } else if o.ids {
        println!("{}", t.id);
    } else {
        print_task(&q, t);
        println!("→ {} delegate {}", repo.cfg.cmd_tasks, t.id);
    }
    Ok(())
}

fn ls(repo: &Repo, args: &[String]) -> Res<()> {
    let o = opts(args)?;
    let q = Q::load(repo)?;
    let rows = q
        .tasks
        .iter()
        .filter(|t| t.state != State::Done && q.matches(t, &o))
        .map(|t| {
            let mut notes = Vec::new();
            if t.state == State::Review {
                notes.push("in review".to_string());
            }
            let u = q.unmet(t);
            if !u.is_empty() {
                notes.push(format!("blocked by {}", ids_str(&u)));
            }
            if t.rework.is_some() {
                notes.push("sent back".into());
            }
            (t, notes.join(", "))
        })
        .collect();
    q.list(rows, &o);
    Ok(())
}

fn blocked(repo: &Repo, args: &[String]) -> Res<()> {
    let o = opts(args)?;
    let q = Q::load(repo)?;
    let rows = q
        .tasks
        .iter()
        .filter(|t| t.state == State::Open && q.matches(t, &o))
        .filter_map(|t| {
            let u = q.unmet(t);
            (!u.is_empty()).then(|| (t, format!("blocked by {}", ids_str(&u))))
        })
        .collect();
    q.list(rows, &o);
    Ok(())
}

fn levels(repo: &Repo) -> Res<()> {
    let q = Q::load(repo)?;
    let open: Vec<&Task> = q.tasks.iter().filter(|t| t.state == State::Open).collect();
    let mut parts = Vec::new();
    for lvl in [Some(1), Some(2), Some(3), Some(4), None] {
        let n = open
            .iter()
            .filter(|t| t.level == lvl && q.delegable(t))
            .count();
        if n > 0 {
            parts.push(format!(
                "!{} {n}",
                lvl.map(|l| l.to_string()).unwrap_or("-".into())
            ));
        }
    }
    println!("delegable by level: {}", parts.join(" · "));
    let lanes: Vec<String> = repo
        .cfg
        .lanes
        .iter()
        .filter_map(|l| {
            let n = open.iter().filter(|t| q.lane(t) == l.name).count();
            (n > 0).then(|| {
                if l.name == l.kind.name() {
                    format!(">{} {n}", l.name)
                } else {
                    format!(">{} ({}) {n}", l.name, l.kind.name())
                }
            })
        })
        .collect();
    println!("open by lane: {}", lanes.join(" · "));
    println!(
        "closed: {} in queue, {} archived",
        q.tasks.len() - open.len() - q.tasks.iter().filter(|t| t.state == State::Review).count(),
        q.archived.len()
    );
    Ok(())
}

fn all(repo: &Repo, args: &[String]) -> Res<()> {
    let o = opts(args)?;
    let q = Q::load(repo)?;
    let rows = q
        .tasks
        .iter()
        .filter(|t| q.matches(t, &o))
        .map(|t| (t, format!("[{}]", t.state.mark())))
        .collect();
    q.list(rows, &o);
    Ok(())
}

fn print_task(q: &Q, t: &Task) {
    let level = t.level.map(|l| format!(" !{l}")).unwrap_or_default();
    let area = t
        .area
        .as_ref()
        .map(|a| format!(" @{a}"))
        .unwrap_or_default();
    let archived = if q.is_archived(t.id) {
        ", archived"
    } else {
        ""
    };
    println!(
        "#{} [{}{archived}]{level}{area} >{}  {}",
        t.id,
        t.state.name(),
        q.lane(t),
        t.text
    );
    for l in &t.body {
        println!("  {l}");
    }
    let mut facts = Vec::new();
    if let Some(b) = &t.branch {
        facts.push(format!("branch {b}"));
    }
    if !t.needs.is_empty() {
        let u = q.unmet(t);
        let unmet = if u.is_empty() {
            String::new()
        } else {
            format!(" (unmet {})", ids_str(&u))
        };
        facts.push(format!("needs {}{unmet}", ids_str(&t.needs)));
    }
    let deps: Vec<u64> = q
        .tasks
        .iter()
        .filter(|d| d.needs.contains(&t.id))
        .map(|d| d.id)
        .collect();
    if !deps.is_empty() {
        facts.push(format!("blocks {}", ids_str(&deps)));
    }
    if let Some(s) = &t.submitted {
        facts.push(format!("submitted at {s}"));
    }
    if let Some(r) = &t.reviewed {
        facts.push(format!("reviewed at {r}"));
    }
    if let Some(v) = &t.via {
        facts.push(format!("via:{v}"));
    }
    if !facts.is_empty() {
        println!("{}", facts.join(" · "));
    }
    if let Some(r) = &t.rework {
        println!("rework: {r}");
    }
}

fn show(repo: &Repo, args: &[String]) -> Res<()> {
    let o = opts(&args.iter().skip(1).cloned().collect::<Vec<_>>())?;
    let q = Q::load(repo)?;
    let t = q.get(parse_id(arg(args, 0, "show <id>")?)?)?;
    if o.json {
        println!("{}", q.json(t, true, true));
    } else {
        print_task(&q, t);
    }
    Ok(())
}

pub fn suggested_branch(t: &Task) -> String {
    format!("{}/task-{}", t.area.as_deref().unwrap_or("work"), t.id)
}

fn branch(repo: &Repo, id: &str) -> Res<()> {
    let q = Q::load(repo)?;
    let t = q.get(parse_id(id)?)?;
    println!(
        "{}",
        t.branch.clone().unwrap_or_else(|| suggested_branch(t))
    );
    Ok(())
}

/// `path.md #12`, `path.md §3`, `path.md R7` — the sections a brief points at, so
/// a worker reads those rather than the whole file.
fn refs(text: &str) -> Vec<String> {
    let words: Vec<&str> = text.split_whitespace().collect();
    let clean = |w: &str| {
        w.trim_matches(|c: char| "()[],;:'\"`".contains(c))
            .trim_end_matches(['.', ',', ';', ':', ')'])
            .to_string()
    };
    // `#12`, `§3`, `R7`, `sect4`, `section 4`'s `4`, `12.1`.
    let is_section = |n: &str| {
        let rest = n
            .strip_prefix('#')
            .or_else(|| n.strip_prefix('§'))
            .or_else(|| n.strip_prefix("sect"))
            .or_else(|| n.strip_prefix('R'))
            .unwrap_or(n);
        !rest.is_empty()
            && rest.chars().all(|c| c.is_ascii_digit() || c == '.')
            && rest.starts_with(|c: char| c.is_ascii_digit())
    };
    let mut out: Vec<String> = Vec::new();
    for (i, w) in words.iter().enumerate() {
        let w = clean(w);
        if !w.ends_with(".md") {
            continue;
        }
        let secs: Vec<String> = words[i + 1..]
            .iter()
            .map(|n| clean(n))
            .take_while(|n| is_section(n))
            .collect();
        let r = if secs.is_empty() {
            w
        } else {
            format!("{w} {}", secs.join(" "))
        };
        if !out.contains(&r) {
            out.push(r);
        }
    }
    out
}

fn delegate(repo: &Repo, id: &str) -> Res<()> {
    let q = Q::load(repo)?;
    let t = q.get(parse_id(id)?)?;
    match t.state {
        State::Done => bail!("#{} is closed", t.id),
        State::Review => bail!("#{} is in review, not waiting on work", t.id),
        State::Open => {}
    }
    if let Some(l) = repo.cfg.lane(q.lane(t)).filter(|l| !l.delegable) {
        bail!(
            "#{} is >{} ({}): {}",
            t.id,
            l.name,
            l.kind.name(),
            l.refuse.as_deref().unwrap_or("not delegable")
        );
    }
    let u = q.unmet(t);
    if !u.is_empty() {
        eprintln!("warning: #{} is blocked by {}", t.id, ids_str(&u));
    }
    let branch = t.branch.clone().unwrap_or_else(|| suggested_branch(t));
    let exists = git::branch_exists(&repo.primary, &branch);
    let has_wt = exists && git::worktree_of(&repo.primary, &branch)?.is_some();
    print!("{}", brief(&q, t, exists, has_wt));
    Ok(())
}

/// The brief for an open, delegable task. `exists` and `has_wt` say whether its
/// branch and a worktree for it are already there, which picks the setup step.
fn brief(q: &Q, t: &Task, exists: bool, has_wt: bool) -> String {
    use std::fmt::Write;
    let repo = q.repo;
    let cfg = &repo.cfg;
    let lane_name = q.lane(t);
    let lane = cfg.lane(lane_name);
    let branch = t.branch.clone().unwrap_or_else(|| suggested_branch(t));
    let setup = if !exists {
        format!("{} new {branch}", cfg.cmd_wt)
    } else if has_wt {
        format!("# {branch} and its worktree exist — the last attempt is there")
    } else {
        format!("{} add {branch}", cfg.cmd_wt)
    };
    let mut docs = cfg.context_docs.clone();
    if let Some(a) = &t.area {
        for d in &cfg.area_docs {
            if repo.primary.join(a).join(d).is_file() {
                docs.push(format!("{a}/{d}"));
            }
        }
    }
    let all_text = format!("{} {}", t.text, t.body.join(" "));
    let refs = refs(&all_text);

    let mut out = String::new();
    let level = t.level.map(|l| format!(" !{l}")).unwrap_or_default();
    let area = t
        .area
        .as_ref()
        .map(|a| format!(" @{a}"))
        .unwrap_or_default();
    let _ = writeln!(out, "#{}{level}{area} >{lane_name}  {}", t.id, t.text);
    for l in &t.body {
        let _ = writeln!(out, "  {l}");
    }
    if let Some(r) = &t.rework {
        let _ = writeln!(out, "REWORK (last attempt sent back): {r}");
    }
    if let Some(n) = lane.and_then(|l| l.note.as_ref()) {
        let _ = writeln!(out, "lane: {n}");
    }
    if !refs.is_empty() {
        let _ = writeln!(
            out,
            "refs: {} — read these sections, not whole files",
            refs.join(", ")
        );
    }
    if !docs.is_empty() {
        let _ = writeln!(out, "docs: {}", docs.join(", "));
    }
    if !cfg.conventions.is_empty() {
        let _ = writeln!(out, "conventions: {}", cfg.conventions.join(", "));
    }
    let _ = writeln!(out, "tier: {}", cfg.tier(t.level));
    let footer = cfg.brief_footer.clone().unwrap_or_else(|| {
        "steps:\n  {setup}\n  cd \"$({wt} path {branch})\"\n  work, commit, then: {tasks} submit {id} {branch}\n\
         rules: do not ship or close it. If 5w refuses something, the refusal names the fix;\n\
         a refusal that itself looks wrong: 5w report \"<what happened>\".\n"
            .into()
    });
    out += &footer
        .replace("{setup}", &setup)
        .replace("{tasks}", &cfg.cmd_tasks)
        .replace("{ship}", &cfg.cmd_ship)
        .replace("{wt}", &cfg.cmd_wt)
        .replace("{id}", &t.id.to_string())
        .replace("{branch}", &branch);
    out
}

/// What each open delegable task's brief costs a reader: `(id, bytes, estimated
/// tokens)`, largest first. For `audit`; one git call for all the branches.
pub fn brief_costs(repo: &Repo) -> Res<Vec<(u64, usize, usize)>> {
    let q = Q::load(repo)?;
    let branches: HashSet<String> = git::git(
        &repo.primary,
        &["for-each-ref", "--format=%(refname:short)", "refs/heads"],
    )?
    .lines()
    .map(String::from)
    .collect();
    let trees: HashSet<String> = git::worktrees(&repo.primary)?
        .into_iter()
        .filter_map(|w| w.branch)
        .collect();
    let mut v: Vec<(u64, usize, usize)> = q
        .tasks
        .iter()
        .filter(|t| t.state == State::Open && q.delegable(t))
        .map(|t| {
            let b = t.branch.clone().unwrap_or_else(|| suggested_branch(t));
            let text = brief(&q, t, branches.contains(&b), trees.contains(&b));
            (t.id, text.len(), crate::tokens::estimate(&text))
        })
        .collect();
    v.sort_by(|a, b| b.2.cmp(&a.2).then(b.1.cmp(&a.1)).then(a.0.cmp(&b.0)));
    Ok(v)
}

fn doctor(repo: &Repo) -> Res<()> {
    let d = doctor_findings(repo)?;
    for p in &d.problems {
        println!("  {p}");
    }
    for n in &d.notes {
        println!("  note: {n}");
    }
    if d.problems.is_empty() {
        println!("  ok — {} queued, {} archived", d.queued, d.archived);
        Ok(())
    } else {
        bail!("{} problem(s)", d.problems.len())
    }
}

pub struct Doctor {
    pub problems: Vec<String>,
    pub notes: Vec<String>,
    pub queued: usize,
    pub archived: usize,
}

/// What `doctor` finds in the queue as it is now, without printing it.
pub fn doctor_findings(repo: &Repo) -> Res<Doctor> {
    let text = repo.load()?;
    let archive = repo.load_archive()?;
    let tasks = queue::parse(&text);
    let archived = queue::parse(&archive);
    let mut problems = Vec::new();
    let mut say = |s: String| problems.push(s);
    for (id, a, b) in queue::duplicates(&tasks) {
        say(format!("#{id} appears twice: lines {a} and {b}"));
    }
    for a in &archived {
        if tasks.iter().any(|t| t.id == a.id) {
            say(format!(
                "#{} is in both {} and {}",
                a.id, repo.cfg.file, repo.cfg.archive
            ));
        }
    }
    let ids: HashSet<u64> = tasks.iter().chain(&archived).map(|t| t.id).collect();
    let cfg = &repo.cfg;
    let mut long = 0;
    for t in &tasks {
        for n in &t.needs {
            if !ids.contains(n) {
                say(format!("#{} needs #{n}, which does not exist", t.id));
            }
        }
        let lane = t.lane.as_deref().unwrap_or(&cfg.default_lane);
        if cfg.lane(lane).is_none() {
            say(format!(
                "#{} is on lane >{lane}, which the config does not name",
                t.id
            ));
        }
        let want = match t.state {
            State::Done => cfg.done_section.clone(),
            _ => cfg.section_for(lane),
        };
        if t.section.as_deref() != Some(want.as_str()) {
            say(format!(
                "#{} ({}) sits under {:?}, expected {:?}",
                t.id,
                t.state.name(),
                t.section.as_deref().unwrap_or("no heading"),
                want
            ));
        }
        if t.state == State::Review {
            match &t.branch {
                None => say(format!("#{} is submitted with no branch", t.id)),
                Some(b) if !git::branch_exists(&repo.primary, b) => say(format!(
                    "#{} is submitted on {b}, which does not exist",
                    t.id
                )),
                _ => {}
            }
        }
        if t.state != State::Done && cfg.title_max > 0 && t.text.chars().count() > cfg.title_max {
            long += 1;
        }
    }
    if queue::unclosed_fence(&text) {
        say("a ``` fence is never closed — every task after it is invisible".into());
    }
    let mut notes = Vec::new();
    let closed = tasks.iter().filter(|t| t.state == State::Done).count();
    if closed > 0 {
        notes.push(format!(
            "{closed} closed tasks still in {} — `5w archive` moves them out",
            cfg.file
        ));
    }
    notes.extend(crate::upkeep::notes(repo)?);
    notes.extend(crate::wt::copy_notes(repo));
    notes.extend(crate::wt::stack_notes(repo));
    if long > 0 {
        notes.push(format!(
            "{long} open titles over {} chars — `5w split`",
            cfg.title_max
        ));
    }
    Ok(Doctor {
        problems,
        notes,
        queued: tasks.len(),
        archived: archived.len(),
    })
}

// --- writing -------------------------------------------------------------------------------

fn validate_field(repo: &Repo, w: &str, ids: &HashSet<u64>) -> Res<Kind> {
    if w.chars().any(|c| c.is_whitespace() || c.is_control()) {
        bail!("a field cannot contain whitespace or control characters: {w:?}");
    }
    let k = queue::classify(w);
    match k {
        Kind::Area | Kind::Level => {}
        Kind::Branch => {
            if !git::ok(&repo.primary, &["check-ref-format", "--branch", &w[7..]]) {
                bail!("not a valid branch name: {:?}", &w[7..]);
            }
        }
        Kind::Lane => {
            if repo.cfg.lane(&w[1..]).is_none() {
                let names: Vec<String> = repo
                    .cfg
                    .lanes
                    .iter()
                    .map(|l| format!(">{}", l.name))
                    .collect();
                bail!("unknown lane {w} (have: {})", names.join(" "));
            }
        }
        Kind::Needs => {
            for n in w[6..].split([',', '#']).filter(|s| !s.is_empty()) {
                let n: u64 = n.parse().map_err(|_| format!("bad blocker in {w}"))?;
                if !ids.contains(&n) {
                    bail!("needs #{n}, which does not exist");
                }
            }
        }
        _ => bail!("not a field: {w:?} (want @area !n >lane needs:#a,#b branch:x)"),
    }
    Ok(k)
}

/// Accept `area:x level:n lane:x` as spellings that survive an unquoted shell.
fn normalise_field(a: &str) -> String {
    if let Some(v) = a.strip_prefix("area:") {
        format!("@{v}")
    } else if let Some(v) = a.strip_prefix("level:") {
        format!("!{v}")
    } else if let Some(v) = a.strip_prefix("lane:") {
        format!(">{v}")
    } else {
        a.to_string()
    }
}

fn add(repo: &Repo, args: &[String]) -> Res<()> {
    let usage = "usage: 5w add <text> [@area] [!n] [>lane] [needs:#1,#2] [--body <text>|-]";
    let mut text = None;
    let mut fields = Vec::new();
    let mut body: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if a == "--body" {
            let v = args.get(i + 1).ok_or(usage)?;
            body = Some(if v == "-" {
                let mut s = String::new();
                std::io::stdin()
                    .read_to_string(&mut s)
                    .map_err(|e| e.to_string())?;
                s
            } else {
                v.clone()
            });
            i += 2;
            continue;
        }
        if text.is_some() && a.starts_with("--") {
            return Err(unknown_flag(repo, "add", a));
        }
        if text.is_none() {
            if a.starts_with('-') {
                bail!(
                    "text first, not a flag: {a} ({} add --help)",
                    repo.cfg.cmd_tasks
                );
            }
            text = Some(a.clone());
        } else {
            fields.push(normalise_field(a));
        }
        i += 1;
    }
    let mut text = text.ok_or(usage)?;
    if text.contains(['\n', '\r']) {
        bail!("text is one line; the rest goes in --body");
    }
    let q = Q::load(repo)?;
    let ids: HashSet<u64> = q.tasks.iter().chain(&q.archived).map(|t| t.id).collect();
    let mut lane = repo.cfg.default_lane.clone();
    for f in &fields {
        if validate_field(repo, f, &ids)? == Kind::Lane {
            lane = f[1..].to_string();
        }
    }
    for tok in queue::tokenize(&text) {
        if tok.kind != Kind::Text {
            bail!(
                "text contains {:?}, which reads as a field — pass fields as separate arguments",
                &text[tok.start..tok.end]
            );
        }
    }
    let mut body_lines = Vec::new();
    if let Some((title, rest)) = queue::split_title(&text, repo.cfg.title_max) {
        println!(
            "  (text over {} chars: the rest went to the body)",
            repo.cfg.title_max
        );
        text = title;
        body_lines = queue::body_lines(&rest, 100);
    }
    body_lines.extend(
        body.map(|b| queue::body_lines(&b, usize::MAX))
            .unwrap_or_default(),
    );
    let section = repo.cfg.section_for(&lane);
    let done = repo.cfg.done_section.clone();
    let minted = std::cell::Cell::new(0);
    let prefix = repo.cfg.commit_prefix.clone();
    let short_text = truncate(&text, 60);
    store::transact(
        repo,
        |ctx| format!("{prefix}: add #{} — {short_text}", ctx.next_id),
        &[],
        |_| Ok(()),
        |f, ctx| {
            let mut line = format!("- [ ] #{} {text}", ctx.next_id);
            for fld in &fields {
                line.push(' ');
                line.push_str(fld);
            }
            let mut block = vec![line];
            block.extend(body_lines.iter().cloned());
            f.queue.insert(block, &section, Some(&done));
            minted.set(ctx.next_id);
            Ok(())
        },
    )?;
    println!("  added #{}", minted.get());
    Ok(())
}

fn set(repo: &Repo, args: &[String]) -> Res<()> {
    let usage = "usage: 5w set <id> <area|level|lane|needs|branch> <value|->";
    let id = parse_id(args.first().ok_or(usage)?)?;
    let field = args.get(1).ok_or(usage)?.as_str();
    let value = args.get(2).ok_or(usage)?.as_str();
    let q = Q::load(repo)?;
    let t = q.get(id)?;
    // A closed task's fields are its record; its branch is what ship keys on.
    if t.state == State::Done {
        bail!("#{id} is closed; `{} open {id}` first", repo.cfg.cmd_tasks);
    }
    let ids: HashSet<u64> = q.tasks.iter().chain(&q.archived).map(|t| t.id).collect();
    let (kind, v) = match field {
        "area" => (Kind::Area, value.trim_start_matches('@').to_string()),
        "level" => (Kind::Level, value.trim_start_matches('!').to_string()),
        "lane" => (Kind::Lane, value.trim_start_matches('>').to_string()),
        "needs" => (Kind::Needs, value.trim_start_matches("needs:").to_string()),
        "branch" => (Kind::Branch, value.to_string()),
        _ => bail!("{usage}"),
    };
    let clear = value == "-";
    if !clear {
        let spelled = match kind {
            Kind::Area => format!("@{v}"),
            Kind::Level => format!("!{v}"),
            Kind::Lane => format!(">{v}"),
            Kind::Needs => format!("needs:{v}"),
            _ => format!("branch:{v}"),
        };
        if validate_field(repo, &spelled, &ids)? != kind {
            bail!("{value:?} is not a valid {field}");
        }
    }
    let to = (kind == Kind::Lane).then(|| {
        repo.cfg
            .section_for(if clear { &repo.cfg.default_lane } else { &v })
    });
    let msg = format!("{}: set #{id} {field} {value}", repo.cfg.commit_prefix);
    let done = repo.cfg.done_section.clone();
    let tasks_cmd = repo.cfg.cmd_tasks.clone();
    store::transact(
        repo,
        |_| msg.clone(),
        &[id],
        |c| open_only(c, id, &tasks_cmd),
        |f, _| {
            f.queue.update(
                id,
                |l| queue::set_field(l, kind, (!clear).then_some(v.as_str())),
                to.as_deref(),
                Some(&done),
            )
        },
    )
}

fn submit(repo: &Repo, args: &[String]) -> Res<()> {
    let id = parse_id(args.first().ok_or("usage: 5w submit <id> [branch]")?)?;
    let q = Q::load(repo)?;
    let t = q.get(id)?;
    if t.state == State::Done {
        bail!("#{id} is closed");
    }
    let branch = match (args.get(1), &t.branch) {
        (Some(b), Some(tb)) if b != tb => bail!("#{id} names branch {tb}, not {b}"),
        (Some(b), _) => b.clone(),
        (None, Some(tb)) => tb.clone(),
        (None, None) => match git::current_branch(&repo.cwd) {
            Some(b) if b != repo.trunk => b,
            _ => bail!("name the branch: 5w submit {id} <branch>"),
        },
    };
    if !git::branch_exists(&repo.primary, &branch) {
        bail!("no branch {branch}");
    }
    if let Some(wt) = git::worktree_of(&repo.primary, &branch)?
        && git::dirty(&wt)?
    {
        bail!(
            "{branch} has uncommitted files in {}; commit, then submit",
            wt.display()
        );
    }
    let tip =
        git::rev(&repo.primary, &format!("refs/heads/{branch}")).ok_or("cannot resolve branch")?;
    submit_at(repo, t, &branch, &tip)
}

/// Submit `t` as `branch` at `tip`: the commit `submit` and `ci --event submit` share.
pub fn submit_at(repo: &Repo, t: &Task, branch: &str, tip: &str) -> Res<()> {
    let id = t.id;
    let branch = branch.to_string();
    let ahead = git::git(
        &repo.primary,
        &["rev-list", "--count", &format!("{}..{tip}", repo.trunk)],
    )?;
    if ahead == "0" {
        eprintln!("warning: {branch} has nothing {} lacks", repo.trunk);
    }
    warn_not_delegable(repo, t, "submit it");
    let sha = short(tip).to_string();
    let msg = format!("{}: submit #{id} for review", repo.cfg.commit_prefix);
    let tasks_cmd = repo.cfg.cmd_tasks.clone();
    store::transact(
        repo,
        |_| msg.clone(),
        &[id],
        |c| {
            open_only(c, id, &tasks_cmd)?;
            match &committed(c, id)?.branch {
                Some(b) if *b != branch => bail!("#{id} now names branch {b}, not {branch}"),
                _ => Ok(()),
            }
        },
        |f, _| {
            f.queue.update(
                id,
                |l| {
                    let l = queue::set_mark(l, State::Review);
                    let l = queue::set_field(&l, Kind::Branch, Some(&branch));
                    queue::set_field(&l, Kind::Submitted, Some(&sha))
                },
                None,
                None,
            )
        },
    )?;
    println!("  #{id} submitted — {branch} at {sha}");
    Ok(())
}

fn review(repo: &Repo, args: &[String]) -> Res<()> {
    let o = opts(args)?;
    let q = Q::load(repo)?;
    let mut found = None;
    for t in q.tasks.iter().filter(|t| t.state == State::Review) {
        found = found.or(Some(t.id));
        if o.ids {
            println!("{}", t.id);
            continue;
        }
        let mut notes = Vec::new();
        let b = t.branch.as_deref();
        let tip = b.and_then(|b| git::rev(&repo.primary, &format!("refs/heads/{b}")));
        if b.is_some() && tip.is_none() {
            notes.push("branch is gone".to_string());
        }
        let mut moved = None;
        if let (Some(tip), Some(sub)) = (&tip, &t.submitted) {
            moved = Some("0".to_string());
            if !tip.starts_with(sub.as_str()) {
                let n = git::opt(
                    &repo.primary,
                    &["rev-list", "--count", &format!("{sub}..{tip}")],
                );
                notes.push(format!(
                    "moved since submit (+{} commits)",
                    n.as_deref().unwrap_or("?")
                ));
                moved = n;
            }
        }
        let (mut stat, mut behind) = (None, None);
        if let (Some(b), Some(_)) = (b, &tip) {
            let range = format!("{}...{b}", repo.trunk);
            stat = git::opt(&repo.primary, &["diff", "--shortstat", &range]);
            behind = git::opt(
                &repo.primary,
                &["rev-list", "--count", &format!("{b}..{}", repo.trunk)],
            );
        }
        if o.json {
            let num = |v: &Option<String>| v.clone().unwrap_or("null".into());
            let j = q.json(t, o.full, true);
            println!(
                "{},\"tip\":{},\"moved\":{},\"diff\":{},\"behind\":{}}}",
                &j[..j.len() - 1],
                tip.as_deref().map(js).unwrap_or("null".into()),
                num(&moved),
                stat.as_deref()
                    .map(|s| js(s.trim()))
                    .unwrap_or("null".into()),
                num(&behind),
            );
            continue;
        }
        q.row(t, &notes.join(", "), &o);
        if let (Some(b), Some(_)) = (b, &tip) {
            let range = format!("{}...{b}", repo.trunk);
            let behind = match behind.as_deref() {
                Some(n) if n != "0" && !n.is_empty() => format!(" · {n} behind"),
                _ => String::new(),
            };
            println!(
                "    {} · git diff {range}{behind}",
                stat.as_deref().unwrap_or_default().trim()
            );
        }
    }
    if o.json || o.ids {
        return Ok(());
    }
    match found {
        Some(id) => println!("→ {} accept {id}", repo.cfg.cmd_tasks),
        None => println!("(nothing submitted)"),
    }
    let checklist = &repo.cfg.checklist;
    if !checklist.is_empty() {
        if o.flags.iter().any(|f| f == "--checklist") || (!o.compact && found.is_some()) {
            println!("checklist:");
            for (i, c) in checklist.iter().enumerate() {
                println!("  {}. {c}", i + 1);
            }
        } else if found.is_some() {
            println!("(checklist: 5w review --checklist)");
        }
    }
    Ok(())
}

fn accept(repo: &Repo, args: &[String]) -> Res<()> {
    let usage = "usage: 5w accept <id>... [--at <rev>] [--force]";
    let mut ids = Vec::new();
    let mut at = None;
    let mut force = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--at" => {
                at = Some(args.get(i + 1).ok_or(usage)?.clone());
                i += 1;
            }
            "--force" => force = true,
            a if a.starts_with("--") => return Err(unknown_flag(repo, "accept", a)),
            a => ids.push(parse_id(a)?),
        }
        i += 1;
    }
    if ids.is_empty() {
        bail!("{usage}");
    }
    if at.is_some() && ids.len() > 1 {
        bail!("--at names one reviewed commit: accept one id with it");
    }
    // Each id is its own accept, with every check and its own commit. The
    // first refusal stops the rest, as ship does: what follows may depend on it.
    let mut accepted: Vec<u64> = Vec::new();
    for (n, &id) in ids.iter().enumerate() {
        if let Err(e) = accept_one(repo, id, at.as_deref(), force) {
            // In a batch nothing is committed, so there is nothing to tally.
            if ids.len() == 1 || store::batching() {
                return Err(e);
            }
            let done = match accepted.is_empty() {
                true => "none accepted".to_string(),
                false => format!("accepted {}", ids_str(&accepted)),
            };
            let rest = match &ids[n + 1..] {
                [] => String::new(),
                r => format!("; not tried {}", ids_str(r)),
            };
            bail!("{e} ({done}{rest})");
        }
        accepted.push(id);
    }
    Ok(())
}

pub fn accept_one(repo: &Repo, id: u64, at: Option<&str>, force: bool) -> Res<()> {
    let tasks = &repo.cfg.cmd_tasks;
    let q = Q::load(repo)?;
    let t = q.get(id)?;
    match t.state {
        State::Done => bail!("#{id} is already closed"),
        State::Open if !force => {
            bail!(
                "#{id} was never submitted — worker: `{tasks} submit {id} <branch>`; self-done: `{tasks} done {id} --self`"
            )
        }
        _ => {}
    }
    warn_not_delegable(repo, t, "accept it");
    let mut reviewed = None;
    if let Some(b) = &t.branch {
        let tip = git::rev(&repo.primary, &format!("refs/heads/{b}"));
        match (at, tip) {
            (Some(r), _) => {
                reviewed =
                    Some(git::rev(&repo.primary, r).ok_or_else(|| format!("cannot resolve {r}"))?)
            }
            (None, Some(tip)) => {
                // The review was of the submitted commit; later ones are unread.
                if let Some(sub) = &t.submitted
                    && !tip.starts_with(sub.as_str())
                    && !force
                {
                    let n = git::opt(
                        &repo.primary,
                        &["rev-list", "--count", &format!("{sub}..{tip}")],
                    )
                    .unwrap_or("?".into());
                    bail!(
                        "{b} gained {n} commit(s) after it was submitted at {sub}: `git log {sub}..{b}`; once reviewed, `{tasks} accept {id} --at {}`",
                        short(&tip)
                    );
                }
                reviewed = Some(tip);
            }
            (None, None) if force => {
                eprintln!("warning: branch {b} is gone; no reviewed commit recorded")
            }
            (None, None) => bail!(
                "branch {b} is gone, so there is nothing to record as reviewed (--force if it landed another way)"
            ),
        }
    }
    let sha = reviewed.as_deref().map(|r| short(r).to_string());
    let msg = format!("{}: accept #{id}", repo.cfg.commit_prefix);
    let done = repo.cfg.done_section.clone();
    let seen = (t.state, t.submitted.clone(), t.branch.clone());
    store::transact(
        repo,
        |_| msg.clone(),
        &[id],
        |c| {
            let c = committed(c, id)?;
            if (c.state, c.submitted.clone(), c.branch.clone()) != seen {
                bail!("#{id} differs on the trunk from what you read — look again");
            }
            if c.state != State::Review && !force {
                bail!("#{id} is not submitted on the trunk");
            }
            Ok(())
        },
        |f, _| {
            f.queue.update(
                id,
                |l| {
                    let l = queue::set_mark(l, State::Done);
                    let l = queue::set_field(&l, Kind::Rework, None);
                    let l = queue::set_field(&l, Kind::Via, Some("review"));
                    queue::set_field(&l, Kind::Reviewed, sha.as_deref())
                },
                Some(&done),
                None,
            )
        },
    )?;
    println!(
        "  #{id} accepted{}",
        sha.map(|s| format!(" at {s}")).unwrap_or_default()
    );
    Ok(())
}

fn reject(repo: &Repo, args: &[String]) -> Res<()> {
    let usage = "usage: 5w reject <id> <reason...>";
    let id = parse_id(args.first().ok_or(usage)?)?;
    let reason = args[1..].join(" ");
    if reason.trim().is_empty() {
        bail!("a rejection needs a reason — the next attempt reads it first");
    }
    let tasks = &repo.cfg.cmd_tasks;
    let q = Q::load(repo)?;
    match q.get(id)?.state {
        State::Done => bail!(
            "#{id} is closed, not pending review, so there is nothing to reject — to redo it: `{tasks} open {id}`, then the worker submits"
        ),
        State::Open => bail!(
            "#{id} was never submitted, so there is nothing to reject — worker: `{tasks} submit {id} <branch>`"
        ),
        State::Review => {}
    }
    let msg = format!("{}: reject #{id}", repo.cfg.commit_prefix);
    store::transact(
        repo,
        |_| msg.clone(),
        &[id],
        |c| {
            if committed(c, id)?.state != State::Review {
                bail!("#{id} is not submitted on the trunk");
            }
            Ok(())
        },
        |f, _| {
            f.queue.update(
                id,
                |l| {
                    let l = queue::set_mark(l, State::Open);
                    let l = queue::set_field(&l, Kind::Submitted, None);
                    queue::set_field(&l, Kind::Rework, Some(reason.trim()))
                },
                None,
                None,
            )
        },
    )?;
    println!("  #{id} sent back");
    Ok(())
}

fn done(repo: &Repo, args: &[String]) -> Res<()> {
    let id = parse_id(args.first().ok_or("usage: 5w done <id> --<close>")?)?;
    let flag = args.get(1).map(|s| s.as_str()).unwrap_or("");
    // The flags `done` takes are the lanes' close words, from the config.
    if let Some(extra) = args.get(2)
        && extra.starts_with("--")
        && !repo.cfg.lanes.iter().any(|l| extra[2..] == l.close)
    {
        return Err(unknown_flag(repo, "done", extra));
    }
    let q = Q::load(repo)?;
    let t = q.get(id)?;
    if t.state == State::Done {
        bail!("#{id} is already closed");
    }
    let lane_name = q.lane(t).to_string();
    let Some(lane) = repo.cfg.lane(&lane_name) else {
        bail!("#{id} is on unknown lane >{lane_name}")
    };
    let want = format!("--{}", lane.close);
    if flag != want {
        let tasks = &repo.cfg.cmd_tasks;
        let review = if lane.delegable {
            format!("; delegated work: `{tasks} submit {id}` then `accept`")
        } else {
            String::new()
        };
        bail!(
            "#{id} is >{lane_name}: close with `{tasks} done {id} {want}` (records via:{}){review}",
            lane.close
        );
    }
    if args.len() > 2 {
        bail!(
            "done takes one close flag: `{} done {id} {want}`",
            repo.cfg.cmd_tasks
        );
    }
    if t.state == State::Review {
        eprintln!(
            "warning: #{id} was submitted; closing via:{} instead of review",
            lane.close
        );
    }
    let close = lane.close.clone();
    let msg = format!("{}: close #{id} via:{close}", repo.cfg.commit_prefix);
    let done = repo.cfg.done_section.clone();
    let tasks_cmd = repo.cfg.cmd_tasks.clone();
    let default_lane = repo.cfg.default_lane.clone();
    store::transact(
        repo,
        |_| msg.clone(),
        &[id],
        |c| {
            open_only(c, id, &tasks_cmd)?;
            let l = committed(c, id)?
                .lane
                .clone()
                .unwrap_or(default_lane.clone());
            if l != lane_name {
                bail!("#{id} is >{l} on the trunk, not >{lane_name} — look again");
            }
            Ok(())
        },
        |f, _| {
            f.queue.update(
                id,
                |l| {
                    let l = queue::set_mark(l, State::Done);
                    let l = queue::set_field(&l, Kind::Rework, None);
                    queue::set_field(&l, Kind::Via, Some(&close))
                },
                Some(&done),
                None,
            )
        },
    )?;
    println!("  #{id} closed via:{close}");
    Ok(())
}

fn reopen(repo: &Repo, id: &str) -> Res<()> {
    let id = parse_id(id)?;
    let q = Q::load(repo)?;
    let t = q.get(id)?;
    if q.is_archived(id) {
        bail!(
            "#{id} is archived; move its block from {} back to {} by hand",
            repo.cfg.archive,
            repo.cfg.file
        );
    }
    let section = repo.cfg.section_for(q.lane(t));
    let msg = format!("{}: reopen #{id}", repo.cfg.commit_prefix);
    let done = repo.cfg.done_section.clone();
    store::transact(
        repo,
        |_| msg.clone(),
        &[id],
        |c| committed(c, id).map(|_| ()),
        |f, _| {
            f.queue.update(
                id,
                |l| {
                    let l = queue::set_mark(l, State::Open);
                    let l = queue::set_field(&l, Kind::Via, None);
                    let l = queue::set_field(&l, Kind::Reviewed, None);
                    queue::set_field(&l, Kind::Submitted, None)
                },
                Some(&section),
                Some(&done),
            )
        },
    )?;
    println!("  #{id} reopened");
    Ok(())
}

pub const ARCHIVE_HEADER: &str = "# Archive\n\nClosed tasks moved out of the queue by `5w archive`. Their ids stay taken, they still\nsatisfy `needs:`, and ship still reads their `branch:` and `reviewed:`.\n";

/// Move every closed task out of the queue, in one commit touching both files.
/// Only rows closed on the trunk are committed and counted; a row closed only in
/// the checkout moves there, and with none closed on the trunk nothing is committed.
fn archive(repo: &Repo) -> Res<()> {
    let q = Q::load(repo)?;
    let closed = |t: &Task| t.state == State::Done;
    if !q.tasks.iter().any(closed)
        && !queue::parse(&repo.committed()?.unwrap_or_default())
            .iter()
            .any(closed)
    {
        println!("  nothing closed to archive");
        return Ok(());
    }
    // Counted where the op first runs, on the committed copy under the lock:
    // a row closed on the trunk after any earlier read is still counted.
    let moved: std::cell::RefCell<Option<Vec<u64>>> = std::cell::RefCell::new(None);
    let done = repo.cfg.done_section.clone();
    store::transact(
        repo,
        |_| {
            format!(
                "{}: archive {} closed tasks",
                repo.cfg.commit_prefix,
                moved.borrow().as_ref().map_or(0, Vec::len)
            )
        },
        &[],
        |_| Ok(()),
        |f, _| {
            if moved.borrow().is_none() {
                let ids = queue::parse(&f.queue.text())
                    .iter()
                    .filter(|t| closed(t))
                    .map(|t| t.id)
                    .collect();
                *moved.borrow_mut() = Some(ids);
            }
            let blocks = f.queue.take(|t| t.state == State::Done);
            if !blocks.is_empty() && f.archive.lines.iter().all(|l| l.trim().is_empty()) {
                f.archive = queue::Doc::new(ARCHIVE_HEADER);
            }
            for b in blocks {
                f.archive.insert(b, &done, None);
            }
            Ok(())
        },
    )?;
    let moved = moved.into_inner().unwrap_or_default();
    // A row closed on the trunk but open in the checkout was reopened by hand:
    // it is archived on the trunk and the checkout keeps its copy.
    let reopened: Vec<u64> = moved
        .iter()
        .copied()
        .filter(|id| {
            q.tasks
                .iter()
                .any(|t| t.id == *id && t.state != State::Done)
        })
        .collect();
    match reopened.as_slice() {
        [] => {}
        [id] => println!(
            "  note: #{id} is closed on {} but open in the checkout — `{} reopen {id}` to reopen it on {}",
            repo.trunk, repo.cfg.cmd_tasks, repo.trunk
        ),
        ids => println!(
            "  note: {} are closed on {} but open in the checkout — `{} reopen <id>` to reopen them on {}",
            ids.iter()
                .map(|id| format!("#{id}"))
                .collect::<Vec<_>>()
                .join(", "),
            repo.trunk,
            repo.cfg.cmd_tasks,
            repo.trunk
        ),
    }
    match moved.len() {
        0 => println!("  nothing closed on {} to archive", repo.trunk),
        n => println!("  archived {n} → {}", repo.cfg.archive),
    }
    Ok(())
}

/// Shorten over-long titles into title + body, in one commit.
fn split(repo: &Repo, args: &[String]) -> Res<()> {
    let all = args.iter().any(|a| a == "--all");
    let max = repo.cfg.title_max;
    if max == 0 {
        bail!("title_max is 0 in the config — nothing to split to");
    }
    let q = Q::load(repo)?;
    let ids: Vec<u64> = q
        .tasks
        .iter()
        .filter(|t| (all || t.state != State::Done) && t.text.chars().count() > max)
        .map(|t| t.id)
        .collect();
    if ids.is_empty() {
        println!("  no titles over {max} chars");
        return Ok(());
    }
    let msg = format!(
        "{}: split {} over-long titles",
        repo.cfg.commit_prefix,
        ids.len()
    );
    store::transact(
        repo,
        |_| msg.clone(),
        &[],
        |_| Ok(()),
        |f, _| {
            for &id in &ids {
                // A copy that lacks the task (not yet committed) is left alone.
                if f.queue.block(id).is_some() {
                    f.queue.split_title(id, max)?;
                }
            }
            Ok(())
        },
    )?;
    println!("  split {} titles", ids.len());
    Ok(())
}

/// The write commands `batch` takes: the ones that edit named rows.
const BATCH_COMMANDS: &[&str] = &[
    "add", "set", "submit", "accept", "reject", "done", "open", "reopen",
];

/// Several queue edits in one commit — one signature for a round of review.
/// Each stdin line is a write command as it would follow `5w`; each runs with
/// every check it has alone, under one lock held for the whole batch and
/// against the queue as the trunk and the lines before it leave it. The first
/// refusal ends the batch with nothing committed.
fn batch(repo: &Repo, args: &[String]) -> Res<()> {
    let usage = "usage: 5w batch < edits (one write command per line, e.g. `accept 4`)";
    if !args.is_empty() {
        bail!("{usage}");
    }
    let mut input = String::new();
    std::io::stdin()
        .read_to_string(&mut input)
        .map_err(|e| e.to_string())?;
    let mut lines = Vec::new();
    for (n, l) in input.lines().enumerate() {
        let at = n + 1;
        let words = split_words(l).map_err(|e| format!("batch line {at}: {e}"))?;
        let Some(cmd) = words.first() else { continue };
        if !BATCH_COMMANDS.contains(&cmd.as_str()) {
            bail!(
                "batch line {at}: {cmd} is not a batch edit (takes {})",
                BATCH_COMMANDS.join(" ")
            );
        }
        if words.windows(2).any(|w| w[0] == "--body" && w[1] == "-") {
            bail!("batch line {at}: stdin holds the batch; give --body its text inline");
        }
        lines.push((at, words));
    }
    if lines.is_empty() {
        bail!("{usage}");
    }
    store::batch(repo, || {
        for (at, words) in &lines {
            run(repo, &words[0], &words[1..])
                .map_err(|e| format!("batch line {at}: {e}; nothing committed"))?;
        }
        Ok(())
    })
}

/// A line split into words as a shell would for these commands: whitespace
/// separates, '…' and "…" quote, and a backslash escapes the next character
/// (outside single quotes).
fn split_words(line: &str) -> Res<Vec<String>> {
    let mut words = Vec::new();
    let (mut word, mut started) = (String::new(), false);
    let mut quote: Option<char> = None;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some('"') | None, '\\') => {
                word.push(chars.next().ok_or("a trailing \\ escapes nothing")?);
                started = true;
            }
            (Some(_), c) => word.push(c),
            (None, '\'' | '"') => {
                quote = Some(c);
                started = true;
            }
            (None, c) if c.is_whitespace() => {
                if started {
                    words.push(std::mem::take(&mut word));
                    started = false;
                }
            }
            (None, c) => {
                word.push(c);
                started = true;
            }
        }
    }
    if let Some(q) = quote {
        bail!("unclosed {q}");
    }
    if started {
        words.push(word);
    }
    Ok(words)
}

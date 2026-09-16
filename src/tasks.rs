use crate::bail;
use crate::git;
use crate::queue::{self, Kind, State, Task};
use crate::store::{self, Repo};
use crate::util::{Res, Sty, parse_id, short, truncate};
use std::collections::HashSet;
use std::io::Read;

pub const USAGE: &str = "\
usage: 5w <command> [args]

queue
  ready [filters]       Delegable work with every blocker done, by complexity (default)
  ls [filters]          Every open task, blocked ones marked
  blocked               Open tasks waiting on something, with what
  levels                Counts per complexity and per lane
  all                   Open, submitted and done
  show <id>             One task: fields, body, blockers, dependents
  delegate <id>         A brief to hand an agent
  branch <id>           The task's branch, or a suggestion
  doctor                Check the file: duplicate ids, unknown blockers and lanes

changes (each one commits itself to the trunk, and only itself)
  add \"<text>\" [fields] [--body <text>|--body -]
  set <id> <area|level|lane|needs|branch> <value|->
  submit <id> [branch]  Worker: finished, hand it back. Records the tip reviewed against
  review                Supervisor: submitted work, with diffstat and drift since submit
  accept <id> [--at <rev>] [--force]
                        Reviewed and good. Records the reviewed commit; ship checks it
  reject <id> <reason>  Back to open, carrying the objection into the next brief
  done <id> --<close>   Close without review; the flag must match the lane (--self, --decided)
  open <id>             Reopen, closure record dropped

branches
  wt <new|add|ls|path|rm|link|install|setup>   see `5w wt help`
  ship <branch> [--sync] [--squash [-m msg]] [--force]
  init                  Write .5w.toml and TASKS.md if missing, and commit them

filters, any order: @area !level >lane — or area:x level:n lane:x, which need no quoting
ids: #14 or 14 — the bare number needs no quoting in zsh";

struct Filters {
    area: Option<String>,
    level: Option<u8>,
    lane: Option<String>,
}

fn filters(args: &[String]) -> Res<Filters> {
    let mut f = Filters {
        area: None,
        level: None,
        lane: None,
    };
    for a in args {
        if let Some(v) = a.strip_prefix('@').or_else(|| a.strip_prefix("area:")) {
            f.area = Some(v.into());
        } else if let Some(v) = a.strip_prefix('!').or_else(|| a.strip_prefix("level:")) {
            f.level = Some(v.parse().map_err(|_| format!("bad level {a}"))?);
        } else if let Some(v) = a.strip_prefix('>').or_else(|| a.strip_prefix("lane:")) {
            f.lane = Some(v.into());
        } else {
            bail!("unknown filter {a:?} (want @area, !level, >lane)");
        }
    }
    Ok(f)
}

struct Q<'a> {
    repo: &'a Repo,
    tasks: Vec<Task>,
    done: HashSet<u64>,
    sty: Sty,
}

impl<'a> Q<'a> {
    fn load(repo: &'a Repo) -> Res<Q<'a>> {
        let text = repo.load()?;
        let tasks = queue::parse(&text);
        let dup = queue::duplicates(&tasks);
        if !dup.is_empty() {
            let mut msg = format!(
                "{} carries an id twice — every state and count would be wrong:\n",
                repo.cfg.file
            );
            for (id, a, b) in dup {
                msg += &format!("  #{id:<4} line {a} and line {b}\n");
            }
            msg +=
                "Delete the stale line, or renumber the younger one if they are different tasks.";
            return Err(msg);
        }
        let done = tasks
            .iter()
            .filter(|t| t.state == State::Done)
            .map(|t| t.id)
            .collect();
        Ok(Q {
            repo,
            tasks,
            done,
            sty: Sty::new(),
        })
    }

    fn get(&self, id: u64) -> Res<&Task> {
        self.tasks
            .iter()
            .find(|t| t.id == id)
            .ok_or_else(|| format!("no task #{id}"))
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

    fn matches(&self, t: &Task, f: &Filters) -> bool {
        f.area.as_ref().is_none_or(|a| t.area.as_ref() == Some(a))
            && f.level.is_none_or(|l| t.level == Some(l))
            && f.lane.as_ref().is_none_or(|l| self.lane(t) == l)
    }

    fn row(&self, t: &Task, note: &str) {
        let s = &self.sty;
        let area = t.area.as_ref().map(|a| format!("@{a}")).unwrap_or_default();
        let mut out = format!(
            "  {} {:<9} {}",
            s.bold(&format!("#{:<3}", t.id)),
            area,
            t.text
        );
        if let Some(b) = &t.branch {
            out += &format!(" {}", s.dim(&format!("[{b}]")));
        }
        let lane = self.lane(t);
        if lane != self.repo.cfg.default_lane {
            out += &format!(" {}", s.dim(&format!(">{lane}")));
        }
        if !t.body.is_empty() {
            out += &s.dim(" +body");
        }
        if !note.is_empty() {
            out += &format!(" {}", s.red(note));
        }
        println!("{out}");
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

pub fn run(repo: &Repo, cmd: &str, args: &[String]) -> Res<()> {
    match cmd {
        "ready" => ready(repo, args),
        "ls" | "list" => ls(repo, args),
        "blocked" => blocked(repo),
        "levels" => levels(repo),
        "all" => all(repo),
        "show" => show(repo, arg(args, 0, "show <id>")?),
        "delegate" => delegate(repo, arg(args, 0, "delegate <id>")?),
        "branch" => branch(repo, arg(args, 0, "branch <id>")?),
        "doctor" => doctor(repo),
        "add" => add(repo, args),
        "set" => set(repo, args),
        "submit" => submit(repo, args),
        "review" => review(repo),
        "accept" => accept(repo, args),
        "reject" => reject(repo, args),
        "done" => done(repo, args),
        "open" | "reopen" => reopen(repo, arg(args, 0, "open <id>")?),
        _ => bail!("unknown command: {cmd} (try: 5w help)"),
    }
}

fn arg<'a>(args: &'a [String], i: usize, usage: &str) -> Res<&'a str> {
    args.get(i)
        .map(|s| s.as_str())
        .ok_or_else(|| format!("usage: 5w {usage}"))
}

// --- reading ------------------------------------------------------------------------

fn ready(repo: &Repo, args: &[String]) -> Res<()> {
    let f = filters(args)?;
    let q = Q::load(repo)?;
    let mut shown = 0;
    for lvl in [Some(1), Some(2), Some(3), Some(4), None] {
        let mut header = false;
        for t in q
            .tasks
            .iter()
            .filter(|t| t.state == State::Open && t.level == lvl)
        {
            // Non-delegable lanes appear only when asked for by name.
            if f.lane.is_none() && !q.delegable(t) {
                continue;
            }
            if !q.matches(t, &f) || !q.unmet(t).is_empty() {
                continue;
            }
            if !header {
                let label = lvl.map(|l| format!("!{l}")).unwrap_or("!-".into());
                println!("\n{}  {}", q.sty.bold(&label), repo.cfg.tier(lvl));
                header = true;
            }
            let note = if t.rework.is_some() {
                format!("sent back — {} delegate {}", repo.cfg.cmd_tasks, t.id)
            } else {
                String::new()
            };
            q.row(t, &note);
            shown += 1;
        }
    }
    if shown == 0 {
        println!("(nothing ready under those filters)");
    }
    let open: Vec<&Task> = q.tasks.iter().filter(|t| t.state == State::Open).collect();
    let mut parts = vec![format!("{shown} ready")];
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
        parts.push(format!("{review} waiting on review"));
    }
    println!("\n{}", q.sty.dim(&parts.join(" · ")));
    Ok(())
}

fn ls(repo: &Repo, args: &[String]) -> Res<()> {
    let f = filters(args)?;
    let q = Q::load(repo)?;
    for t in q
        .tasks
        .iter()
        .filter(|t| t.state != State::Done && q.matches(t, &f))
    {
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
        q.row(t, &notes.join(", "));
    }
    Ok(())
}

fn blocked(repo: &Repo) -> Res<()> {
    let q = Q::load(repo)?;
    for t in q.tasks.iter().filter(|t| t.state == State::Open) {
        let u = q.unmet(t);
        if !u.is_empty() {
            q.row(t, &format!("blocked by {}", ids_str(&u)));
        }
    }
    Ok(())
}

fn levels(repo: &Repo) -> Res<()> {
    let q = Q::load(repo)?;
    let open: Vec<&Task> = q.tasks.iter().filter(|t| t.state == State::Open).collect();
    println!("By complexity (open, delegable lanes):");
    for lvl in [Some(1), Some(2), Some(3), Some(4), None] {
        let n = open
            .iter()
            .filter(|t| t.level == lvl && q.delegable(t))
            .count();
        if n > 0 {
            let label = lvl.map(|l| l.to_string()).unwrap_or("-".into());
            println!("  !{label}  {n:>3}  {}", repo.cfg.tier(lvl));
        }
    }
    println!("By lane:");
    for l in &repo.cfg.lanes {
        let n = open.iter().filter(|t| q.lane(t) == l.name).count();
        if n > 0 {
            println!("  >{:<7} {n:>3}", l.name);
        }
    }
    let unknown = open
        .iter()
        .filter(|t| repo.cfg.lane(q.lane(t)).is_none())
        .count();
    if unknown > 0 {
        println!("  {unknown} on lanes the config does not name — 5w doctor");
    }
    Ok(())
}

fn all(repo: &Repo) -> Res<()> {
    let q = Q::load(repo)?;
    for t in &q.tasks {
        let area = t.area.as_ref().map(|a| format!("@{a}")).unwrap_or_default();
        println!("  [{}] #{:<3} {:<9} {}", t.state.mark(), t.id, area, t.text);
    }
    Ok(())
}

fn show(repo: &Repo, id: &str) -> Res<()> {
    let q = Q::load(repo)?;
    let t = q.get(parse_id(id)?)?;
    println!("#{}  {}", t.id, t.text);
    for l in &t.body {
        println!("    {l}");
    }
    println!("  state:     {}", t.state.name());
    println!("  area:      {}", t.area.as_deref().unwrap_or("-"));
    println!(
        "  level:     {} — {}",
        t.level.map(|l| l.to_string()).unwrap_or("-".into()),
        repo.cfg.tier(t.level)
    );
    println!("  lane:      {}", q.lane(t));
    println!("  branch:    {}", t.branch.as_deref().unwrap_or("-"));
    if let Some(r) = &t.rework {
        println!("  rework:    {r}");
    }
    if let Some(s) = &t.submitted {
        println!("  submitted: at {s}");
    }
    if let Some(r) = &t.reviewed {
        println!("  reviewed:  at {r}");
    }
    if let Some(v) = &t.via {
        println!("  closed:    via:{v}");
    }
    let u = q.unmet(t);
    println!(
        "  needs:     {}{}",
        if t.needs.is_empty() {
            "-".into()
        } else {
            ids_str(&t.needs)
        },
        if u.is_empty() {
            String::new()
        } else {
            format!(" (unmet: {})", ids_str(&u))
        }
    );
    let deps: Vec<u64> = q
        .tasks
        .iter()
        .filter(|d| d.needs.contains(&t.id))
        .map(|d| d.id)
        .collect();
    if !deps.is_empty() {
        println!("  blocks:    {}", ids_str(&deps));
    }
    Ok(())
}

fn suggested_branch(t: &Task) -> String {
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

fn delegate(repo: &Repo, id: &str) -> Res<()> {
    let q = Q::load(repo)?;
    let t = q.get(parse_id(id)?)?;
    let cfg = &repo.cfg;
    match t.state {
        State::Done => bail!("#{} is already closed", t.id),
        State::Review => bail!("#{} is submitted and waiting on review, not on work", t.id),
        State::Open => {}
    }
    let lane_name = q.lane(t);
    let lane = cfg.lane(lane_name);
    if let Some(l) = lane.filter(|l| !l.delegable) {
        bail!(
            "#{} is >{}: {}",
            t.id,
            l.name,
            l.refuse.as_deref().unwrap_or("not delegable")
        );
    }
    let u = q.unmet(t);
    if !u.is_empty() {
        eprintln!("warning: #{} is blocked by {}", t.id, ids_str(&u));
    }
    let branch = t.branch.clone().unwrap_or_else(|| suggested_branch(t));
    let setup = if !git::branch_exists(&repo.primary, &branch) {
        format!("{} new {branch}", cfg.cmd_wt)
    } else if git::worktree_of(&repo.primary, &branch)?.is_some() {
        format!("# {branch} exists with a worktree — the last attempt is in it")
    } else {
        format!(
            "{} add {branch}    # the branch exists; this restores its worktree",
            cfg.cmd_wt
        )
    };
    let mut docs = cfg.context_docs.clone();
    if let Some(a) = &t.area {
        for d in &cfg.area_docs {
            if repo.primary.join(a).join(d).is_file() {
                docs.push(format!("{a}/{d}"));
            }
        }
    }
    println!("\nTask #{} — {}\n", t.id, t.text);
    for l in &t.body {
        println!("  {l}");
    }
    if !t.body.is_empty() {
        println!();
    }
    println!(
        "  complexity  !{}  ({})",
        t.level.map(|l| l.to_string()).unwrap_or("-".into()),
        cfg.tier(t.level)
    );
    println!(
        "  lane        >{lane_name}{}",
        lane.and_then(|l| l.note.as_ref())
            .map(|n| format!("  — {n}"))
            .unwrap_or_default()
    );
    println!(
        "  area        {}",
        t.area
            .as_ref()
            .map(|a| format!("{a}/"))
            .unwrap_or("(none)".into())
    );
    if !docs.is_empty() {
        println!("  context     {}", docs.join(", "));
    }
    if !cfg.conventions.is_empty() {
        println!("  conventions {}", cfg.conventions.join(", "));
    }
    if let Some(r) = &t.rework {
        println!("  REWORK      {r}");
        println!("              (the last attempt was sent back for this — read it first)");
    }
    println!("\n  {setup}\n  cd \"$({} path {branch})\"\n", cfg.cmd_wt);
    let footer = cfg.brief_footer.clone().unwrap_or_else(|| {
        "  Commit on that branch. Then hand it back for review — do not ship it and do\n  \
         not close it:\n\n    {tasks} submit {id} {branch}\n\n  \
         The supervisor reviews the branch and accepts or rejects it. Only an\n  \
         accepted task ships, and `{ship}` enforces that.\n"
            .into()
    });
    print!(
        "{}",
        footer
            .replace("{tasks}", &cfg.cmd_tasks)
            .replace("{ship}", &cfg.cmd_ship)
            .replace("{wt}", &cfg.cmd_wt)
            .replace("{id}", &t.id.to_string())
            .replace("{branch}", &branch)
    );
    Ok(())
}

fn doctor(repo: &Repo) -> Res<()> {
    let text = repo.load()?;
    let tasks = queue::parse(&text);
    let mut problems = 0;
    let mut say = |s: String| {
        println!("  {s}");
        problems += 1;
    };
    for (id, a, b) in queue::duplicates(&tasks) {
        say(format!("#{id} appears twice: lines {a} and {b}"));
    }
    let ids: HashSet<u64> = tasks.iter().map(|t| t.id).collect();
    let cfg = &repo.cfg;
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
    }
    if queue::unclosed_fence(&text) {
        say("a ``` fence is never closed — every task after it is invisible".into());
    }
    if repo.trunk_checkout()?.is_some()
        && let Some(c) = repo.committed()?
        && c != text
    {
        println!(
            "  (note: {} has uncommitted edits in the {} checkout)",
            cfg.file, repo.trunk
        );
    }
    if problems == 0 {
        println!("  ok — {} tasks", tasks.len());
        Ok(())
    } else {
        bail!("{problems} problem(s)")
    }
}

// --- writing -------------------------------------------------------------------------

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
                bail!(
                    "unknown lane {w} (configured: {})",
                    repo.cfg
                        .lanes
                        .iter()
                        .map(|l| format!(">{}", l.name))
                        .collect::<Vec<_>>()
                        .join(" ")
                );
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
        _ => bail!(
            "not a field: {w:?} (want @area !n >lane needs:#a,#b branch:x — or area:x level:n lane:x)"
        ),
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
    let usage =
        "usage: 5w add \"<text>\" [@area] [!n] [>lane] [needs:#1,#2] [--body <text>|--body -]";
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
        if text.is_none() {
            if a.starts_with('-') {
                bail!("add takes text first, not a flag: {a}\n{usage}");
            }
            text = Some(a.clone());
        } else {
            fields.push(normalise_field(a));
        }
        i += 1;
    }
    let text = text.ok_or(usage)?;
    if text.contains(['\n', '\r']) {
        bail!("task text is one line; put the rest in --body");
    }
    let current = queue::parse(&repo.load()?);
    let ids: HashSet<u64> = current.iter().map(|t| t.id).collect();
    let mut lane = repo.cfg.default_lane.clone();
    for f in &fields {
        if validate_field(repo, f, &ids)? == Kind::Lane {
            lane = f[1..].to_string();
        }
    }
    // Stray field syntax inside the text would be parsed as a field later.
    for tok in queue::tokenize(&text) {
        if tok.kind != Kind::Text {
            bail!(
                "the text contains {:?}, which reads as a field — pass fields as separate arguments",
                &text[tok.start..tok.end]
            );
        }
    }
    let section = repo.cfg.section_for(&lane);
    let done = repo.cfg.done_section.clone();
    let body_lines: Vec<String> = body
        .map(|b| {
            b.lines()
                .filter(|l| !l.trim().is_empty())
                .map(|l| format!("  {l}"))
                .collect()
        })
        .unwrap_or_default();
    let minted = std::cell::Cell::new(0);
    let prefix = repo.cfg.commit_prefix.clone();
    let short_text = truncate(&text, 60);
    store::transact(
        repo,
        |ctx| format!("{prefix}: add #{} — {short_text}", ctx.next_id),
        &[],
        |_| Ok(()),
        |doc, ctx| {
            let mut line = format!("- [ ] #{} {text}", ctx.next_id);
            for f in &fields {
                line.push(' ');
                line.push_str(f);
            }
            let mut block = vec![line];
            block.extend(body_lines.iter().cloned());
            doc.insert(block, &section, Some(&done));
            minted.set(ctx.next_id);
            Ok(())
        },
    )?;
    println!(
        "  added #{} under {}",
        minted.get(),
        section.trim_start_matches("## ")
    );
    Ok(())
}

fn set(repo: &Repo, args: &[String]) -> Res<()> {
    let usage = "usage: 5w set <id> <area|level|lane|needs|branch> <value|->";
    let id = parse_id(args.first().ok_or(usage)?)?;
    let field = args.get(1).ok_or(usage)?.as_str();
    let value = args.get(2).ok_or(usage)?.as_str();
    let q = Q::load(repo)?;
    let t = q.get(id)?;
    // A closed task's fields are its record. Rewriting one — above all its
    // branch, which is what ship's gate keys on — would let a closure authorise
    // work it never saw.
    if t.state == State::Done {
        bail!(
            "#{id} is closed; its fields are the record. `{} open {id}` first",
            repo.cfg.cmd_tasks
        );
    }
    let ids: HashSet<u64> = q.tasks.iter().map(|t| t.id).collect();
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
    let mut to = None;
    if kind == Kind::Lane && t.state != State::Done {
        let lane = if clear {
            repo.cfg.default_lane.clone()
        } else {
            v.clone()
        };
        to = Some(repo.cfg.section_for(&lane));
    }
    let msg = format!("{}: set #{id} {field} {value}", repo.cfg.commit_prefix);
    let done = repo.cfg.done_section.clone();
    let tasks_cmd = repo.cfg.cmd_tasks.clone();
    store::transact(
        repo,
        |_| msg.clone(),
        &[id],
        |c| open_only(c, id, &tasks_cmd),
        |doc, _| {
            doc.update(
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
        bail!("#{id} is already closed");
    }
    let branch = match (args.get(1), &t.branch) {
        (Some(b), Some(tb)) if b != tb => {
            bail!("#{id} names branch {tb}, not {b} — `5w set {id} branch {b}` if that changed")
        }
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
            "{branch} has uncommitted or untracked files in {} — work that is not committed is not handed in. Commit it, then submit.",
            wt.display()
        );
    }
    let tip =
        git::rev(&repo.primary, &format!("refs/heads/{branch}")).ok_or("cannot resolve branch")?;
    let ahead = git::git(
        &repo.primary,
        &["rev-list", "--count", &format!("{}..{tip}", repo.trunk)],
    )?;
    if ahead == "0" {
        eprintln!("warning: {branch} has no commits that {} lacks", repo.trunk);
    }
    let sha = short(&tip).to_string();
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
        |doc, _| {
            doc.update(
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
    println!("  #{id} submitted for review — {branch} at {sha}");
    Ok(())
}

fn review(repo: &Repo) -> Res<()> {
    let q = Q::load(repo)?;
    let s = &q.sty;
    let mut found = false;
    println!("Waiting on review:");
    for t in q.tasks.iter().filter(|t| t.state == State::Review) {
        found = true;
        let mut notes = Vec::new();
        let b = t.branch.as_deref();
        let tip = b.and_then(|b| git::rev(&repo.primary, &format!("refs/heads/{b}")));
        if b.is_some() && tip.is_none() {
            notes.push("branch is gone".to_string());
        }
        if let (Some(tip), Some(sub)) = (&tip, &t.submitted)
            && !tip.starts_with(sub.as_str())
        {
            let n = git::opt(
                &repo.primary,
                &["rev-list", "--count", &format!("{sub}..{tip}")],
            )
            .unwrap_or("?".into());
            notes.push(format!("moved since submit (+{n} commits)"));
        }
        q.row(t, &notes.join(", "));
        if let (Some(b), Some(_)) = (b, &tip) {
            let range = format!("{}...{b}", repo.trunk);
            let stat =
                git::opt(&repo.primary, &["diff", "--shortstat", &range]).unwrap_or_default();
            let behind = git::opt(
                &repo.primary,
                &["rev-list", "--count", &format!("{b}..{}", repo.trunk)],
            )
            .unwrap_or_default();
            println!("        {}", s.dim(stat.trim()));
            let behind_note = if behind != "0" && !behind.is_empty() {
                format!("   ({behind} behind {})", repo.trunk)
            } else {
                String::new()
            };
            println!(
                "        {}",
                s.dim(&format!("git diff {range}{behind_note}"))
            );
        }
    }
    if !found {
        println!("  (nothing submitted)");
    } else if !repo.cfg.checklist.is_empty() {
        println!("\nEvery review checks:");
        for (i, c) in repo.cfg.checklist.iter().enumerate() {
            println!("  {}. {c}", i + 1);
        }
    }
    Ok(())
}

fn accept(repo: &Repo, args: &[String]) -> Res<()> {
    let usage = "usage: 5w accept <id> [--at <rev>] [--force]";
    let mut id = None;
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
            a if id.is_none() => id = Some(parse_id(a)?),
            a => bail!("unexpected {a:?}\n{usage}"),
        }
        i += 1;
    }
    let id = id.ok_or(usage)?;
    let q = Q::load(repo)?;
    let t = q.get(id)?;
    match t.state {
        State::Done => bail!("#{id} is already closed"),
        State::Open if !force => bail!(
            "#{id} was never submitted. Whoever did the work runs `{} submit {id} <branch>`;\n  \
             if you did it yourself, `{} done {id} --self`. (--force accepts anyway.)",
            repo.cfg.cmd_tasks,
            repo.cfg.cmd_tasks
        ),
        _ => {}
    }
    let mut reviewed = None;
    if let Some(b) = &t.branch {
        let tip = git::rev(&repo.primary, &format!("refs/heads/{b}"));
        match (&at, tip) {
            (Some(r), _) => {
                reviewed =
                    Some(git::rev(&repo.primary, r).ok_or_else(|| format!("cannot resolve {r}"))?)
            }
            (None, Some(tip)) => {
                // The review was of the submitted commit. A tip that moved since
                // carries commits nobody has looked at.
                if let Some(sub) = &t.submitted
                    && !tip.starts_with(sub.as_str())
                    && !force
                {
                    let log = git::opt(
                        &repo.primary,
                        &["log", "--oneline", &format!("{sub}..{tip}")],
                    )
                    .unwrap_or_default();
                    bail!(
                        "{b} gained commits after it was submitted at {sub}:\n{}\n\n  \
                             Review them, then accept what you reviewed:\n    {} accept {id} --at {}",
                        log.lines()
                            .map(|l| format!("    {l}"))
                            .collect::<Vec<_>>()
                            .join("\n"),
                        repo.cfg.cmd_tasks,
                        short(&tip)
                    );
                }
                reviewed = Some(tip);
            }
            (None, None) if force => {
                eprintln!("warning: branch {b} does not exist; no reviewed commit recorded")
            }
            (None, None) => bail!(
                "branch {b} does not exist, so there is no commit to record as reviewed — and a\n  \
                 closed row with none would authorise whatever branch later takes that name.\n  \
                 Find the work, or --force if it landed some other way."
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
                bail!(
                    "#{id} changed on the trunk while this ran (or differs from the working copy) — look again"
                );
            }
            if c.state != State::Review && !force {
                bail!("#{id} is not submitted on the trunk");
            }
            Ok(())
        },
        |doc, _| {
            doc.update(
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
        "  #{id} accepted (via:review){}",
        sha.map(|s| format!(" at {s}")).unwrap_or_default()
    );
    Ok(())
}

fn reject(repo: &Repo, args: &[String]) -> Res<()> {
    let usage = "usage: 5w reject <id> <reason...>";
    let id = parse_id(args.first().ok_or(usage)?)?;
    let reason = args[1..].join(" ");
    if reason.trim().is_empty() {
        bail!("a rejection needs a reason — it is what the next attempt reads first");
    }
    let q = Q::load(repo)?;
    let t = q.get(id)?;
    if t.state == State::Done {
        bail!("#{id} is closed; `{} open {id}` first", repo.cfg.cmd_tasks);
    }
    let msg = format!("{}: reject #{id}", repo.cfg.commit_prefix);
    let tasks_cmd = repo.cfg.cmd_tasks.clone();
    store::transact(
        repo,
        |_| msg.clone(),
        &[id],
        |c| open_only(c, id, &tasks_cmd),
        |doc, _| {
            doc.update(
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
    println!("  #{id} sent back: {}", reason.trim());
    Ok(())
}

fn done(repo: &Repo, args: &[String]) -> Res<()> {
    let id = parse_id(args.first().ok_or("usage: 5w done <id> --<close>")?)?;
    let flag = args.get(1).map(|s| s.as_str()).unwrap_or("");
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
        let how = if lane.delegable {
            format!(
                "  Delegable work goes through review:\n    {tasks} submit {id} <branch>   # whoever did it\n    {tasks} accept {id}            # the supervisor, after reading the diff\n\n"
            )
        } else {
            String::new()
        };
        bail!(
            "#{id} is on >{lane_name}. Closing it records via:{} and needs the flag that says so:\n\n{how}  {tasks} done {id} {want}\n\n  \
             No flag can check who is typing. It exists so closing is a deliberate act with a recorded meaning.",
            lane.close
        );
    }
    if t.state == State::Review {
        eprintln!(
            "warning: #{id} was submitted for review; closing it via:{} instead",
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
                bail!("#{id} is on >{l} on the trunk, not >{lane_name} — look again");
            }
            Ok(())
        },
        |doc, _| {
            doc.update(
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
    println!("  #{id} closed (via:{close})");
    Ok(())
}

fn reopen(repo: &Repo, id: &str) -> Res<()> {
    let id = parse_id(id)?;
    let q = Q::load(repo)?;
    let t = q.get(id)?;
    let section = repo.cfg.section_for(q.lane(t));
    let msg = format!("{}: reopen #{id}", repo.cfg.commit_prefix);
    let done = repo.cfg.done_section.clone();
    store::transact(
        repo,
        |_| msg.clone(),
        &[id],
        |c| committed(c, id).map(|_| ()),
        |doc, _| {
            doc.update(
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

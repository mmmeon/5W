//! `.5w.toml` — read with a small TOML subset parser, so the tool has no
//! dependencies. Supported: `[table]` headers, bare/quoted/dotted keys, basic and
//! literal strings (single and triple quoted), integers, booleans, and arrays of
//! those. That is everything the config uses.

use crate::bail;
use crate::util::Res;
use std::collections::BTreeMap;

#[derive(Clone, Debug)]
pub enum Val {
    Str(String),
    Int(i64),
    Bool(bool),
    Arr(Vec<Val>),
}

pub fn parse_toml(src: &str) -> Res<Vec<(String, Val)>> {
    let b = src.as_bytes();
    let mut i = 0;
    let mut table = String::new();
    let mut out = Vec::new();
    let line = |i: usize| src[..i.min(src.len())].matches('\n').count() + 1;
    loop {
        skip_blank(b, &mut i);
        if i >= b.len() {
            break;
        }
        if b[i] == b'[' {
            let Some(end) = src[i..].find(']') else {
                bail!("config line {}: unclosed [", line(i))
            };
            table = src[i + 1..i + end].trim().to_string();
            i += end + 1;
            eol(b, &mut i).map_err(|e| format!("config line {}: {e}", line(i)))?;
            continue;
        }
        let r = (|| -> Res<(String, Val)> {
            let key = parse_key(b, &mut i)?;
            skip_sp(b, &mut i);
            if b.get(i) != Some(&b'=') {
                bail!("expected = after {key}");
            }
            i += 1;
            skip_sp(b, &mut i);
            let v = parse_val(b, &mut i)?;
            eol(b, &mut i)?;
            Ok((key, v))
        })();
        let (key, v) = r.map_err(|e| format!("config line {}: {e}", line(i)))?;
        out.push((
            if table.is_empty() {
                key
            } else {
                format!("{table}.{key}")
            },
            v,
        ));
    }
    Ok(out)
}

fn skip_sp(b: &[u8], i: &mut usize) {
    while *i < b.len() && (b[*i] == b' ' || b[*i] == b'\t') {
        *i += 1;
    }
}

fn skip_blank(b: &[u8], i: &mut usize) {
    loop {
        while *i < b.len() && b[*i].is_ascii_whitespace() {
            *i += 1;
        }
        if *i < b.len() && b[*i] == b'#' {
            while *i < b.len() && b[*i] != b'\n' {
                *i += 1;
            }
        } else {
            return;
        }
    }
}

fn eol(b: &[u8], i: &mut usize) -> Res<()> {
    skip_sp(b, i);
    if *i < b.len() && b[*i] == b'#' {
        while *i < b.len() && b[*i] != b'\n' {
            *i += 1;
        }
    }
    if *i < b.len() && b[*i] == b'\r' {
        *i += 1;
    }
    if *i < b.len() && b[*i] != b'\n' {
        bail!("unexpected {:?}", b[*i] as char);
    }
    Ok(())
}

fn parse_key(b: &[u8], i: &mut usize) -> Res<String> {
    let mut key = String::new();
    loop {
        if b.get(*i) == Some(&b'"') {
            key.push_str(&basic(b, i)?);
        } else {
            let s = *i;
            while *i < b.len() && (b[*i].is_ascii_alphanumeric() || b[*i] == b'_' || b[*i] == b'-')
            {
                *i += 1;
            }
            if s == *i {
                bail!("expected a key");
            }
            key.push_str(std::str::from_utf8(&b[s..*i]).unwrap());
        }
        skip_sp(b, i);
        if b.get(*i) == Some(&b'.') {
            *i += 1;
            skip_sp(b, i);
            key.push('.');
        } else {
            return Ok(key);
        }
    }
}

fn parse_val(b: &[u8], i: &mut usize) -> Res<Val> {
    match b.get(*i) {
        Some(b'"') => Ok(Val::Str(basic(b, i)?)),
        Some(b'\'') => Ok(Val::Str(literal(b, i)?)),
        Some(b'[') => {
            *i += 1;
            let mut v = Vec::new();
            loop {
                skip_blank(b, i);
                if b.get(*i) == Some(&b']') {
                    *i += 1;
                    return Ok(Val::Arr(v));
                }
                v.push(parse_val(b, i)?);
                skip_blank(b, i);
                match b.get(*i) {
                    Some(b',') => *i += 1,
                    Some(b']') => {}
                    _ => bail!("expected , or ] in array"),
                }
            }
        }
        _ => {
            let s = *i;
            while *i < b.len() && (b[*i].is_ascii_alphanumeric() || b[*i] == b'-' || b[*i] == b'_')
            {
                *i += 1;
            }
            let w = std::str::from_utf8(&b[s..*i]).unwrap();
            match w {
                "true" => Ok(Val::Bool(true)),
                "false" => Ok(Val::Bool(false)),
                _ => w
                    .replace('_', "")
                    .parse()
                    .map(Val::Int)
                    .map_err(|_| format!("bad value {w:?}")),
            }
        }
    }
}

fn basic(b: &[u8], i: &mut usize) -> Res<String> {
    let multi = b[*i..].starts_with(b"\"\"\"");
    *i += if multi { 3 } else { 1 };
    if multi {
        if b[*i..].starts_with(b"\r\n") {
            *i += 2;
        } else if b.get(*i) == Some(&b'\n') {
            *i += 1;
        }
    }
    let mut out: Vec<u8> = Vec::new();
    loop {
        let Some(&c) = b.get(*i) else {
            bail!("unterminated string")
        };
        if multi && b[*i..].starts_with(b"\"\"\"") {
            *i += 3;
            break;
        }
        if !multi && c == b'"' {
            *i += 1;
            break;
        }
        if !multi && c == b'\n' {
            bail!("newline in string");
        }
        if c == b'\\' {
            *i += 1;
            let Some(&e) = b.get(*i) else {
                bail!("unterminated escape")
            };
            *i += 1;
            match e {
                b'n' => out.push(b'\n'),
                b't' => out.push(b'\t'),
                b'r' => out.push(b'\r'),
                b'"' => out.push(b'"'),
                b'\\' => out.push(b'\\'),
                b'u' | b'U' => {
                    let n = if e == b'u' { 4 } else { 8 };
                    let hex =
                        std::str::from_utf8(b.get(*i..*i + n).unwrap_or_default()).unwrap_or("");
                    let ch = u32::from_str_radix(hex, 16).ok().and_then(char::from_u32);
                    let Some(ch) = ch else {
                        bail!("bad unicode escape")
                    };
                    *i += n;
                    out.extend_from_slice(ch.to_string().as_bytes());
                }
                b'\n' if multi => {
                    while *i < b.len() && b[*i].is_ascii_whitespace() {
                        *i += 1;
                    }
                }
                _ => bail!("bad escape \\{}", e as char),
            }
            continue;
        }
        out.push(c);
        *i += 1;
    }
    String::from_utf8(out).map_err(|_| "invalid utf-8 in string".into())
}

fn literal(b: &[u8], i: &mut usize) -> Res<String> {
    let multi = b[*i..].starts_with(b"'''");
    *i += if multi { 3 } else { 1 };
    if multi && b.get(*i) == Some(&b'\n') {
        *i += 1;
    }
    let s = *i;
    loop {
        if *i >= b.len() {
            bail!("unterminated string");
        }
        if multi && b[*i..].starts_with(b"'''") {
            let v = std::str::from_utf8(&b[s..*i]).unwrap().to_string();
            *i += 3;
            return Ok(v);
        }
        if !multi && b[*i] == b'\'' {
            let v = std::str::from_utf8(&b[s..*i]).unwrap().to_string();
            *i += 1;
            return Ok(v);
        }
        *i += 1;
    }
}

// --- typed config ---------------------------------------------------------------

/// What a lane's work needs, whatever the project calls the lane. Behaviour
/// follows the kind; the name is the project's own vocabulary (`>game` can be a
/// manual lane).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LaneKind {
    /// Anything can do it from the repository alone.
    Agent,
    /// Needs access an agent may not have: a machine, an account, a secret.
    Restricted,
    /// A person doing it by hand, outside the repository.
    Manual,
    /// A call only the owner makes.
    Decision,
}

impl LaneKind {
    pub fn parse(s: &str) -> Option<LaneKind> {
        Some(match s {
            "agent" => LaneKind::Agent,
            "restricted" => LaneKind::Restricted,
            "manual" => LaneKind::Manual,
            "decision" => LaneKind::Decision,
            _ => return None,
        })
    }
    pub fn name(self) -> &'static str {
        match self {
            LaneKind::Agent => "agent",
            LaneKind::Restricted => "restricted",
            LaneKind::Manual => "manual",
            LaneKind::Decision => "decision",
        }
    }
    pub fn delegable(self) -> bool {
        matches!(self, LaneKind::Agent | LaneKind::Restricted)
    }
    pub fn close(self) -> &'static str {
        if self == LaneKind::Decision {
            "decided"
        } else {
            "self"
        }
    }
    fn note(self) -> Option<&'static str> {
        match self {
            LaneKind::Restricted => {
                Some("needs access an agent may not have: a machine, an account, a secret")
            }
            _ => None,
        }
    }
    fn refuse(self) -> Option<&'static str> {
        match self {
            LaneKind::Manual => Some("manual — a person does this by hand, outside the repository"),
            LaneKind::Decision => Some("a decision only the owner can make, not work to hand off"),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Lane {
    pub name: String,
    pub kind: LaneKind,
    /// Shown by `ready` and accepted by `delegate`.
    pub delegable: bool,
    /// The flag `done` requires and the `via:` it records: `self` means `--self`.
    pub close: String,
    /// Tasks on this lane live under their own heading instead of the open one.
    pub section: Option<String>,
    /// One line printed in a brief for this lane.
    pub note: Option<String>,
    /// Why `delegate` refuses a non-delegable lane.
    pub refuse: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Config {
    /// The oldest 5w this project works with.
    pub requires: Option<String>,
    pub file: String,
    /// Where `archive` moves closed tasks.
    pub archive: String,
    /// Longest task line text; longer is split into title and body. 0: no limit.
    pub title_max: usize,
    pub trunk: Option<String>,
    pub perennial: Vec<String>,
    pub commit_prefix: String,
    pub open_section: String,
    pub done_section: String,
    pub default_lane: String,
    pub levels: BTreeMap<u8, String>,
    pub lanes: Vec<Lane>,
    pub require_task: bool,
    pub wt_root: Option<String>,
    pub links_file: String,
    pub install: Option<String>,
    pub install_marker: Option<String>,
    /// Gitignored paths ship may delete with a worktree without asking.
    pub disposable: Vec<String>,
    pub context_docs: Vec<String>,
    pub area_docs: Vec<String>,
    pub conventions: Vec<String>,
    pub brief_footer: Option<String>,
    pub checklist: Vec<String>,
    pub cmd_tasks: String,
    pub cmd_wt: String,
    pub cmd_ship: String,
}

impl Lane {
    pub fn of_kind(name: &str, kind: LaneKind) -> Lane {
        Lane {
            name: name.into(),
            kind,
            delegable: kind.delegable(),
            close: kind.close().into(),
            section: None,
            note: kind.note().map(String::from),
            refuse: kind.refuse().map(String::from),
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        let lane = |name: &str, kind| Lane::of_kind(name, kind);
        Config {
            requires: None,
            file: "TASKS.md".into(),
            archive: "DONE.md".into(),
            title_max: 120,
            trunk: None,
            perennial: vec![],
            commit_prefix: "chore(tasks)".into(),
            open_section: "## Open".into(),
            done_section: "## Done".into(),
            default_lane: "agent".into(),
            levels: BTreeMap::from([
                (1, "mechanical — a small fast model".into()),
                (2, "standard implementation — a mid model".into()),
                (3, "design judgement — the strongest model".into()),
                (
                    4,
                    "open research — the strongest model, and review the reasoning".into(),
                ),
            ]),
            lanes: vec![
                lane("agent", LaneKind::Agent),
                lane("restricted", LaneKind::Restricted),
                lane("manual", LaneKind::Manual),
                lane("owner", LaneKind::Decision),
            ],
            require_task: false,
            wt_root: None,
            links_file: ".worktree-links".into(),
            install: None,
            install_marker: None,
            disposable: vec!["node_modules".into(), "target".into(), ".next".into()],
            context_docs: vec![],
            area_docs: vec!["README.md".into(), "FINDINGS.md".into(), "DESIGN.md".into()],
            conventions: vec![],
            brief_footer: None,
            checklist: vec![],
            cmd_tasks: "5w".into(),
            cmd_wt: "5w wt".into(),
            cmd_ship: "5w ship".into(),
        }
    }
}

impl Config {
    pub fn from_toml(src: &str) -> Res<Config> {
        let kv = parse_toml(src)?;
        // The pin first: a project written for a newer 5w may use keys this one
        // does not know, and "unknown key" would hide the real problem.
        if let Some((_, v)) = kv.iter().find(|(k, _)| k == "requires") {
            match v {
                Val::Str(r) => crate::upkeep::check_requires(r)?,
                _ => bail!("config: requires must be a version string"),
            }
        }
        let mut c = Config::default();
        let s = |v: &Val, k: &str| -> Res<String> {
            match v {
                Val::Str(s) => Ok(s.clone()),
                _ => Err(format!("config: {k} must be a string")),
            }
        };
        let arr = |v: &Val, k: &str| -> Res<Vec<String>> {
            match v {
                Val::Arr(a) => a.iter().map(|x| s(x, k)).collect(),
                _ => Err(format!("config: {k} must be an array of strings")),
            }
        };
        let boolean = |v: &Val, k: &str| -> Res<bool> {
            match v {
                Val::Bool(b) => Ok(*b),
                _ => Err(format!("config: {k} must be true or false")),
            }
        };
        // Lane fields are collected raw and resolved after the loop, because the
        // kind decides the defaults and may be written after the fields.
        #[derive(Default)]
        struct RawLane {
            kind: Option<String>,
            delegable: Option<bool>,
            close: Option<String>,
            section: Option<String>,
            note: Option<String>,
            refuse: Option<String>,
        }
        let mut raw: Vec<(String, RawLane)> = Vec::new();
        for (k, v) in &kv {
            match k.as_str() {
                "requires" => c.requires = Some(s(v, k)?),
                "file" => c.file = s(v, k)?,
                "archive" => c.archive = s(v, k)?,
                "title_max" => match v {
                    Val::Int(n) if *n >= 0 => c.title_max = *n as usize,
                    _ => bail!("config: title_max must be a non-negative integer"),
                },
                "trunk" => c.trunk = Some(s(v, k)?),
                "perennial" => c.perennial = arr(v, k)?,
                "commit_prefix" => c.commit_prefix = s(v, k)?,
                "default_lane" => c.default_lane = s(v, k)?,
                "require_task" => c.require_task = boolean(v, k)?,
                "sections.open" => c.open_section = s(v, k)?,
                "sections.done" => c.done_section = s(v, k)?,
                "worktrees.root" => c.wt_root = Some(s(v, k)?),
                "worktrees.links_file" => c.links_file = s(v, k)?,
                "worktrees.install" => c.install = Some(s(v, k)?),
                "worktrees.install_marker" => c.install_marker = Some(s(v, k)?),
                "worktrees.disposable" => c.disposable = arr(v, k)?,
                "delegate.context" => c.context_docs = arr(v, k)?,
                "delegate.area_docs" => c.area_docs = arr(v, k)?,
                "delegate.conventions" => c.conventions = arr(v, k)?,
                "delegate.footer" => c.brief_footer = Some(s(v, k)?),
                "review.checklist" => c.checklist = arr(v, k)?,
                "commands.tasks" => c.cmd_tasks = s(v, k)?,
                "commands.wt" => c.cmd_wt = s(v, k)?,
                "commands.ship" => c.cmd_ship = s(v, k)?,
                _ if k.starts_with("levels.") => {
                    let n: u8 = k[7..]
                        .parse()
                        .map_err(|_| format!("config: bad level key {k}"))?;
                    if !(1..=4).contains(&n) {
                        bail!("config: levels run 1 to 4, not {n}");
                    }
                    c.levels.insert(n, s(v, k)?);
                }
                _ if k.starts_with("lanes.") => {
                    let rest = &k[6..];
                    let Some((name, field)) = rest.split_once('.') else {
                        bail!("config: bad lane key {k}")
                    };
                    if !name.bytes().all(|b| b.is_ascii_lowercase()) {
                        bail!("config: lane names are lowercase letters: {name}");
                    }
                    let idx = match raw.iter().position(|(n, _)| n == name) {
                        Some(i) => i,
                        None => {
                            raw.push((name.to_string(), RawLane::default()));
                            raw.len() - 1
                        }
                    };
                    let l = &mut raw[idx].1;
                    match field {
                        "kind" => l.kind = Some(s(v, k)?),
                        "delegable" => l.delegable = Some(boolean(v, k)?),
                        "close" => l.close = Some(s(v, k)?),
                        "section" => l.section = Some(s(v, k)?),
                        "note" => l.note = Some(s(v, k)?),
                        "refuse" => l.refuse = Some(s(v, k)?),
                        _ => bail!("config: unknown lane field {k}"),
                    }
                }
                _ => bail!(
                    "config: unknown key {k} (this is 5w {}; a newer one may know it — set `requires` to say which)",
                    crate::upkeep::VERSION
                ),
            }
        }
        // A config that names lanes replaces the defaults wholesale.
        if !raw.is_empty() {
            c.lanes = raw
                .into_iter()
                .map(|(name, r)| {
                    // Without `kind`, infer it from the older flags, so existing
                    // configs keep their meaning.
                    let kind = match &r.kind {
                        Some(k) => LaneKind::parse(k).ok_or_else(|| {
                            format!("config: lanes.{name}.kind must be agent, restricted, manual or decision, not {k:?}")
                        })?,
                        None if r.close.as_deref() == Some("decided") => LaneKind::Decision,
                        None if r.delegable == Some(false) => LaneKind::Manual,
                        None => LaneKind::Agent,
                    };
                    let mut l = Lane::of_kind(&name, kind);
                    if let Some(d) = r.delegable {
                        l.delegable = d;
                    }
                    if let Some(cl) = r.close {
                        l.close = cl;
                    }
                    l.section = r.section;
                    if r.note.is_some() {
                        l.note = r.note;
                    }
                    if r.refuse.is_some() {
                        l.refuse = r.refuse;
                    }
                    Ok(l)
                })
                .collect::<Res<Vec<Lane>>>()?;
        }
        if !c.lanes.iter().any(|l| l.name == c.default_lane) {
            bail!(
                "config: default_lane {} is not a configured lane",
                c.default_lane
            );
        }
        Ok(c)
    }

    pub fn lane(&self, name: &str) -> Option<&Lane> {
        self.lanes.iter().find(|l| l.name == name)
    }

    pub fn tier(&self, level: Option<u8>) -> String {
        level
            .and_then(|l| self.levels.get(&l).cloned())
            .unwrap_or_else(|| "unrated — size it before delegating".into())
    }

    /// Where an open task on this lane lives.
    pub fn section_for(&self, lane: &str) -> String {
        self.lane(lane)
            .and_then(|l| l.section.clone())
            .unwrap_or_else(|| self.open_section.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_subset() {
        let src = concat!(
            "# comment\n",
            "trunk = \"main\"   # trailing\n",
            "perennial = [\"cloudflare\", 'x']\n",
            "[lanes.agent]\ndelegable = true\n",
            "[lanes.capture]\nkind = \"manual\"\nsection = \"## Capture\"\n",
            "[lanes.legacy]\ndelegable = false\n",
            "[levels]\n1 = \"tiny\"\n",
            "[delegate]\nfooter = \"\"\"\nline one\nline \"two\"\n\"\"\"\n",
        );
        let c = Config::from_toml(src).unwrap();
        assert_eq!(c.trunk.as_deref(), Some("main"));
        assert_eq!(c.perennial, vec!["cloudflare", "x"]);
        assert_eq!(c.lanes.len(), 3);
        let capture = c.lane("capture").unwrap();
        assert!(capture.kind == LaneKind::Manual && !capture.delegable && capture.close == "self");
        // An old-style lane with no kind keeps its meaning.
        assert_eq!(c.lane("legacy").unwrap().kind, LaneKind::Manual);
        assert_eq!(c.levels[&1], "tiny");
        assert_eq!(c.brief_footer.as_deref(), Some("line one\nline \"two\"\n"));
    }

    #[test]
    fn rejects_unknown_keys() {
        assert!(Config::from_toml("nope = 1").is_err());
    }
}

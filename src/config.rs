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
    #[allow(dead_code)]
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

#[derive(Clone, Debug)]
pub struct Lane {
    pub name: String,
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
    pub file: String,
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
    pub context_docs: Vec<String>,
    pub area_docs: Vec<String>,
    pub conventions: Vec<String>,
    pub brief_footer: Option<String>,
    pub checklist: Vec<String>,
    pub cmd_tasks: String,
    pub cmd_wt: String,
    pub cmd_ship: String,
}

impl Default for Config {
    fn default() -> Self {
        let lane = |name: &str, delegable, close: &str| Lane {
            name: name.into(),
            delegable,
            close: close.into(),
            section: None,
            note: None,
            refuse: None,
        };
        let mut owner = lane("owner", false, "decided");
        owner.refuse = Some("a decision only the owner can make, not work to hand off".into());
        let mut local = lane("local", true, "self");
        local.note = Some("needs this machine, an account or a secret".into());
        Config {
            file: "TASKS.md".into(),
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
            lanes: vec![lane("agent", true, "self"), local, owner],
            require_task: false,
            wt_root: None,
            links_file: ".worktree-links".into(),
            install: None,
            install_marker: None,
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
        let mut lanes_seen = false;
        for (k, v) in &kv {
            match k.as_str() {
                "file" => c.file = s(v, k)?,
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
                    if !lanes_seen {
                        // A config that names lanes replaces the defaults wholesale.
                        c.lanes.clear();
                        lanes_seen = true;
                    }
                    if !name.bytes().all(|b| b.is_ascii_lowercase()) {
                        bail!("config: lane names are lowercase letters: {name}");
                    }
                    let idx = match c.lanes.iter().position(|l| l.name == name) {
                        Some(i) => i,
                        None => {
                            c.lanes.push(Lane {
                                name: name.into(),
                                delegable: true,
                                close: "self".into(),
                                section: None,
                                note: None,
                                refuse: None,
                            });
                            c.lanes.len() - 1
                        }
                    };
                    let l = &mut c.lanes[idx];
                    match field {
                        "delegable" => l.delegable = boolean(v, k)?,
                        "close" => l.close = s(v, k)?,
                        "section" => l.section = Some(s(v, k)?),
                        "note" => l.note = Some(s(v, k)?),
                        "refuse" => l.refuse = Some(s(v, k)?),
                        _ => bail!("config: unknown lane field {k}"),
                    }
                }
                _ => bail!("config: unknown key {k}"),
            }
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
            "[lanes.game]\ndelegable = false\nsection = \"## In-game capture\"\n",
            "[levels]\n1 = \"tiny\"\n",
            "[delegate]\nfooter = \"\"\"\nline one\nline \"two\"\n\"\"\"\n",
        );
        let c = Config::from_toml(src).unwrap();
        assert_eq!(c.trunk.as_deref(), Some("main"));
        assert_eq!(c.perennial, vec!["cloudflare", "x"]);
        assert_eq!(c.lanes.len(), 2);
        assert!(!c.lane("game").unwrap().delegable);
        assert_eq!(c.levels[&1], "tiny");
        assert_eq!(c.brief_footer.as_deref(), Some("line one\nline \"two\"\n"));
    }

    #[test]
    fn rejects_unknown_keys() {
        assert!(Config::from_toml("nope = 1").is_err());
    }
}

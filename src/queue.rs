//! The task file: parsing, and edits addressed by id.
//!
//! One task per line, optional body indented under it:
//!
//! ```text
//! - [ ] #14 what to do  @area !3 >agent needs:#12,#13 branch:area/what
//!   Longer brief. Every following line indented by two spaces or a tab
//!   belongs to #14, and moves with it.
//! ```
//!
//! Lines inside ``` fences are documentation and never tasks — a format example
//! is a task line in every respect but intent, and editing it by accident is how
//! the original shell version once deleted a real task.

use crate::bail;
use crate::util::Res;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum State {
    #[default]
    Open,
    Review,
    Done,
}

impl State {
    pub fn mark(self) -> char {
        match self {
            State::Open => ' ',
            State::Review => '~',
            State::Done => 'x',
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            State::Open => "open",
            State::Review => "submitted, waiting on review",
            State::Done => "done",
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Task {
    pub id: u64,
    pub state: State,
    pub area: Option<String>,
    pub level: Option<u8>,
    pub lane: Option<String>,
    pub needs: Vec<u64>,
    pub branch: Option<String>,
    pub rework: Option<String>,
    pub via: Option<String>,
    pub submitted: Option<String>,
    pub reviewed: Option<String>,
    pub text: String,
    /// Index of the task line.
    pub line: usize,
    /// Body lines, indentation stripped.
    pub body: Vec<String>,
    /// The heading this task sits under.
    pub section: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Text,
    Area,
    Level,
    Lane,
    Needs,
    Branch,
    Rework,
    Via,
    Submitted,
    Reviewed,
}

#[derive(Debug)]
pub struct Tok {
    pub kind: Kind,
    pub start: usize,
    pub end: usize,
}

pub fn head(line: &str) -> Option<(State, u64, &str)> {
    let r = line.strip_prefix("- [")?;
    let m = r.chars().next()?;
    let state = match m {
        ' ' => State::Open,
        '~' => State::Review,
        'x' => State::Done,
        _ => return None,
    };
    let r = r[m.len_utf8()..].strip_prefix("] #")?;
    let digits = r.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 {
        return None;
    }
    let id = r[..digits].parse().ok()?;
    let rest = &r[digits..];
    if rest.is_empty() {
        Some((state, id, ""))
    } else {
        rest.strip_prefix(' ').map(|x| (state, id, x))
    }
}

fn all(s: &str, f: impl Fn(u8) -> bool) -> bool {
    !s.is_empty() && s.bytes().all(f)
}

pub fn classify(w: &str) -> Kind {
    if let Some(v) = w.strip_prefix('@')
        && all(v, |b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Kind::Area;
    }
    if w.len() == 2 && w.starts_with('!') && (b'1'..=b'4').contains(&w.as_bytes()[1]) {
        return Kind::Level;
    }
    if let Some(v) = w.strip_prefix('>')
        && all(v, |b| b.is_ascii_lowercase())
    {
        return Kind::Lane;
    }
    if let Some(v) = w.strip_prefix("needs:")
        && all(v, |b| b.is_ascii_digit() || b == b',' || b == b'#')
        && v.bytes().any(|b| b.is_ascii_digit())
    {
        return Kind::Needs;
    }
    if let Some(v) = w.strip_prefix("branch:")
        && !v.is_empty()
    {
        return Kind::Branch;
    }
    if let Some(v) = w.strip_prefix("via:")
        && all(v, |b| b.is_ascii_lowercase())
    {
        return Kind::Via;
    }
    if let Some(v) = w.strip_prefix("submitted:")
        && all(v, |b| b.is_ascii_hexdigit())
    {
        return Kind::Submitted;
    }
    if let Some(v) = w.strip_prefix("reviewed:")
        && all(v, |b| b.is_ascii_hexdigit())
    {
        return Kind::Reviewed;
    }
    Kind::Text
}

pub fn tokenize(rest: &str) -> Vec<Tok> {
    let b = rest.as_bytes();
    let mut i = 0;
    let mut out = Vec::new();
    while i < b.len() {
        if b[i] == b' ' || b[i] == b'\t' || b[i] == b'\r' {
            i += 1;
            continue;
        }
        let start = i;
        if rest[i..].starts_with("rework:\"") {
            let mut j = i + 8;
            let mut end = None;
            while j < b.len() {
                match b[j] {
                    b'\\' => j += 2,
                    b'"' => {
                        end = Some(j + 1);
                        break;
                    }
                    _ => j += 1,
                }
            }
            if let Some(e) = end
                && (e == b.len() || b[e] == b' ' || b[e] == b'\t' || b[e] == b'\r')
            {
                out.push(Tok {
                    kind: Kind::Rework,
                    start,
                    end: e,
                });
                i = e;
                continue;
            }
        }
        while i < b.len() && b[i] != b' ' && b[i] != b'\t' && b[i] != b'\r' {
            i += 1;
        }
        out.push(Tok {
            kind: classify(&rest[start..i]),
            start,
            end: i,
        });
    }
    out
}

fn unescape(s: &str) -> String {
    let mut out = String::new();
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c == '\\' {
            if let Some(n) = it.next() {
                out.push(n);
            }
        } else {
            out.push(c);
        }
    }
    out
}

pub fn escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace(['\n', '\r'], " ")
}

fn is_fence(line: &str) -> bool {
    line.starts_with("```")
}

pub fn is_body(line: &str) -> bool {
    (line.starts_with("  ") || line.starts_with('\t')) && !line.trim().is_empty()
}

pub fn parse(text: &str) -> Vec<Task> {
    // CRLF files parse like LF ones: a trailing \r must not become part of the
    // last field, or `branch:x\r` stops matching the branch ship looks for.
    let lines: Vec<&str> = text
        .split('\n')
        .map(|l| l.strip_suffix('\r').unwrap_or(l))
        .collect();
    let mut tasks = Vec::new();
    let mut fence = false;
    let mut section: Option<String> = None;
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if is_fence(line) {
            fence = !fence;
            i += 1;
            continue;
        }
        if fence {
            i += 1;
            continue;
        }
        if line.starts_with("## ") {
            section = Some(line.trim_end().to_string());
        }
        let Some((state, id, rest)) = head(line) else {
            i += 1;
            continue;
        };
        let mut t = Task {
            id,
            state,
            line: i,
            section: section.clone(),
            ..Default::default()
        };
        let mut words = Vec::new();
        let toks = tokenize(rest);
        for (n, tok) in toks.iter().enumerate() {
            let w = &rest[tok.start..tok.end];
            // The last token of a kind is the field; an earlier lookalike is prose.
            let is_field =
                tok.kind != Kind::Text && !toks[n + 1..].iter().any(|t| t.kind == tok.kind);
            if !is_field {
                words.push(w);
                continue;
            }
            match tok.kind {
                Kind::Text => {}
                Kind::Area => t.area = Some(w[1..].into()),
                Kind::Level => t.level = Some(w.as_bytes()[1] - b'0'),
                Kind::Lane => t.lane = Some(w[1..].into()),
                Kind::Needs => {
                    t.needs = w[6..]
                        .split([',', '#'])
                        .filter_map(|n| n.parse().ok())
                        .collect();
                }
                Kind::Branch => t.branch = Some(w[7..].into()),
                Kind::Rework => t.rework = Some(unescape(&w[8..w.len() - 1])),
                Kind::Via => t.via = Some(w[4..].into()),
                Kind::Submitted => t.submitted = Some(w[10..].into()),
                Kind::Reviewed => t.reviewed = Some(w[9..].into()),
            }
        }
        t.text = words.join(" ");
        let mut j = i + 1;
        while j < lines.len() && is_body(lines[j]) {
            let l = lines[j];
            t.body.push(
                l.strip_prefix("  ")
                    .or_else(|| l.strip_prefix('\t'))
                    .unwrap_or(l)
                    .to_string(),
            );
            j += 1;
        }
        tasks.push(t);
        i = j;
    }
    tasks
}

pub fn duplicates(tasks: &[Task]) -> Vec<(u64, usize, usize)> {
    let mut first = std::collections::HashMap::new();
    let mut out = Vec::new();
    for t in tasks {
        if let Some(&l) = first.get(&t.id) {
            out.push((t.id, l + 1, t.line + 1));
        } else {
            first.insert(t.id, t.line);
        }
    }
    out
}

/// The highest id on any task-shaped line, fences included. Minting must never
/// reuse an id, and an unclosed fence would otherwise hide every task after it.
pub fn max_id(text: &str) -> u64 {
    text.split('\n')
        .filter_map(|l| head(l.trim_end_matches('\r')).map(|(_, id, _)| id))
        .max()
        .unwrap_or(0)
}

pub fn unclosed_fence(text: &str) -> bool {
    text.split('\n').filter(|l| is_fence(l)).count() % 2 == 1
}

// --- editing ----------------------------------------------------------------------

pub fn set_mark(line: &str, s: State) -> String {
    // "- [" is three ASCII bytes and the mark is one of three ASCII chars.
    format!("- [{}{}", s.mark(), &line[4..])
}

fn repr(kind: Kind, v: &str) -> String {
    match kind {
        Kind::Area => format!("@{v}"),
        Kind::Level => format!("!{v}"),
        Kind::Lane => format!(">{v}"),
        Kind::Needs => format!("needs:{v}"),
        Kind::Branch => format!("branch:{v}"),
        Kind::Rework => format!("rework:\"{}\"", escape(v)),
        Kind::Via => format!("via:{v}"),
        Kind::Submitted => format!("submitted:{v}"),
        Kind::Reviewed => format!("reviewed:{v}"),
        Kind::Text => v.to_string(),
    }
}

/// Replace the field of `kind` — the last token of that kind, which is the one
/// `parse` reads — and append `value` if given. Earlier lookalikes are prose
/// ("still >manual, so…") and stay; the shell version stripped them out of the text.
/// The rest of the line keeps its original spacing.
pub fn set_field(line: &str, kind: Kind, value: Option<&str>) -> String {
    let Some((_, _, rest)) = head(line) else {
        return line.to_string();
    };
    let prefix = &line[..line.len() - rest.len()];
    let toks = tokenize(rest);
    let drop = toks.iter().rposition(|t| t.kind == kind);
    let mut out = String::new();
    let mut prev_end = 0;
    let mut kept_any = false;
    for (i, t) in toks.iter().enumerate() {
        if Some(i) != drop {
            if kept_any {
                out.push_str(&rest[prev_end..t.start]);
            }
            out.push_str(&rest[t.start..t.end]);
            kept_any = true;
        }
        prev_end = t.end;
    }
    if let Some(v) = value {
        if kept_any {
            out.push(' ');
        }
        out.push_str(&repr(kind, v));
    }
    format!("{prefix}{out}")
}

/// Split over-long task text into a title of at most `max` characters and the
/// rest. Prefers the end of the first sentence, then the last clause break, then
/// the last word boundary. `title + rest` loses nothing but the break itself.
pub fn split_title(text: &str, max: usize) -> Option<(String, String)> {
    if max == 0 || text.chars().count() <= max {
        return None;
    }
    // Byte offset of the max-th char.
    let limit = text
        .char_indices()
        .nth(max)
        .map(|(i, _)| i)
        .unwrap_or(text.len());
    let head = &text[..limit];
    let min = head.char_indices().nth(20).map(|(i, _)| i).unwrap_or(0);
    let find_last = |pats: &[&str]| {
        pats.iter()
            .filter_map(|p| head.rfind(p).map(|i| (i, p.len())))
            .filter(|(i, _)| *i >= min)
            .max_by_key(|(i, _)| *i)
    };
    let first_sentence = [". ", "? ", "! "]
        .iter()
        .filter_map(|p| head.find(p).map(|i| (i + 1, p.len() - 1)))
        .filter(|(i, _)| *i >= min)
        .min_by_key(|(i, _)| *i);
    let (cut, skip, ellipsis) = if let Some((i, n)) = first_sentence {
        (i, n, false)
    } else if let Some((i, n)) = find_last(&[" — ", " -- ", "; ", ": ", ", "]) {
        (i, n, false)
    } else if let Some(i) = head.rfind(' ').filter(|i| *i >= min) {
        (i, 1, false)
    } else {
        (limit, 0, false)
    };
    let mut title = text[..cut].trim_end().to_string();
    if ellipsis {
        title.push('…');
    }
    let rest = text[cut + skip..].trim_start().to_string();
    Some((title, rest))
}

/// Replace the text of a task line, keeping every field token as it was.
pub fn set_text(line: &str, text: &str) -> String {
    let Some((_, _, rest)) = head(line) else {
        return line.to_string();
    };
    let prefix = &line[..line.len() - rest.len()];
    let toks = tokenize(rest);
    let fields: Vec<&str> = toks
        .iter()
        .enumerate()
        .filter(|(n, t)| t.kind != Kind::Text && !toks[n + 1..].iter().any(|u| u.kind == t.kind))
        .map(|(_, t)| &rest[t.start..t.end])
        .collect();
    let mut out = format!("{prefix}{text}");
    for f in fields {
        out.push(' ');
        out.push_str(f);
    }
    out
}

/// Word-wrap into body lines, two-space indented.
pub fn body_lines(text: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    for para in text.lines().filter(|l| !l.trim().is_empty()) {
        let mut line = String::new();
        for w in para.split_whitespace() {
            if !line.is_empty() && line.chars().count() + 1 + w.chars().count() > width {
                out.push(format!("  {line}"));
                line.clear();
            }
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(w);
        }
        if !line.is_empty() {
            out.push(format!("  {line}"));
        }
    }
    out
}

pub struct Doc {
    pub lines: Vec<String>,
    crlf: bool,
}

impl Doc {
    /// Lines are held without their \r; a CRLF file is written back as CRLF.
    pub fn new(text: &str) -> Doc {
        Doc {
            lines: text
                .split('\n')
                .map(|l| l.strip_suffix('\r').unwrap_or(l).to_string())
                .collect(),
            crlf: text.contains("\r\n"),
        }
    }

    pub fn text(&self) -> String {
        self.lines.join(if self.crlf { "\r\n" } else { "\n" })
    }

    /// (line, length including body) of task `id`, fences skipped.
    pub fn block(&self, id: u64) -> Option<(usize, usize)> {
        let text = self.text();
        let t = parse(&text).into_iter().find(|t| t.id == id)?;
        Some((t.line, 1 + t.body.len()))
    }

    fn section_range(&self, header: &str) -> Option<(usize, usize)> {
        let mut fence = false;
        let mut start = None;
        for (i, l) in self.lines.iter().enumerate() {
            if is_fence(l) {
                fence = !fence;
                continue;
            }
            if fence {
                continue;
            }
            if l.starts_with("## ") || l.starts_with("# ") {
                if let Some(s) = start {
                    return Some((s, i));
                }
                if l.trim_end() == header {
                    start = Some(i);
                }
            }
        }
        start.map(|s| (s, self.lines.len()))
    }

    /// Append a block at the end of a section, creating the section if the file
    /// lacks it — before `before` when that heading exists, else at the end.
    pub fn insert(&mut self, block: Vec<String>, header: &str, before: Option<&str>) {
        let Some((h, e)) = self.section_range(header) else {
            let at = before.and_then(|b| self.section_range(b)).map(|(s, _)| s);
            let mut new = vec![header.to_string(), String::new()];
            new.extend(block);
            new.push(String::new());
            match at {
                Some(at) => {
                    self.lines.splice(at..at, new);
                }
                None => {
                    // Keep a single trailing newline at the end of the file.
                    while self.lines.last().is_some_and(|l| l.is_empty()) {
                        self.lines.pop();
                    }
                    self.lines.push(String::new());
                    self.lines.extend(new);
                }
            }
            return;
        };
        match (h + 1..e).rev().find(|&i| !self.lines[i].trim().is_empty()) {
            Some(last) => {
                self.lines.splice(last + 1..last + 1, block);
            }
            None => {
                let mut new = vec![String::new()];
                new.extend(block);
                new.push(String::new());
                self.lines.splice(h + 1..e, new);
            }
        }
    }

    /// Shorten task `id`'s line to a title, moving the rest to the top of its
    /// body. Returns whether anything changed.
    pub fn split_title(&mut self, id: u64, max: usize) -> Res<bool> {
        let Some((at, _)) = self.block(id) else {
            bail!("no task #{id}")
        };
        let text = self.text();
        let t = parse(&text)
            .into_iter()
            .find(|t| t.id == id)
            .expect("block found");
        let Some((title, rest)) = split_title(&t.text, max) else {
            return Ok(false);
        };
        self.lines[at] = set_text(&self.lines[at], &title);
        let body = body_lines(&rest, 100);
        self.lines.splice(at + 1..at + 1, body);
        Ok(true)
    }

    /// Remove and return the blocks of every task matching `pick`.
    pub fn take(&mut self, pick: impl Fn(&Task) -> bool) -> Vec<Vec<String>> {
        let text = self.text();
        let mut spans: Vec<(usize, usize)> = parse(&text)
            .iter()
            .filter(|t| pick(t))
            .map(|t| (t.line, 1 + t.body.len()))
            .collect();
        spans.sort();
        let blocks: Vec<Vec<String>> = spans
            .iter()
            .map(|(a, n)| self.lines[*a..*a + *n].to_vec())
            .collect();
        for (a, n) in spans.iter().rev() {
            self.lines.drain(*a..*a + *n);
        }
        blocks
    }

    /// Rewrite the task line of `id`, and move its block to `to` if it is not
    /// already under that heading.
    pub fn update(
        &mut self,
        id: u64,
        f: impl Fn(&str) -> String,
        to: Option<&str>,
        before: Option<&str>,
    ) -> Res<()> {
        let Some((at, len)) = self.block(id) else {
            bail!("no task #{id}")
        };
        self.lines[at] = f(&self.lines[at]);
        let Some(to) = to else { return Ok(()) };
        let text = self.text();
        let here = parse(&text)
            .into_iter()
            .find(|t| t.id == id)
            .and_then(|t| t.section);
        if here.as_deref() == Some(to) {
            return Ok(());
        }
        let block: Vec<String> = self.lines.drain(at..at + len).collect();
        self.insert(block, to, before);
        Ok(())
    }
}

/// The same row, byte for byte in every part that is read (its state aside).
pub fn identical(o: &Task, n: &Task) -> bool {
    o.text == n.text
        && o.body == n.body
        && (&o.area, o.level, &o.lane, &o.needs, &o.branch)
            == (&n.area, n.level, &n.lane, &n.needs, &n.branch)
        && (&o.rework, &o.via, &o.submitted, &o.reviewed)
            == (&n.rework, &n.via, &n.submitted, &n.reviewed)
}

/// Every row of a queue and its archive, the queue's first.
pub fn parse_all(texts: [&str; 2]) -> Vec<Task> {
    texts.into_iter().flat_map(parse).collect()
}

/// Rows by id; a later one wins.
pub fn by_id(v: &[Task]) -> std::collections::HashMap<u64, &Task> {
    v.iter().map(|t| (t.id, t)).collect()
}

/// The rows that differ between two readings of a queue and its archive —
/// added, removed, or with a different state or any part that is read. A row
/// moved, or a line rewritten to say the same, is not changed.
pub fn changed_ids(
    old: &std::collections::HashMap<u64, &Task>,
    new: &std::collections::HashMap<u64, &Task>,
) -> std::collections::BTreeSet<u64> {
    new.iter()
        .filter(|(id, n)| {
            old.get(id)
                .is_none_or(|o| o.state != n.state || !identical(o, n))
        })
        .map(|(id, _)| *id)
        .chain(old.keys().filter(|id| !new.contains_key(id)).copied())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = "# Tasks\n\n```\n- [ ] #1 example @x\n```\n\n## Open\n\n- [ ] #1 real one @faces !3 >agent needs:#2\n  body line\n- [ ] #2 second rework:\"bad | / \\\"quoted\\\"\"\n\n## Done\n\n- [x] #3 old via:self\n";

    #[test]
    fn parses_fields_and_skips_fences() {
        let t = parse(DOC);
        assert_eq!(t.len(), 3);
        assert_eq!(t[0].id, 1);
        assert_eq!(t[0].area.as_deref(), Some("faces"));
        assert_eq!(t[0].level, Some(3));
        assert_eq!(t[0].needs, vec![2]);
        assert_eq!(t[0].body, vec!["body line"]);
        assert_eq!(t[0].text, "real one");
        assert_eq!(t[1].rework.as_deref(), Some("bad | / \"quoted\""));
        assert_eq!(t[2].state, State::Done);
        assert_eq!(t[2].section.as_deref(), Some("## Done"));
    }

    #[test]
    fn set_field_round_trips() {
        let l = "- [ ] #5 do it  @a !2 >agent";
        let l2 = set_field(l, Kind::Rework, Some("a \"b\" | c"));
        let l3 = set_field(&l2, Kind::Rework, None);
        assert_eq!(l3, l);
        assert_eq!(parse(&l2)[0].rework.as_deref(), Some("a \"b\" | c"));
        assert_eq!(
            set_field(l, Kind::Level, Some("4")),
            "- [ ] #5 do it  @a >agent !4"
        );
    }

    #[test]
    fn moves_block_with_body_and_leaves_fence_alone() {
        let mut d = Doc::new(DOC);
        d.update(1, |l| set_mark(l, State::Done), Some("## Done"), None)
            .unwrap();
        let out = d.text();
        assert!(out.contains("```\n- [ ] #1 example @x\n```"));
        assert!(out.ends_with(
            "- [x] #3 old via:self\n- [x] #1 real one @faces !3 >agent needs:#2\n  body line\n"
        ));
        assert_eq!(parse(&out).len(), 3);
    }

    #[test]
    fn splits_titles_without_losing_text() {
        let t = "Recreate the editor on the site: render every one of the 329 slider values a decoded preset carries. The data is all there and none of it is a guess.";
        let (title, rest) = split_title(t, 120).unwrap();
        assert_eq!(
            title,
            "Recreate the editor on the site: render every one of the 329 slider values a decoded preset carries."
        );
        assert_eq!(rest, "The data is all there and none of it is a guess.");
        assert!(split_title("short", 120).is_none());
        let line = "- [ ] #5 still >manual prose here  @a !2 >agent branch:x/y";
        assert_eq!(
            set_text(line, "new"),
            "- [ ] #5 new @a !2 >agent branch:x/y"
        );
    }

    #[test]
    fn split_at_word_boundary_without_ellipsis() {
        // When splitting at a word boundary, there should be no ellipsis because
        // the rest is preserved in the body, not lost.
        let t =
            "Build the thing on the site so that every reader sees it correctly and understands";
        let (title, rest) = split_title(t, 60).unwrap();
        // The title should NOT end with "…" because the text is not lost, it's in the body
        assert!(
            !title.ends_with('…'),
            "title should not end with ellipsis: {}",
            title
        );
        assert_eq!(
            title,
            "Build the thing on the site so that every reader sees it"
        );
        assert_eq!(rest, "correctly and understands");
    }

    #[test]
    fn creates_missing_section() {
        let mut d = Doc::new("# T\n\n## Done\n");
        d.insert(vec!["- [ ] #1 a".into()], "## Open", Some("## Done"));
        assert_eq!(d.text(), "# T\n\n## Open\n\n- [ ] #1 a\n\n## Done\n");
    }
}

use std::io::IsTerminal;

pub type Res<T> = Result<T, String>;

#[macro_export]
macro_rules! bail {
    ($($t:tt)*) => { return Err(format!($($t)*)) };
}

/// Set by `--no-color`, which main strips from the arguments before dispatch.
pub static NO_COLOR_FLAG: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// ANSI styling, off when the stream is not a terminal, NO_COLOR is set, TERM
/// is `dumb` or `--no-color` was given.
pub struct Sty(pub bool);

impl Sty {
    /// Styling for stdout.
    pub fn new() -> Self {
        Self::on(std::io::stdout().is_terminal())
    }
    /// Styling for stderr, where refusals go: its own terminal test, not stdout's.
    pub fn stderr() -> Self {
        Self::on(std::io::stderr().is_terminal())
    }
    fn on(tty: bool) -> Self {
        Sty(colour(
            tty,
            std::env::var_os("NO_COLOR").is_some(),
            std::env::var_os("TERM").as_deref(),
            NO_COLOR_FLAG.load(std::sync::atomic::Ordering::Relaxed),
        ))
    }
    fn wrap(&self, code: &str, s: &str) -> String {
        if self.0 {
            format!("\x1b[{code}m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }
    pub fn bold(&self, s: &str) -> String {
        self.wrap("1", s)
    }
    pub fn dim(&self, s: &str) -> String {
        self.wrap("2", s)
    }
    pub fn red(&self, s: &str) -> String {
        self.wrap("31", s)
    }
    /// The one-line refusal, `5w: <error>`, red when styled.
    pub fn refusal(&self, e: &str) -> String {
        self.red(&format!("5w: {e}"))
    }
}

/// Whether to colour: only on a terminal that is not `TERM=dumb`, with neither
/// NO_COLOR set nor `--no-color` given.
pub fn colour(tty: bool, no_color_env: bool, term: Option<&std::ffi::OsStr>, flag: bool) -> bool {
    tty && !no_color_env && !flag && term.is_none_or(|t| t != "dumb")
}

/// A tool's stderr as one line for a refusal: its non-blank lines, trimmed,
/// joined with `; `.
pub fn one_line(s: &str) -> String {
    let lines: Vec<&str> = s.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    lines.join("; ")
}

/// `#14`, `14` and `'#14'` all name task 14. Accepting the bare number is what
/// lets zsh users stop quoting: an unquoted `#14` there is a comment.
pub fn parse_id(s: &str) -> Res<u64> {
    let t = s.trim().trim_start_matches('#');
    t.parse::<u64>()
        .map_err(|_| format!("not a task id: {s:?} (want #14 or 14)"))
}

pub fn short(sha: &str) -> &str {
    &sha[..sha.len().min(12)]
}

pub fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let t: String = s.chars().take(n).collect();
        format!("{}…", t.trim_end())
    }
}

#[cfg(test)]
mod tests {
    use super::{Sty, colour, one_line};
    use std::ffi::OsStr;

    #[test]
    fn colour_is_off_for_dumb_terminals_and_no_color() {
        let xterm = Some(OsStr::new("xterm-256color"));
        assert!(colour(true, false, xterm, false));
        assert!(colour(true, false, None, false));
        assert!(!colour(false, false, xterm, false));
        assert!(!colour(true, true, xterm, false));
        assert!(!colour(true, false, Some(OsStr::new("dumb")), false));
        assert!(!colour(true, false, xterm, true));
    }

    #[test]
    fn one_line_joins_a_tools_stderr() {
        assert_eq!(
            one_line("error: bad\n\nhint: do this\r\n  hint: or that\n"),
            "error: bad; hint: do this; hint: or that"
        );
        assert_eq!(one_line("  \n"), "");
    }

    #[test]
    fn refusal_is_red_only_on_a_colour_terminal_and_otherwise_unchanged() {
        let xterm = Some(OsStr::new("xterm-256color"));
        let line = |tty, env, term, flag| Sty(colour(tty, env, term, flag)).refusal("no task #9");
        assert_eq!(
            line(true, false, xterm, false),
            "\x1b[31m5w: no task #9\x1b[0m"
        );
        for off in [
            line(false, false, xterm, false),
            line(true, true, xterm, false),
            line(true, false, Some(OsStr::new("dumb")), false),
            line(true, false, xterm, true),
            line(false, true, Some(OsStr::new("dumb")), true),
        ] {
            assert_eq!(off, "5w: no task #9");
        }
    }
}

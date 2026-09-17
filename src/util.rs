use std::io::IsTerminal;

pub type Res<T> = Result<T, String>;

#[macro_export]
macro_rules! bail {
    ($($t:tt)*) => { return Err(format!($($t)*)) };
}

/// Set by `--no-color`, which main strips from the arguments before dispatch.
pub static NO_COLOR_FLAG: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// ANSI styling, off when stdout is not a terminal, NO_COLOR is set, TERM is
/// `dumb` or `--no-color` was given.
pub struct Sty(pub bool);

impl Sty {
    pub fn new() -> Self {
        Sty(colour(
            std::io::stdout().is_terminal(),
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
}

/// Whether to colour: only on a terminal that is not `TERM=dumb`, with neither
/// NO_COLOR set nor `--no-color` given.
pub fn colour(tty: bool, no_color_env: bool, term: Option<&std::ffi::OsStr>, flag: bool) -> bool {
    tty && !no_color_env && !flag && term.is_none_or(|t| t != "dumb")
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
    use super::colour;
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
}

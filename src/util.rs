use std::io::IsTerminal;

pub type Res<T> = Result<T, String>;

#[macro_export]
macro_rules! bail {
    ($($t:tt)*) => { return Err(format!($($t)*)) };
}

/// ANSI styling, off when stdout is not a terminal or NO_COLOR is set.
pub struct Sty(pub bool);

impl Sty {
    pub fn new() -> Self {
        Sty(std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none())
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

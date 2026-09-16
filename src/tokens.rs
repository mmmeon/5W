//! An estimate of how many tokens a model spends reading a text.
//!
//! 5w ships no tokenizer — no dependencies, and every model family has its own
//! vocabulary — so this prices the pieces a byte-pair encoder's pre-tokenizer
//! cuts text into, at what such encoders typically merge them into:
//!
//! - a run of ASCII letters: 1 token per 7 letters (a common word is one token)
//! - a run of digits: 1 token per 3 digits (encoders group digits in threes)
//! - a run of ASCII punctuation: 1 token per 3 characters (`":`, `--`, `{"`)
//! - a run of spaces or tabs: 1 token, but a single space before anything else
//!   is free — it joins the piece after it
//! - any other character (a line break, `·`, `…`, `—`): 1 token
//!
//! A sha splits at every letter–digit boundary, as it does in the encoders.
//! Calibrated against o200k_base and cl100k_base on the benchmark's output and
//! this repository's prose: within 10% on each (README.md, *Measuring output*).
//! It is for comparing texts with each other — a trim, a regression, one command
//! against another — not for billing. Used by `tests/bench.rs`; a command that
//! prices text declares `mod tokens;` and calls [`estimate`].

/// Estimated tokens in `text`; the module comment has the heuristic.
pub fn estimate(text: &str) -> usize {
    let c: Vec<char> = text.chars().collect();
    let run = |mut i: usize, f: fn(&char) -> bool| {
        while c.get(i).is_some_and(f) {
            i += 1;
        }
        i
    };
    let blank = |x: &char| matches!(x, ' ' | '\t');
    let mut n = 0;
    let mut i = 0;
    while i < c.len() {
        let (end, tokens) = match c[i] {
            ' ' if c.get(i + 1).is_some_and(|x| !blank(x) && *x != '\n') => (i + 1, 0),
            ' ' | '\t' => (run(i, blank), 1),
            x if x.is_ascii_alphabetic() => {
                let e = run(i, char::is_ascii_alphabetic);
                (e, (e - i).div_ceil(7))
            }
            x if x.is_ascii_digit() => {
                let e = run(i, char::is_ascii_digit);
                (e, (e - i).div_ceil(3))
            }
            x if x.is_ascii_punctuation() => {
                let e = run(i, char::is_ascii_punctuation);
                (e, (e - i).div_ceil(3))
            }
            _ => (i + 1, 1),
        };
        n += tokens;
        i = end;
    }
    n
}

#[cfg(test)]
mod tests {
    use super::estimate;

    #[test]
    fn prices_the_pieces() {
        assert_eq!(estimate(""), 0);
        assert_eq!(estimate("ready"), 1);
        assert_eq!(estimate(" ready"), 1);
        assert_eq!(estimate("submitted"), 2);
        assert_eq!(estimate("5w ready"), 3);
        assert_eq!(estimate("#14"), 2);
        assert_eq!(estimate("1000000"), 3);
        assert_eq!(estimate("3f9c2a1b7d04"), 11);
        assert_eq!(estimate("a\nb"), 3);
        assert_eq!(estimate("  body"), 2);
        assert_eq!(estimate("\"id\":14,"), 5);
        assert_eq!(estimate("a · b"), 3);
    }
}

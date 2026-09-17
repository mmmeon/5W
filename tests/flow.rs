use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};

static N: AtomicU32 = AtomicU32::new(0);

/// The binary under test: the one cargo built, or a release artifact named by
/// FIVEW_TEST_BIN (a file called `5w`), so a release is tested as shipped.
fn bin5w() -> String {
    std::env::var("FIVEW_TEST_BIN").unwrap_or_else(|_| env!("CARGO_BIN_EXE_5w").to_string())
}

struct Repo {
    root: PathBuf,
    main: PathBuf,
}

fn env(c: &mut Command, root: &Path) {
    c.env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .env("NO_COLOR", "1")
        .env("FIVEW_WT_ROOT", root.join("wt"))
        // Hooks call `5w` from PATH: make that the binary under test.
        .env("PATH", path_with_5w());
}

fn env_with_relative_wt_root(c: &mut Command, _root: &Path) {
    c.env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .env("NO_COLOR", "1")
        // Don't set FIVEW_WT_ROOT to test relative path normalization
        .env_remove("FIVEW_WT_ROOT")
        // Hooks call `5w` from PATH: make that the binary under test.
        .env("PATH", path_with_5w());
}

impl Repo {
    fn new(name: &str) -> Repo {
        let root = std::env::temp_dir().join(format!(
            "5w-test-{name}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&root);
        let main = root.join("repo");
        std::fs::create_dir_all(&main).unwrap();
        let r = Repo { root, main };
        r.git(&r.main, &["init", "-q", "-b", "main"]);
        std::fs::write(r.main.join("README"), "hi\n").unwrap();
        r.git(&r.main, &["add", "README"]);
        r.git(&r.main, &["commit", "-qm", "init"]);
        r.ok(&r.main, &["init"]);
        r
    }

    fn cli(&self, cwd: &Path, args: &[&str]) -> Output {
        let mut c = Command::new(bin5w());
        c.args(args).current_dir(cwd);
        env(&mut c, &self.root);
        c.output().unwrap()
    }

    fn cli_with_relative_wt(&self, cwd: &Path, args: &[&str]) -> Output {
        let mut c = Command::new(bin5w());
        c.args(args).current_dir(cwd);
        env_with_relative_wt_root(&mut c, &self.root);
        c.output().unwrap()
    }

    fn ok_with_relative_wt(&self, cwd: &Path, args: &[&str]) -> String {
        let o = self.cli_with_relative_wt(cwd, args);
        let out = format!(
            "{}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        );
        assert!(o.status.success(), "5w {args:?} failed:\n{out}");
        out
    }

    fn ok(&self, cwd: &Path, args: &[&str]) -> String {
        let o = self.cli(cwd, args);
        let out = format!(
            "{}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        );
        assert!(o.status.success(), "5w {args:?} failed:\n{out}");
        out
    }

    fn fails(&self, cwd: &Path, args: &[&str]) -> String {
        let o = self.cli(cwd, args);
        let out = format!(
            "{}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        );
        assert!(
            !o.status.success(),
            "5w {args:?} should have failed:\n{out}"
        );
        // A refusal exits 1; 101 is a panic, which a message check alone can miss.
        assert_eq!(o.status.code(), Some(1), "5w {args:?}:\n{out}");
        out
    }

    /// A refusal: exit 1 and exactly one line on stderr, which is returned.
    fn refuses(&self, cwd: &Path, args: &[&str]) -> String {
        refusal(&self.cli(cwd, args), args)
    }

    fn git(&self, cwd: &Path, args: &[&str]) -> String {
        let mut c = Command::new("git");
        c.args(args).current_dir(cwd);
        env(&mut c, &self.root);
        let o = c.output().unwrap();
        assert!(
            o.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        String::from_utf8_lossy(&o.stdout).trim_end().to_string()
    }

    fn tasks(&self) -> String {
        std::fs::read_to_string(self.main.join("TASKS.md")).unwrap()
    }

    fn commit_in(&self, wt: &Path, file: &str, content: &str) {
        std::fs::write(wt.join(file), content).unwrap();
        self.git(wt, &["add", file]);
        self.git(wt, &["commit", "-qm", &format!("edit {file}")]);
    }

    fn wt(&self, branch: &str) -> PathBuf {
        PathBuf::from(self.ok(&self.main, &["wt", "path", branch]).trim())
    }

    /// Every commit 5w made must itself pass the protocol it enforces.
    fn lint_history(&self) {
        let root = self.git(&self.main, &["rev-list", "--max-parents=0", "main"]);
        self.ok(&self.main, &["lint", &format!("{root}..main")]);
    }

    fn line(&self, id: u64) -> String {
        self.tasks()
            .lines()
            .find(|l| l.contains(&format!("] #{id} ")))
            .unwrap_or("")
            .to_string()
    }
}

/// Check `o` is a refusal — exit 1 and exactly one line on stderr — and return that line.
fn refusal(o: &Output, args: &[&str]) -> String {
    let err = String::from_utf8_lossy(&o.stderr).to_string();
    assert_eq!(o.status.code(), Some(1), "5w {args:?}: {err}");
    assert!(
        err.ends_with('\n') && err.matches('\n').count() == 1,
        "5w {args:?}: a refusal is one line: {err:?}"
    );
    err
}

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn full_loop_add_submit_accept_ship() {
    let r = Repo::new("loop");
    r.ok(&r.main, &["add", "write the thing", "area:core", "level:2"]);
    assert!(r.line(1).starts_with("- [ ] #1 write the thing @core !2"));
    assert!(r.ok(&r.main, &["ready"]).contains("#1"));

    r.ok(&r.main, &["wt", "new", "core/thing"]);
    let wt = r.wt("core/thing");
    r.commit_in(&wt, "thing.txt", "done\n");

    // From inside the worktree, no branch argument, no quoting.
    r.ok(&wt, &["submit", "1"]);
    assert!(r.line(1).starts_with("- [~] #1"));
    assert!(r.line(1).contains("branch:core/thing"));
    assert!(r.ok(&r.main, &["review"]).contains("#1"));

    r.ok(&r.main, &["accept", "1"]);
    let l = r.line(1);
    assert!(
        l.starts_with("- [x] #1") && l.contains("via:review") && l.contains("reviewed:"),
        "{l}"
    );

    // Each state change is one commit touching only TASKS.md.
    for sha in r
        .git(&r.main, &["log", "--format=%H", "-4"])
        .lines()
        .take(3)
    {
        assert_eq!(
            r.git(&r.main, &["show", "--name-only", "--format=", sha]),
            "TASKS.md"
        );
    }

    // Accept moved main, so the branch is behind: ship says so, --sync fixes it.
    assert!(r.fails(&r.main, &["ship", "core/thing"]).contains("--sync"));
    let out = r.ok(&r.main, &["ship", "core/thing", "--sync"]);
    assert!(out.contains("rebased since; same change"), "{out}");
    assert!(r.main.join("thing.txt").exists());
    assert!(!wt.exists());
    assert_eq!(r.git(&r.main, &["branch", "--list", "core/thing"]), "");
    assert_eq!(r.git(&r.main, &["status", "--porcelain"]), "");
    r.lint_history();
}

#[test]
fn submit_and_accept_record_full_shas_and_short_ones_still_read() {
    let r = Repo::new("fullsha");
    r.ok(&r.main, &["add", "one"]);
    r.ok(&r.main, &["add", "two"]);
    r.ok(&r.main, &["wt", "new", "a/x"]);
    let wt = r.wt("a/x");
    r.commit_in(&wt, "x.txt", "x\n");
    let tip = r.git(&r.main, &["rev-parse", "a/x"]);

    // A prefix could come to name another object; the row holds the whole name,
    // and text output shows 12 of it.
    let out = r.ok(&wt, &["submit", "1"]);
    assert!(out.contains(&format!("a/x at {}\n", &tip[..12])), "{out}");
    assert!(
        r.line(1).ends_with(&format!(" submitted:{tip}")),
        "{}",
        r.line(1)
    );
    let shown = r.ok(&r.main, &["show", "1"]);
    assert!(
        shown.contains(&format!("submitted at {}", &tip[..12])),
        "{shown}"
    );
    assert!(!shown.contains(&tip[..13]), "{shown}");
    let json = r.ok(&r.main, &["review", "--json", "--full"]);
    assert!(json.contains(&format!("\"submitted\":\"{tip}\"")), "{json}");
    let out = r.ok(&r.main, &["accept", "1"]);
    assert!(
        out.contains(&format!("accepted at {}\n", &tip[..12])),
        "{out}"
    );
    assert!(
        r.line(1).ends_with(&format!(" reviewed:{tip}")),
        "{}",
        r.line(1)
    );
    r.lint_history();

    // Rows holding 12-hex prefixes, as earlier versions wrote them, still
    // review, show, accept, audit and ship.
    r.ok(&r.main, &["wt", "new", "b/y"]);
    let wt = r.wt("b/y");
    r.commit_in(&wt, "y.txt", "y\n");
    r.ok(&wt, &["submit", "2"]);
    let tip2 = r.git(&r.main, &["rev-parse", "b/y"]);
    let t = r
        .tasks()
        .replace(&tip, &tip[..12])
        .replace(&tip2, &tip2[..12]);
    std::fs::write(r.main.join("TASKS.md"), t).unwrap();
    r.git(&r.main, &["commit", "-qam", "chore(tasks): shorten"]);
    assert!(r.line(2).ends_with(&format!(" submitted:{}", &tip2[..12])));
    let json = r.ok(&r.main, &["review", "--json", "--full"]);
    assert!(
        json.contains(&format!("\"submitted\":\"{}\"", &tip2[..12]))
            && json.contains("\"moved\":0"),
        "{json}"
    );
    r.ok(&r.main, &["show", "2"]);
    r.ok(&r.main, &["accept", "2"]);
    assert!(
        r.line(2).ends_with(&format!(" reviewed:{tip2}")),
        "{}",
        r.line(2)
    );
    r.ok(&r.main, &["audit"]);
    let out = r.ok(&r.main, &["ship", "a/x", "--sync"]);
    assert!(
        out.contains(&format!("at {} (rebased since; same change)", &tip[..12])),
        "{out}"
    );
    r.ok(&r.main, &["ship", "b/y", "--sync"]);
    assert!(r.main.join("x.txt").exists() && r.main.join("y.txt").exists());
}

/// SHA-1 of `data`: enough to find colliding commit ids without asking git to
/// write hundreds of thousands of objects.
fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&((data.len() as u64) * 8).to_be_bytes());
    for block in msg.chunks(64) {
        let mut w = [0u32; 80];
        for (i, word) in block.chunks(4).enumerate() {
            w[i] = u32::from_be_bytes(word.try_into().unwrap());
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = h;
        for (i, wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5A827999),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6),
            };
            let t = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(*wi);
            (e, d, c, b, a) = (d, c, b.rotate_left(30), a, t);
        }
        for (x, y) in h.iter_mut().zip([a, b, c, d, e]) {
            *x = x.wrapping_add(y);
        }
    }
    let mut out = [0u8; 20];
    for (i, x) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&x.to_be_bytes());
    }
    out
}

/// Two children of `parent` with its tree, whose shas share their first 7 hex
/// digits: a birthday search over commits that differ only in their message,
/// hashed here, so git writes just the two that collide.
fn commits_sharing_a_prefix(r: &Repo, parent: &str) -> (String, String) {
    use std::io::Write;
    assert_eq!(parent.len(), 40, "the search hashes SHA-1 commit ids");
    let tree = r.git(&r.main, &["rev-parse", &format!("{parent}^{{tree}}")]);
    let body = |i: u32| {
        format!(
            "tree {tree}\nparent {parent}\nauthor t <t@example.com> 0 +0000\n\
             committer t <t@example.com> 0 +0000\n\ngrind {i}\n"
        )
    };
    let write = |i: u32| {
        let mut c = Command::new("git");
        c.args(["hash-object", "-t", "commit", "-w", "--stdin"])
            .current_dir(&r.main)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        env(&mut c, &r.root);
        let mut child = c.spawn().unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(body(i).as_bytes())
            .unwrap();
        let o = child.wait_with_output().unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        String::from_utf8(o.stdout).unwrap().trim().to_string()
    };
    let mut seen = std::collections::HashMap::new();
    // Expected near 2^14.5 tries; the odds of none among 2^21 are nil.
    for i in 0..1u32 << 21 {
        let content = body(i);
        let mut object = format!("commit {}\0", content.len()).into_bytes();
        object.extend_from_slice(content.as_bytes());
        let id = sha1(&object);
        // The first 7 hex digits: 28 bits.
        let key = u32::from_be_bytes(id[..4].try_into().unwrap()) >> 4;
        if let Some(j) = seen.insert(key, i) {
            let (a, b) = (write(j), write(i));
            assert_eq!(a[..7], b[..7], "hashed ids differ from git's: {a} {b}");
            assert_ne!(a, b);
            return (a, b);
        }
    }
    panic!("no two of 2^21 commits share 7 hex digits");
}

#[test]
fn a_recorded_prefix_that_is_ambiguous_or_too_short_authorises_nothing() {
    let r = Repo::new("ambiguous");
    r.ok(&r.main, &["add", "one"]);
    r.ok(&r.main, &["wt", "new", "a/x"]);
    let wt = r.wt("a/x");
    r.commit_in(&wt, "x.txt", "x\n");
    let w = r.git(&wt, &["rev-parse", "HEAD"]);
    // `a` is what was submitted and reviewed; `b` shares its first 7 digits.
    let (a, b) = commits_sharing_a_prefix(&r, &w);
    let rewrite = |from: &str, to: &str| {
        let t = r.tasks();
        assert!(t.contains(from), "{from} not in:\n{t}");
        std::fs::write(r.main.join("TASKS.md"), t.replace(from, to)).unwrap();
        r.git(&r.main, &["commit", "-qam", "chore(tasks): rewrite #1"]);
    };
    r.git(&wt, &["reset", "-q", "--hard", &a]);
    r.ok(&wt, &["submit", "1"]);
    r.git(&wt, &["reset", "-q", "--hard", &b]);

    // accept: a prefix both commits share names neither, nor does one under 7 digits.
    rewrite(&format!("submitted:{a}"), &format!("submitted:{}", &a[..7]));
    let err = r.refuses(&r.main, &["accept", "1"]);
    assert!(err.contains("is ambiguous"), "{err}");
    // review does not suggest the accept that refuses.
    let out = r.ok(&r.main, &["review"]);
    assert!(
        out.contains("is ambiguous") && !out.contains("→ 5w accept 1\n"),
        "{out}"
    );
    assert!(out.contains("5w accept 1 --at"), "{out}");
    rewrite(
        &format!("submitted:{}", &a[..7]),
        &format!("submitted:{}", &b[..6]),
    );
    let err = r.refuses(&r.main, &["accept", "1"]);
    assert!(err.contains("is not a commit name"), "{err}");
    assert!(r.line(1).starts_with("- [~] #1"));

    // ship: likewise for reviewed:, whatever the tip starts with.
    r.ok(&r.main, &["accept", "1", "--at", &a]);
    rewrite(&format!("reviewed:{a}"), &format!("reviewed:{}", &a[..7]));
    let err = r.refuses(&r.main, &["ship", "a/x"]);
    assert!(err.contains("is ambiguous"), "{err}");
    rewrite(
        &format!("reviewed:{}", &a[..7]),
        &format!("reviewed:{}", &b[..1]),
    );
    let err = r.refuses(&r.main, &["ship", "a/x"]);
    assert!(err.contains("is not a commit name"), "{err}");
}

#[test]
fn a_peers_uncommitted_row_is_never_swept_into_a_commit() {
    let r = Repo::new("peer");
    r.ok(&r.main, &["add", "first"]);
    let with_peer = r.tasks().replace(
        "- [ ] #1 first",
        "- [ ] #1 first\n- [ ] #7 a peer's row, not committed",
    );
    std::fs::write(r.main.join("TASKS.md"), with_peer).unwrap();

    r.ok(&r.main, &["add", "second"]);
    // The id skips past the uncommitted row, and the commit holds only #8.
    assert!(r.line(8).contains("second"));
    let diff = r.git(&r.main, &["show", "--format=", "HEAD"]);
    assert!(
        diff.contains("+- [ ] #8 second") && !diff.contains("#7"),
        "{diff}"
    );
    // The peer's row is still there, still uncommitted.
    let wdiff = r.git(&r.main, &["diff"]);
    assert!(wdiff.contains("+- [ ] #7 a peer's row"), "{wdiff}");
    assert!(
        !wdiff.contains("+- [ ] #8") && !wdiff.contains("-- [ ] #8"),
        "{wdiff}"
    );

    // Naming the peer's row pulls it into a commit — but #7 now sits below the
    // committed #8, and committing it would reuse an id: refused, nothing written.
    let before = r.git(&r.main, &["rev-parse", "main"]);
    let line = r.refuses(&r.main, &["set", "7", "level", "2"]);
    assert!(line.contains("#7") && line.contains("#9"), "{line}");
    assert_eq!(r.git(&r.main, &["rev-parse", "main"]), before);
    assert!(r.line(7).contains("a peer's row") && !r.line(7).contains("!2"));

    // Renumbered as the refusal says, it commits; every commit lints clean.
    let renumbered = r.tasks().replace("#7 a peer's row", "#9 a peer's row");
    std::fs::write(r.main.join("TASKS.md"), renumbered).unwrap();
    r.ok(&r.main, &["set", "9", "level", "2"]);
    let diff = r.git(&r.main, &["show", "--format=", "HEAD"]);
    assert!(diff.contains("+- [ ] #9 a peer's row"), "{diff}");
    r.lint_history();
}

#[test]
fn a_named_uncommitted_row_above_the_committed_ids_is_pulled_in() {
    let r = Repo::new("peer-above");
    r.ok(&r.main, &["add", "first"]);
    let with_hand = r
        .tasks()
        .replace("- [ ] #1 first", "- [ ] #1 first\n- [ ] #2 by hand");
    std::fs::write(r.main.join("TASKS.md"), with_hand).unwrap();
    r.ok(&r.main, &["set", "2", "level", "2"]);
    let diff = r.git(&r.main, &["show", "--format=", "HEAD"]);
    assert!(diff.contains("+- [ ] #2 by hand"), "{diff}");
    r.lint_history();
}

#[test]
fn reject_refuses_work_that_was_never_submitted() {
    let r = Repo::new("reject-open");
    r.ok(&r.main, &["add", "x"]);
    let out = r.fails(&r.main, &["reject", "1", "not good"]);
    assert!(out.contains("never submitted"), "{out}");
    assert!(r.line(1).starts_with("- [ ] #1") && !r.line(1).contains("rework:"));
    // Closed was never pending review either; `open` alone would not make it rejectable.
    r.ok(&r.main, &["done", "1", "--self"]);
    let out = r.fails(&r.main, &["reject", "1", "not good"]);
    assert!(
        out.contains("not pending review, so there is nothing to reject"),
        "{out}"
    );
    assert!(!out.contains("open 1` first"), "{out}");
    assert!(r.line(1).starts_with("- [x] #1") && !r.line(1).contains("rework:"));
}

#[test]
fn reject_reason_survives_every_special_character() {
    let r = Repo::new("reject");
    r.ok(&r.main, &["add", "x"]);
    r.ok(&r.main, &["wt", "new", "a/x"]);
    let wt = r.wt("a/x");
    r.commit_in(&wt, "f", "1\n");
    r.ok(&wt, &["submit", "1"]);
    let reason = r#"a | b / c "quoted" \ back"#;
    r.ok(&r.main, &["reject", "1", reason]);
    let show = r.ok(&r.main, &["show", "1"]);
    assert!(show.contains(&format!("rework: {reason}")), "{show}");
    assert!(r.ok(&r.main, &["delegate", "1"]).contains(reason));
}

#[test]
fn commits_after_submit_block_accept_until_reviewed() {
    let r = Repo::new("drift");
    r.ok(&r.main, &["add", "x"]);
    r.ok(&r.main, &["wt", "new", "a/x"]);
    let wt = r.wt("a/x");
    r.commit_in(&wt, "f", "1\n");
    r.ok(&wt, &["submit", "1"]);
    r.commit_in(&wt, "f", "2\n");
    assert!(r.ok(&r.main, &["review"]).contains("moved since submit"));
    let out = r.fails(&r.main, &["accept", "1"]);
    assert!(out.contains("after it was submitted"), "{out}");
    let out = r.fails(&r.main, &["accept", "1", "--at", "^a/x"]);
    assert_eq!(out, "5w: cannot resolve ^a/x\n");
    r.ok(&r.main, &["accept", "1", "--at", "a/x"]);
}

#[test]
fn accept_takes_several_ids_one_commit_each_and_stops_at_a_refusal() {
    let r = Repo::new("batchaccept");
    for (n, b) in [(1, "a/one"), (2, "a/two"), (3, "a/three"), (4, "a/four")] {
        r.ok(&r.main, &["add", &format!("task {n}")]);
        r.ok(&r.main, &["wt", "new", b]);
        let wt = r.wt(b);
        r.commit_in(&wt, "f", &format!("{n}\n"));
        if n != 3 {
            r.ok(&wt, &["submit", &n.to_string()]);
        }
    }
    let before = r.git(&r.main, &["rev-list", "--count", "main"]);
    // #3 was never submitted: #1 and #2 are accepted, #4 is not tried.
    let out = r.fails(&r.main, &["accept", "1", "2", "3", "4"]);
    assert!(out.contains("never submitted"), "{out}");
    assert!(
        out.contains("accepted #1 #2") && out.contains("not tried #4"),
        "{out}"
    );
    assert!(r.line(1).starts_with("- [x] #1") && r.line(1).contains("reviewed:"));
    assert!(r.line(2).starts_with("- [x] #2"));
    assert!(r.line(4).starts_with("- [~] #4"));
    let after = r.git(&r.main, &["rev-list", "--count", "main"]);
    assert_eq!(
        after.parse::<u32>().unwrap(),
        before.parse::<u32>().unwrap() + 2
    );
    // --at names one reviewed commit, so it takes one id.
    assert!(
        r.fails(&r.main, &["accept", "4", "1", "--at", "a/four"])
            .contains("one id")
    );
    r.ok(&r.main, &["accept", "4"]);
    r.lint_history();
}

impl Repo {
    /// `5w batch` with `input` on stdin: (succeeded, stdout and stderr).
    fn batch(&self, input: &str) -> (bool, String) {
        use std::io::Write;
        let mut c = Command::new(bin5w());
        c.arg("batch")
            .current_dir(&self.main)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        env(&mut c, &self.root);
        let mut child = c.spawn().unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        let o = child.wait_with_output().unwrap();
        let out = format!(
            "{}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        );
        (o.status.success(), out)
    }
}

#[test]
fn batch_commits_every_edit_in_one_commit_that_lint_and_audit_read() {
    let r = Repo::new("batch");
    for (n, b) in [(1, "a/one"), (2, "a/two"), (3, "a/three")] {
        r.ok(&r.main, &["add", &format!("task {n}")]);
        r.ok(&r.main, &["wt", "new", b]);
        let wt = r.wt(b);
        r.commit_in(&wt, "f", &format!("{n}\n"));
        r.ok(&wt, &["submit", &n.to_string()]);
    }
    let before = r.git(&r.main, &["rev-parse", "main"]);
    let (ok, out) = r.batch(
        "accept 1\n\naccept '#2'\nreject 3 'the \"why\" goes' here\nadd \"from a batch, with a comma\" area:core\n",
    );
    assert!(ok, "{out}");
    // One commit on top of the trunk, naming every edit; the body keeps each message.
    assert_eq!(r.git(&r.main, &["rev-parse", "main~1"]), before);
    assert_eq!(
        r.git(&r.main, &["log", "-1", "--format=%s", "main"]),
        "chore(tasks): accept #1, accept #2, reject #3, add #4"
    );
    let body = r.git(&r.main, &["log", "-1", "--format=%b", "main"]);
    assert!(
        body.contains("chore(tasks): accept #1\n")
            && body.contains("chore(tasks): reject #3\n")
            && body.contains("chore(tasks): add #4 — from a batch, with a comma"),
        "{body}"
    );
    assert!(r.line(1).starts_with("- [x] #1") && r.line(1).contains("reviewed:"));
    assert!(r.line(2).starts_with("- [x] #2"));
    assert!(
        r.line(3).starts_with("- [ ] #3")
            && r.line(3).contains("rework:\"the \\\"why\\\" goes here\"")
    );
    assert!(
        r.line(4)
            .starts_with("- [ ] #4 from a batch, with a comma @core")
    );

    // An edit that changes nothing is not named: `set` to the value a row has.
    r.ok(&r.main, &["set", "4", "level", "3"]);
    let tip = r.git(&r.main, &["rev-parse", "main"]);
    let (ok, out) = r.batch("set 4 level 3\n");
    assert!(ok && out.contains("nothing to commit"), "{out}");
    assert_eq!(r.git(&r.main, &["rev-parse", "main"]), tip);
    let (ok, out) = r.batch("set 4 level 3\nset 3 level 2\n");
    assert!(ok, "{out}");
    let (ok, out) = r.batch("set 4 level 3\nset 4 area docs\nset 3 level 1\n");
    assert!(ok, "{out}");
    assert_eq!(
        r.git(&r.main, &["log", "-1", "--format=%s", "main"]),
        "chore(tasks): set #4, set #3"
    );
    assert_eq!(r.git(&r.main, &["status", "--porcelain"]), "");
    r.lint_history();

    // Audit counts each edit, and knows the commit as 5w's.
    let out = r.ok(&r.main, &["audit"]);
    assert!(out.contains("review 2 accepted"), "{out}");
    assert!(out.contains("rework 1 rejections"), "{out}");
    assert!(out.contains("outside 0 queue commits"), "{out}");
}

#[test]
fn a_refusal_anywhere_in_a_batch_commits_nothing() {
    let r = Repo::new("batch-refused");
    for (n, b) in [(1, "a/one"), (2, "a/two")] {
        r.ok(&r.main, &["add", &format!("task {n}")]);
        r.ok(&r.main, &["wt", "new", b]);
        let wt = r.wt(b);
        r.commit_in(&wt, "f", &format!("{n}\n"));
    }
    r.ok(&r.main, &["submit", "1", "a/one"]);
    let (tip, tasks) = (r.git(&r.main, &["rev-parse", "main"]), r.tasks());
    let untouched = |out: &str| {
        assert_eq!(r.git(&r.main, &["rev-parse", "main"]), tip, "{out}");
        assert_eq!(r.tasks(), tasks, "{out}");
        assert_eq!(r.git(&r.main, &["status", "--porcelain"]), "", "{out}");
    };
    // #2 was never submitted: the accept of #1 before it is not committed either.
    let (ok, out) = r.batch("accept 1\nadd \"new\"\naccept 2\n");
    assert!(
        !ok && out.contains("batch line 3: #2 was never submitted"),
        "{out}"
    );
    assert!(out.contains("nothing committed"), "{out}");
    untouched(&out);
    // An edit of a row an earlier line already closed is checked against that line.
    let (ok, out) = r.batch("accept 1\naccept 1\n");
    assert!(
        !ok && out.contains("batch line 2: #1 is already closed"),
        "{out}"
    );
    untouched(&out);
    // A row takes one edit per batch: lint reads a commit row by row, and
    // submit-then-accept in one commit would read as accepted, never submitted.
    r.ok(&r.main, &["wt", "new", "a/three"]);
    r.commit_in(&r.wt("a/three"), "g", "3\n");
    r.ok(&r.main, &["add", "task 3"]);
    let (tip, tasks) = (r.git(&r.main, &["rev-parse", "main"]), r.tasks());
    let untouched = |out: &str| {
        assert_eq!(r.git(&r.main, &["rev-parse", "main"]), tip, "{out}");
        assert_eq!(r.tasks(), tasks, "{out}");
        assert_eq!(r.git(&r.main, &["status", "--porcelain"]), "", "{out}");
    };
    let (ok, out) = r.batch("submit 3 a/three\naccept 3\n");
    assert!(
        !ok && out.contains("batch line 2: #3 is already edited in this batch"),
        "{out}"
    );
    untouched(&out);
    // Several ids on one line refuse as one edit each, with nothing tallied.
    let (ok, out) = r.batch("accept 1 2\n");
    assert!(!ok && !out.contains("accepted #1"), "{out}");
    untouched(&out);
    for (input, want) in [
        ("ship a/one\n", "batch line 1: ship is not a batch edit"),
        ("accept 1\nadd \"open\n", "batch line 2: unclosed \""),
        ("add x --body -\n", "give --body its text inline"),
        ("\n", "usage: 5w batch"),
    ] {
        let (ok, out) = r.batch(input);
        assert!(!ok && out.contains(want), "{input:?}: {out}");
        untouched(&out);
    }
}

#[test]
fn review_json_is_one_object_per_submitted_task() {
    let r = Repo::new("reviewjson");
    assert_eq!(r.ok(&r.main, &["review", "--json"]), "");
    r.ok(&r.main, &["add", "x"]);
    r.ok(&r.main, &["add", "y"]);
    r.ok(&r.main, &["wt", "new", "a/x"]);
    let wt = r.wt("a/x");
    r.commit_in(&wt, "f", "1\n");
    r.ok(&wt, &["submit", "1"]);
    r.commit_in(&wt, "f", "2\n");
    let tip = r.git(&wt, &["rev-parse", "HEAD"]);
    let out = r.ok(&r.main, &["review", "--json"]);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 1, "{out}");
    let j = lines[0];
    assert!(
        j.starts_with("{\"id\":1,\"state\":\"review\",")
            && j.ends_with('}')
            && j.contains("\"branch\":\"a/x\"")
            && j.contains(&format!("\"tip\":\"{}\"", tip.trim()))
            && j.contains("\"moved\":1,")
            && j.contains("\"diff\":\"1 file changed, 1 insertion(+)\"")
            && j.contains("\"behind\":1}")
            && j.contains("\"unmet\":[],\"rework\":null,\"tip\"")
            && !j.contains("\"lane\"")
            && !j.contains("\"body\""),
        "{j}"
    );
    let full = r.ok(&r.main, &["review", "--json", "--full"]);
    assert!(
        full.contains("\"lane\":")
            && full.contains("\"body\":[]")
            && full.contains("\"behind\":1}"),
        "{full}"
    );
    assert_eq!(r.ok(&r.main, &["review", "--ids"]), "1\n");
    let plain = r.ok(&r.main, &["review"]);
    assert!(plain.contains("\n→ 5w accept 1\n"), "{plain}");
    assert!(!out.contains('→'), "{out}");
}

#[test]
fn a_change_after_accept_blocks_ship() {
    let r = Repo::new("postaccept");
    r.ok(&r.main, &["add", "x"]);
    r.ok(&r.main, &["wt", "new", "a/x"]);
    let wt = r.wt("a/x");
    r.commit_in(&wt, "f", "1\n");
    r.ok(&wt, &["submit", "1"]);
    r.ok(&r.main, &["accept", "1"]);
    r.commit_in(&wt, "f", "sneaky\n");
    let out = r.fails(&r.main, &["ship", "a/x", "--sync"]);
    assert!(out.contains("is not the change #1 accepted"), "{out}");
    // Nothing irreversible happened.
    assert!(wt.exists());
    assert!(!r.main.join("f").exists());
}

#[test]
fn a_forced_ship_of_a_changed_branch_says_so_once_and_only_once_it_lands() {
    let r = Repo::new("postacceptforce");
    r.ok(&r.main, &["add", "x"]);
    r.ok(&r.main, &["wt", "new", "a/x"]);
    let wt = r.wt("a/x");
    r.commit_in(&wt, "f", "1\n");
    r.ok(&wt, &["submit", "1"]);
    r.ok(&r.main, &["accept", "1"]);
    r.commit_in(&wt, "f", "changed\n");
    // Accept moved main: behind, so the refusal stands alone, no --force notice first.
    let out = r.refuses(&r.main, &["ship", "a/x", "--force"]);
    assert!(
        out.contains("--sync") && !out.contains("changed since"),
        "{out}"
    );
    // The check runs before and after the rebase; the notice is said once.
    let out = r.ok(&r.main, &["ship", "a/x", "--force", "--sync"]);
    assert_eq!(
        out.matches("ship: #1's branch changed since review at ")
            .count(),
        1,
        "{out}"
    );
    assert!(r.main.join("f").exists());
}

#[test]
fn squash_lands_one_commit_on_the_verified_trunk() {
    let r = Repo::new("squash");
    r.ok(&r.main, &["add", "x"]);
    r.ok(&r.main, &["wt", "new", "a/x"]);
    let wt = r.wt("a/x");
    r.commit_in(&wt, "f", "1\n");
    r.commit_in(&wt, "g", "2\n");
    r.commit_in(&wt, "f", "3\n");
    r.ok(&wt, &["submit", "1"]);
    r.ok(&r.main, &["accept", "1"]);
    // The trunk gains a file after the review: a squash onto main-by-name
    // with a stale index would revert it.
    r.ok(&r.main, &["add", "y"]);
    let before = r.git(&r.main, &["rev-parse", "main"]);
    r.ok(&r.main, &["ship", "a/x", "--sync", "--squash"]);
    assert_eq!(
        r.git(
            &r.main,
            &["rev-list", "--count", &format!("{before}..main")]
        ),
        "1"
    );
    assert_eq!(std::fs::read_to_string(r.main.join("f")).unwrap(), "3\n");
    assert!(r.main.join("g").exists());
    assert!(r.tasks().contains("#2 y"), "the trunk's newer row survived");
}

#[test]
fn closing_needs_the_flag_that_matches_the_lane() {
    let r = Repo::new("done");
    r.ok(&r.main, &["add", "decide it", "lane:owner"]);
    assert!(
        r.fails(&r.main, &["done", "1", "--self"])
            .contains("--decided")
    );
    assert!(r.fails(&r.main, &["delegate", "1"]).contains("decision"));
    assert!(!r.ok(&r.main, &["ready"]).contains("#1 "));
    r.ok(&r.main, &["done", "1", "--decided"]);
    assert!(r.line(1).contains("via:decided"));
    r.ok(&r.main, &["open", "1"]);
    r.lint_history();
    assert!(r.line(1).starts_with("- [ ] #1") && !r.line(1).contains("via:"));
    // Open moves it back out of Done.
    let t = r.tasks();
    assert!(t.find("#1 decide").unwrap() < t.find("## Done").unwrap());
}

#[test]
fn a_flag_a_command_does_not_take_is_refused() {
    let r = Repo::new("flags");
    // A lane's close word is configuration: the flag `done` takes comes from it.
    let cfg = std::fs::read_to_string(r.main.join(".5w.toml"))
        .unwrap()
        .replace(
            "[lanes.owner]\nkind = \"decision\"",
            "[lanes.owner]\nkind = \"decision\"\nclose = \"signed-off\"",
        );
    std::fs::write(r.main.join(".5w.toml"), cfg).unwrap();
    r.git(&r.main, &["commit", "-qam", "owner closes signed-off"]);
    r.ok(&r.main, &["add", "x", "--body", "--x text"]);
    r.ok(&r.main, &["add", "y", "lane:owner"]);
    r.ok(&r.main, &["add", "z"]);
    let before = r.git(&r.main, &["rev-parse", "HEAD"]);
    for (args, cmd) in [
        (&["ready", "--bogus"][..], "ready"),
        (&["ready", "@x", "--jsn"], "ready"),
        (&["next", "--bogus"], "next"),
        (&["ls", "--bogus"], "ls"),
        (&["blocked", "--bogus"], "blocked"),
        (&["all", "--bogus"], "all"),
        (&["show", "1", "--ids"], "show"),
        (&["review", "--bogus"], "review"),
        (&["delegate", "1", "--bogus"], "delegate"),
        (&["branch", "1", "--bogus"], "branch"),
        (&["doctor", "--bogus"], "doctor"),
        (&["levels", "--bogus"], "levels"),
        (&["archive", "--bogus"], "archive"),
        (&["open", "1", "--bogus"], "open"),
        (&["split", "--bogus"], "split"),
        (&["add", "w", "--bogus"], "add"),
        (&["accept", "1", "--bogus"], "accept"),
        (&["done", "3", "--self", "--bogus"], "done"),
        (&["done", "2", "--signed-off", "--bogus"], "done"),
    ] {
        let out = r.fails(&r.main, args);
        let flag = args.iter().rev().find(|a| a.starts_with("--")).unwrap();
        assert_eq!(
            out,
            format!("5w: unknown flag {flag} for {cmd} (5w {cmd} --help)\n"),
            "{args:?}"
        );
    }
    assert_eq!(r.git(&r.main, &["rev-parse", "HEAD"]), before);
    // Every documented flag still works.
    for args in [
        &["ready", "--json", "--ids", "--limit", "1", "--full"][..],
        &["next", "--json"],
        &["ls", "--ids", "--limit", "1"],
        &["list", "--full"],
        &["blocked", "--json"],
        &["all", "--ids", "--full"],
        &["show", "1", "--json"],
        &["review", "--checklist"],
        &["review", "--json", "--full"],
        &["review", "--ids"],
        &["split", "--all"],
    ] {
        r.ok(&r.main, args);
    }
    assert!(r.tasks().contains("\n  --x text\n"), "{}", r.tasks());
    r.ok(&r.main, &["done", "2", "--signed-off"]);
    assert!(r.line(2).contains("via:signed-off"), "{}", r.line(2));
    r.ok(&r.main, &["done", "3", "--self"]);
    r.ok(&r.main, &["open", "3"]);
    r.ok(&r.main, &["wt", "new", "a/x"]);
    let wt = r.wt("a/x");
    r.commit_in(&wt, "f", "1\n");
    r.ok(&wt, &["submit", "1"]);
    r.ok(&r.main, &["reject", "1", "--x reason", "--and-more"]);
    assert!(
        r.line(1).contains("rework:\"--x reason --and-more\""),
        "{}",
        r.line(1)
    );
    r.ok(&r.main, &["accept", "1", "--force", "--at", "a/x"]);
    r.ok(&r.main, &["archive"]);
    r.ok(&r.main, &["levels"]);
    r.ok(&r.main, &["doctor"]);
}

#[test]
fn a_flag_wt_report_init_lint_ship_ci_or_update_files_does_not_take_is_refused() {
    let r = Repo::new("unknown-flag-tools");
    r.ok(&r.main, &["wt", "new", "a/x"]);
    r.ok(&r.main, &["report", "something odd"]);
    let before = r.git(&r.main, &["rev-parse", "HEAD"]);
    for (args, cmd) in [
        (&["wt", "ls", "--bogus"][..], "wt ls"),
        (&["wt", "setup", "--bogus"], "wt setup"),
        (&["wt", "path", "a/x", "--bogus"], "wt path"),
        (&["wt", "link", "--bogus"], "wt link"),
        (&["wt", "install", "--bogus"], "wt install"),
        (&["wt", "discard-copy", "--bogus"], "wt discard-copy"),
        (&["wt", "new", "b/y", "--bogus"], "wt new"),
        (&["wt", "add", "a/x", "--bogus"], "wt add"),
        (&["wt", "rm", "a/x", "--bogus"], "wt rm"),
        (&["wt", "prune", "--yes", "--bogus"], "wt prune"),
        (&["report", "list", "--bogus"], "report list"),
        (&["report", "show", "1", "--bogus"], "report show"),
        (&["report", "rm", "1", "--bogus"], "report rm"),
        (&["report", "send", "1", "--bogus"], "report send"),
        (&["init", "--bogus"], "init"),
        (&["lint", "--bogus"], "lint"),
        (&["lint", "HEAD", "--bogus"], "lint"),
        (&["ship", "a/x", "--bogus"], "ship"),
        (&["ci", "--bogus"], "ci"),
        (&["update-files", "--bogus"], "update-files"),
    ] {
        let out = r.refuses(&r.main, args);
        assert_eq!(
            out,
            format!("5w: unknown flag --bogus for {cmd} (5w {cmd} --help)\n"),
            "{args:?}"
        );
        // The help each refusal names exists.
        let help = &[&args[..args.len() - 1], &["--help"]].concat();
        assert!(r.ok(&r.main, help).contains("usage:"), "{help:?}");
    }
    assert_eq!(r.git(&r.main, &["rev-parse", "HEAD"]), before);
    assert!(r.wt("a/x").exists());
    assert!(!r.ok(&r.main, &["report", "list"]).contains("(no reports)"));
    // Every documented flag still works.
    for args in [
        &["wt", "ls"][..],
        &["wt", "setup"],
        &["wt", "new", "b/y", "--from", "a/x"],
        &["report", "send", "1", "--print"],
        &["init"],
        &["lint", "--staged"],
        &["lint", "HEAD"],
    ] {
        r.ok(&r.main, args);
    }
}

#[test]
fn a_refusal_that_once_appended_a_usage_block_is_one_line() {
    let r = Repo::new("one-line-refusals");
    r.ok(&r.main, &["add", "x"]);
    r.ok(&r.main, &["wt", "new", "a/x"]);
    let wt = r.wt("a/x");
    r.git(&wt, &["checkout", "-q", "--detach"]);
    let before = r.git(&r.main, &["rev-parse", "HEAD"]);
    for (cwd, args, want) in [
        (
            &r.main,
            &["ship", "-x"][..],
            "unknown flag -x for ship (5w ship --help)",
        ),
        (
            &wt,
            &["ship"],
            "not on a branch: name the one to ship (5w ship --help)",
        ),
        (
            &r.main,
            &["ci", "-x"],
            "unknown flag -x for ci (5w ci --help)",
        ),
        (
            &r.main,
            &["ci", "extra"],
            "ci takes only flags, not \"extra\" (5w ci --help)",
        ),
        (
            &r.main,
            &["update-files", "extra"],
            "update-files takes only --pin, not \"extra\" (5w update-files --help)",
        ),
        (
            &r.main,
            &["add", "-x", "y"],
            "text first, not a flag: -x (5w add --help)",
        ),
    ] {
        assert_eq!(r.refuses(cwd, args), format!("5w: {want}\n"), "{args:?}");
    }
    assert_eq!(r.git(&r.main, &["rev-parse", "HEAD"]), before);
}

#[test]
fn lint_refuses_a_dash_flag_or_a_second_argument_in_one_line() {
    let r = Repo::new("lint-args");
    r.ok(&r.main, &["add", "x"]);
    let flag = "5w: unknown flag -x for lint (5w lint --help)\n";
    let usage = "5w: lint takes one of --staged | <rev> | <from>..<to> (5w lint --help)\n";
    for (args, want) in [
        (&["lint", "-x"][..], flag),
        (&["lint", "HEAD", "-x"], flag),
        (&["lint", "HEAD", "--staged"], usage),
        (&["lint", "--staged", "HEAD"], usage),
        (&["lint", "HEAD", "extra"], usage),
        (&["lint", "HEAD~1..HEAD", "HEAD"], usage),
    ] {
        assert_eq!(r.fails(&r.main, args), want, "{args:?}");
    }
    for args in [
        &["lint"][..],
        &["lint", "--staged"],
        &["lint", "HEAD"],
        &["lint", "HEAD~1..HEAD"],
        &["lint", "help"],
        &["lint", "--help"],
        &["lint", "-h"],
    ] {
        r.ok(&r.main, args);
    }
}

#[test]
fn lint_refuses_a_revision_that_is_not_a_commit_in_one_line() {
    let r = Repo::new("lint-revs");
    r.ok(&r.main, &["add", "x"]);
    r.ok(&r.main, &["add", "y"]);
    // `^HEAD` resolves to `^<sha>`, which lint once took for a commit on a branch.
    for arg in [
        "^HEAD",
        "nope",
        "^HEAD~1..HEAD",
        "HEAD~1..^HEAD",
        "HEAD..nope",
        "^HEAD...",
    ] {
        let want =
            format!("5w: lint: {arg} is not a commit or a <from>..<to> range (5w lint --help)\n");
        assert_eq!(r.fails(&r.main, &["lint", arg]), want, "{arg}");
    }
    for arg in ["HEAD~1", "HEAD~1..", "..HEAD", "HEAD~1...HEAD"] {
        r.ok(&r.main, &["lint", arg]);
    }
}

#[test]
fn no_color_is_global_but_never_eats_text() {
    let r = Repo::new("nocolor");
    // Before, after or among a command's arguments, on every tool.
    r.ok(&r.main, &["--no-color", "add", "x", "--body", "--no-color"]);
    r.ok(&r.main, &["add", "y", "--no-color", "@a"]);
    assert!(r.tasks().contains("\n  --no-color\n"), "{}", r.tasks());
    assert!(r.line(2).contains("@a"), "{}", r.line(2));
    for args in [
        &["ready", "--no-color"][..],
        &["--no-color", "ready", "--json"],
        &["show", "--no-color", "1"],
        &["doctor", "--no-color"],
        &["wt", "--no-color", "ls"],
        &["report", "list", "--no-color"],
    ] {
        r.ok(&r.main, args);
    }
    r.ok(&r.main, &["wt", "new", "a/x", "--no-color"]);
    let wt = r.wt("a/x");
    r.commit_in(&wt, "f", "1\n");
    r.ok(&wt, &["submit", "--no-color", "1"]);
    // A reason is text once it begins.
    r.ok(
        &r.main,
        &["reject", "--no-color", "1", "fails", "--no-color"],
    );
    assert!(
        r.line(1).contains("rework:\"fails --no-color\""),
        "{}",
        r.line(1)
    );
}

#[test]
fn accept_refuses_work_that_was_never_submitted() {
    let r = Repo::new("unsubmitted");
    r.ok(&r.main, &["add", "x"]);
    assert!(
        r.fails(&r.main, &["accept", "1"])
            .contains("never submitted")
    );
}

#[test]
fn the_format_example_in_a_fence_is_never_a_task() {
    let r = Repo::new("fence");
    // The template's fenced example is `#<id>`; put a real-looking one in.
    let t = r.tasks().replace("- [ ] #<id> <what", "- [ ] #1 <what");
    std::fs::write(r.main.join("TASKS.md"), &t).unwrap();
    r.git(&r.main, &["commit", "-qam", "example"]);
    r.ok(&r.main, &["add", "real"]);
    // Minting skips even a fenced id, so no two lines ever share a number.
    r.ok(&r.main, &["done", "2", "--self"]);
    assert!(
        r.fails(&r.main, &["done", "1", "--self"])
            .contains("no task #1")
    );
    let after = r.tasks();
    assert!(after.contains("```\n- [ ] #1 <what"), "example untouched");
    assert!(after.contains("- [x] #2 real via:self"));
}

#[test]
fn bodies_move_with_their_task() {
    let r = Repo::new("body");
    r.ok(
        &r.main,
        &["add", "with body", "--body", "line one\n\nline two"],
    );
    r.ok(&r.main, &["done", "1", "--self"]);
    assert!(
        r.tasks()
            .ends_with("- [x] #1 with body via:self\n  line one\n  line two\n"),
        "{}",
        r.tasks()
    );
    assert!(r.ok(&r.main, &["show", "1"]).contains("line two"));
}

#[test]
fn concurrent_adds_mint_distinct_ids() {
    let r = Repo::new("concurrent");
    let handles: Vec<_> = (0..8)
        .map(|i| {
            let mut c = Command::new(bin5w());
            c.args(["add", &format!("task {i}")]).current_dir(&r.main);
            env(&mut c, &r.root);
            c.spawn().unwrap()
        })
        .collect();
    for mut h in handles {
        assert!(h.wait().unwrap().success());
    }
    for id in 1..=8 {
        assert!(!r.line(id).is_empty(), "missing #{id}:\n{}", r.tasks());
    }
    r.ok(&r.main, &["doctor"]);
    assert_eq!(r.git(&r.main, &["status", "--porcelain"]), "");
}

#[test]
fn stacks_ship_bottom_first_and_children_reparent() {
    let r = Repo::new("stack");
    r.ok(&r.main, &["wt", "new", "s/a"]);
    let a = r.wt("s/a");
    r.commit_in(&a, "a", "a\n");
    r.ok(&r.main, &["wt", "new", "s/b", "--from", "s/a"]);
    let b = r.wt("s/b");
    r.commit_in(&b, "b", "b\n");
    assert!(
        r.fails(&r.main, &["ship", "s/b"])
            .contains("ship that first")
    );
    r.ok(&r.main, &["ship", "s/a"]);
    assert_eq!(
        r.git(&r.main, &["config", "git-town-branch.s/b.parent"]),
        "main"
    );
    r.ok(&r.main, &["ship", "s/b"]);
    assert!(r.main.join("b").exists());
}

#[test]
fn a_stack_recorded_on_the_trunk_is_flagged_by_doctor_and_wt_ls() {
    let r = Repo::new("misstack");
    r.ok(&r.main, &["wt", "new", "s/a"]);
    let a = r.wt("s/a");
    r.commit_in(&a, "a", "a\n");
    // Made from inside s/a's worktree: stacked on s/a, and nothing to flag.
    r.ok(&a, &["wt", "new", "s/b"]);
    assert_eq!(
        r.git(&r.main, &["config", "git-town-branch.s/b.parent"]),
        "s/a"
    );
    r.commit_in(&r.wt("s/b"), "b", "b\n");
    assert!(!r.ok(&r.main, &["doctor"]).contains("records main"));
    // Made from the trunk's checkout, then moved onto s/a's work by hand.
    r.ok(&r.main, &["wt", "new", "s/c"]);
    let c = r.wt("s/c");
    r.git(&c, &["reset", "-q", "--hard", "s/b"]);
    r.commit_in(&c, "c", "c\n");
    let fix = "s/c holds s/b's unshipped commits but records main as its parent — `git config git-town-branch.s/c.parent s/b`";
    let out = r.ok(&r.main, &["doctor"]);
    assert!(out.contains(fix), "{out}");
    assert!(!out.contains("s/b holds"), "{out}");
    let out = r.ok(&r.main, &["wt", "ls"]);
    assert!(out.contains(fix), "{out}");
    r.git(&r.main, &["config", "git-town-branch.s/c.parent", "s/b"]);
    assert!(!r.ok(&r.main, &["wt", "ls"]).contains("note:"));
}

#[test]
fn a_stack_note_never_names_a_backup_or_a_child() {
    let r = Repo::new("misstackfp");
    r.ok(&r.main, &["wt", "new", "s/b"]);
    let b = r.wt("s/b");
    r.commit_in(&b, "b", "one\n");
    r.commit_in(&b, "b", "two\n");
    // A backup made with git: s/b holds it, and it is no parent.
    r.git(&r.main, &["branch", "backup", "s/b~1"]);
    let out = r.ok(&r.main, &["doctor"]);
    assert!(!out.contains("holds"), "{out}");
    // A child made on s/b, which then moves on: s/b holds the child's tip.
    r.ok(&b, &["wt", "new", "s/c"]);
    r.commit_in(&b, "b", "three\n");
    let out = r.ok(&r.main, &["doctor"]);
    assert!(!out.contains("holds"), "{out}");
    assert!(!r.ok(&r.main, &["wt", "ls"]).contains("note:"));
}

#[test]
fn sync_after_a_squashed_parent_replays_only_the_childs_commits() {
    let r = Repo::new("squashstack");
    r.ok(&r.main, &["add", "bottom"]);
    r.ok(&r.main, &["add", "top"]);
    r.ok(&r.main, &["wt", "new", "s/a"]);
    let a = r.wt("s/a");
    r.commit_in(&a, "a", "one\n");
    r.commit_in(&a, "a", "two\n");
    r.ok(&a, &["submit", "1"]);
    r.ok(&a, &["wt", "new", "s/b"]);
    let b = r.wt("s/b");
    r.commit_in(&b, "b", "b\n");
    r.ok(&b, &["submit", "2"]);
    r.ok(&r.main, &["accept", "1", "2"]);
    r.ok(&r.main, &["ship", "s/a", "--sync", "--squash"]);
    // s/a's two commits are not on main, only the squash of them: replaying
    // them onto it conflicts on `a`.
    let out = r.ok(&r.main, &["ship", "s/b", "--sync"]);
    assert!(
        out.contains("authorised by #2") && out.contains("which landed; same change"),
        "{out}"
    );
    assert_eq!(std::fs::read_to_string(r.main.join("a")).unwrap(), "two\n");
    assert!(r.main.join("b").exists());
    assert_eq!(
        r.git(&r.main, &["rev-list", "--count", "main", "--", "a"]),
        "1"
    );
}

#[test]
fn an_unreviewed_ship_says_so_only_once_it_lands() {
    let r = Repo::new("unreviewed");
    r.ok(&r.main, &["wt", "new", "u/x"]);
    r.commit_in(&r.wt("u/x"), "x", "x\n");
    // Main moves, so u/x is behind: the refusal is the only line, not a notice first.
    r.ok(&r.main, &["add", "moves main"]);
    let out = r.refuses(&r.main, &["ship", "u/x"]);
    assert!(
        out.contains("behind main") && !out.contains("shipping unreviewed"),
        "{out}"
    );
    let out = r.ok(&r.main, &["ship", "u/x", "--sync"]);
    assert!(
        out.contains("ship: no task references u/x — shipping unreviewed"),
        "{out}"
    );
    assert!(r.main.join("x").exists());
}

#[test]
fn an_unreviewed_ship_the_fast_forward_refuses_is_one_line() {
    let r = Repo::new("unreviewedff");
    r.ok(&r.main, &["wt", "new", "u/x"]);
    let wt = r.wt("u/x");
    let edited = format!("{}\n", r.tasks());
    r.commit_in(&wt, "TASKS.md", &edited);
    // Main's checkout holds an uncommitted queue row: git refuses the fast-forward.
    std::fs::write(
        r.main.join("TASKS.md"),
        format!("{}- [ ] #9 peer\n", r.tasks()),
    )
    .unwrap();
    let out = r.refuses(&r.main, &["ship", "u/x"]);
    assert!(
        out.contains("fast-forward") && !out.contains("shipping unreviewed"),
        "{out}"
    );
    assert!(wt.exists());
}

#[test]
fn ship_accepted_lands_every_accepted_branch_bottom_of_stack_first() {
    let r = Repo::new("shipaccepted");
    r.ok(&r.main, &["add", "bottom"]);
    r.ok(&r.main, &["add", "top"]);
    r.ok(&r.main, &["add", "apart"]);
    r.ok(&r.main, &["add", "unreviewed"]);
    // Named so that name order and stack order disagree.
    r.ok(&r.main, &["wt", "new", "z/bottom"]);
    let bottom = r.wt("z/bottom");
    r.commit_in(&bottom, "bottom", "b\n");
    r.ok(&bottom, &["submit", "1"]);
    r.ok(&r.main, &["wt", "new", "a/top", "--from", "z/bottom"]);
    let top = r.wt("a/top");
    r.commit_in(&top, "top", "t\n");
    r.ok(&top, &["submit", "2"]);
    r.ok(&r.main, &["wt", "new", "m/apart"]);
    let apart = r.wt("m/apart");
    r.commit_in(&apart, "apart", "a\n");
    r.ok(&apart, &["submit", "3"]);
    r.ok(&r.main, &["wt", "new", "u/open"]);
    let open = r.wt("u/open");
    r.commit_in(&open, "open", "o\n");
    r.ok(&open, &["submit", "4"]);
    r.ok(&r.main, &["accept", "2", "1", "3"]);

    assert!(
        r.fails(&r.main, &["ship", "--accepted", "-m", "x", "--squash"])
            .contains("drop it")
    );
    assert!(
        r.fails(&r.main, &["ship", "--accepted", "z/bottom"])
            .contains("drop the branch name")
    );

    let out = r.ok(&r.main, &["ship", "--accepted", "--sync"]);
    let at = |b: &str| {
        out.find(&format!("ship: {b} is on main"))
            .unwrap_or(usize::MAX)
    };
    assert!(at("z/bottom") < at("a/top"), "{out}");
    assert!(at("m/apart") < usize::MAX, "{out}");
    for f in ["bottom", "top", "apart"] {
        assert!(r.main.join(f).exists(), "{f} not landed:\n{out}");
    }
    assert!(!r.main.join("open").exists());
    r.git(
        &r.main,
        &["rev-parse", "--verify", "-q", "refs/heads/u/open"],
    );
    assert!(
        r.ok(&r.main, &["ship", "--accepted"])
            .contains("no accepted branch")
    );
}

/// An accepted parent whose branch was deleted without shipping: the child,
/// rebased onto main without it, must not pass as "on the parent, which landed"
/// — whatever the parent's file is called.
fn a_parent_that_never_landed_does_not_authorise_its_child(name: &str) {
    let r = Repo::new("unlanded");
    r.ok(&r.main, &["add", "bottom"]);
    r.ok(&r.main, &["add", "top"]);
    r.ok(&r.main, &["wt", "new", "s/a"]);
    let a = r.wt("s/a");
    std::fs::write(a.join(name), "a\n").unwrap();
    r.git(&a, &["add", "-A"]);
    r.git(&a, &["commit", "-qm", "parent"]);
    r.ok(&a, &["submit", "1"]);
    r.ok(&r.main, &["wt", "new", "s/b", "--from", "s/a"]);
    let b = r.wt("s/b");
    r.commit_in(&b, "b", "b\n");
    r.ok(&b, &["submit", "2"]);
    r.ok(&r.main, &["accept", "1", "2"]);
    let a_tip = r.git(&r.main, &["rev-parse", "s/a"]);
    r.ok(&r.main, &["wt", "rm", "s/a"]);
    r.git(&r.main, &["branch", "-D", "s/a"]);
    r.git(&b, &["rebase", "-q", "--onto", "main", &a_tip]);
    let out = r.fails(&r.main, &["ship", "s/b"]);
    assert!(
        out.contains("is not the change #2 accepted"),
        "{name}: {out}"
    );
    assert!(!r.main.join("b").exists());
}

#[test]
fn a_parent_that_never_landed_does_not_authorise_its_child_plain_name() {
    a_parent_that_never_landed_does_not_authorise_its_child("parent.txt");
}

#[test]
fn a_parent_that_never_landed_does_not_authorise_its_child_non_ascii_name() {
    a_parent_that_never_landed_does_not_authorise_its_child("é");
}

#[test]
fn a_parent_that_never_landed_does_not_authorise_its_child_pathspec_magic_name() {
    a_parent_that_never_landed_does_not_authorise_its_child(":!*");
}

#[test]
fn a_parent_that_never_landed_does_not_authorise_its_child_when_it_changed_a_submodule() {
    // A repository that hides submodules from diffs must not hide a parent
    // whose only change is a gitlink.
    let r = Repo::new("unlandedsub");
    r.git(&r.main, &["config", "diff.ignoreSubmodules", "all"]);
    r.ok(&r.main, &["add", "bottom"]);
    r.ok(&r.main, &["add", "top"]);
    r.ok(&r.main, &["wt", "new", "s/a"]);
    let a = r.wt("s/a");
    let sha = r.git(&a, &["rev-parse", "HEAD"]);
    r.git(
        &a,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{sha},sub"),
        ],
    );
    r.git(&a, &["commit", "-qm", "parent"]);
    r.ok(&a, &["submit", "1"]);
    r.ok(&r.main, &["wt", "new", "s/b", "--from", "s/a"]);
    let b = r.wt("s/b");
    r.commit_in(&b, "b", "b\n");
    r.ok(&b, &["submit", "2"]);
    r.ok(&r.main, &["accept", "1", "2"]);
    let a_tip = r.git(&r.main, &["rev-parse", "s/a"]);
    // The gitlink has no checkout, so the worktree reads as modified.
    r.git(
        &r.main,
        &["worktree", "remove", "--force", a.to_str().unwrap()],
    );
    r.git(&r.main, &["branch", "-D", "s/a"]);
    r.git(&b, &["rebase", "-q", "--onto", "main", &a_tip]);
    let out = r.fails(&r.main, &["ship", "s/b"]);
    assert!(out.contains("is not the change #2 accepted"), "{out}");
    assert!(!r.main.join("b").exists());
}

#[test]
fn a_stacked_branch_changed_after_review_is_refused_once_its_parent_landed() {
    let r = Repo::new("stackdrift");
    r.ok(&r.main, &["add", "bottom"]);
    r.ok(&r.main, &["add", "top"]);
    r.ok(&r.main, &["wt", "new", "s/a"]);
    let a = r.wt("s/a");
    r.commit_in(&a, "a", "a\n");
    r.ok(&a, &["submit", "1"]);
    r.ok(&r.main, &["wt", "new", "s/b", "--from", "s/a"]);
    let b = r.wt("s/b");
    r.commit_in(&b, "b", "b\n");
    r.ok(&b, &["submit", "2"]);
    r.ok(&r.main, &["accept", "1", "2"]);
    r.commit_in(&b, "b", "b, after review\n");
    let out = r.fails(&r.main, &["ship", "--accepted", "--sync"]);
    assert!(
        out.contains("stopped at s/b: s/b is not the change #2 accepted")
            && out.contains("(shipped s/a)"),
        "{out}"
    );
}

#[test]
fn ship_accepted_stops_at_the_first_refusal_naming_the_branch() {
    let r = Repo::new("shipacceptedstop");
    r.ok(&r.main, &["add", "one"]);
    r.ok(&r.main, &["add", "two"]);
    for (id, b) in [("1", "a/one"), ("2", "b/two")] {
        r.ok(&r.main, &["wt", "new", b]);
        let wt = r.wt(b);
        r.commit_in(&wt, id, "x\n");
        r.ok(&wt, &["submit", id]);
    }
    r.ok(&r.main, &["accept", "1", "2"]);
    // Without --sync both are behind main: the first one refuses, nothing lands.
    let out = r.fails(&r.main, &["ship", "--accepted"]);
    let refusal = out.lines().last().unwrap_or("");
    assert!(
        refusal.starts_with("5w: stopped at a/one: ")
            && refusal.contains("--sync")
            && refusal.contains("none shipped"),
        "{out}"
    );
    assert!(!r.main.join("1").exists());
    // A dirty worktree stops b/two after a/one landed; the refusal says both.
    std::fs::write(r.wt("b/two").join("stray"), "s\n").unwrap();
    r.git(&r.wt("b/two"), &["add", "stray"]);
    let out = r.fails(&r.main, &["ship", "--accepted", "--sync"]);
    let refusal = out.lines().last().unwrap_or("");
    assert!(
        refusal.starts_with("5w: stopped at b/two: ")
            && refusal.contains("uncommitted")
            && refusal.contains("(shipped a/one)"),
        "{out}"
    );
    assert!(r.main.join("1").exists() && !r.main.join("2").exists());
    r.git(
        &r.main,
        &["rev-parse", "--verify", "-q", "refs/heads/b/two"],
    );
}

#[test]
fn duplicate_ids_stop_every_command() {
    let r = Repo::new("dup");
    r.ok(&r.main, &["add", "one"]);
    let t = r
        .tasks()
        .replace("- [ ] #1 one", "- [ ] #1 one\n- [ ] #1 again");
    std::fs::write(r.main.join("TASKS.md"), t).unwrap();
    std::fs::write(r.main.join("DONE.md"), "- [x] #1 old\n").unwrap();
    let out = r.refuses(&r.main, &["ready"]);
    assert_eq!(
        out,
        "5w: duplicate ids — fix before anything else: #1 lines 17 and 18 of TASKS.md; \
         #1 in both TASKS.md and DONE.md\n"
    );
    assert!(r.fails(&r.main, &["doctor"]).contains("appears twice"));
}

#[test]
fn doctor_and_reads_catch_duplicates_and_conflict_markers_in_the_archive() {
    let r = Repo::new("dup-archive");
    r.ok(&r.main, &["add", "one"]);
    // Markers left by a hand merge of DONE.md repeat #2 on both sides.
    let done = "# Done\n\n- [x] #2 two\n<<<<<<< HEAD\n- [x] #3 three\n=======\n\
                - [x] #2 two\n>>>>>>> side\n\n```\n<<<<<<< not a marker\n```\n";
    std::fs::write(r.main.join("DONE.md"), done).unwrap();
    let out = r.fails(&r.main, &["doctor"]);
    assert!(
        out.contains("#2 appears twice in DONE.md: lines 3 and 7")
            && out.contains(
                "DONE.md line 4 holds a git conflict marker — resolve the conflict, then commit"
            )
            && !out.contains("line 11")
            && !out.contains("TASKS.md line")
            && !out.contains(" ok "),
        "{out}"
    );
    assert!(
        r.refuses(&r.main, &["ready"])
            .contains("#2 lines 3 and 7 of DONE.md"),
    );
}

#[test]
fn a_subcommand_help_prints_only_that_command() {
    let r = Repo::new("subhelp");
    let full = r.ok(&r.main, &["--help"]);
    for (cmd, want) in [
        ("add", "usage: 5w add <text> [fields]"),
        ("submit", "usage: 5w submit <id> [branch]"),
        ("accept", "usage: 5w accept <id>... [--at <rev>] [--force]"),
        ("reject", "usage: 5w reject <id> <reason>"),
        ("show", "usage: 5w show <id> [--json]"),
        ("ready", "usage: 5w ready [filters] [out]"),
        ("levels", "usage: 5w blocked | levels | all"),
        ("reopen", "usage: 5w open <id>"),
        (
            "hook",
            "usage: 5w hook install | uninstall [pre-commit | pre-receive]",
        ),
        ("doctor", "usage: 5w doctor"),
    ] {
        let o = r.cli(&r.main, &[cmd, "--help"]);
        assert!(o.status.success(), "{cmd} --help failed");
        let out = String::from_utf8_lossy(&o.stdout);
        assert!(out.starts_with(want), "{cmd} -h:\n{out}");
        assert!(
            out.lines().count() <= 3,
            "{cmd} -h is the whole listing:\n{out}"
        );
        assert!(o.stderr.is_empty());
    }
    let ready = r.ok(&r.main, &["ready", "-h"]);
    assert!(ready.contains("\nfilters ") && ready.contains("\nout "));
    assert!(r.ok(&r.main, &["show", "-h"]).contains("\nids "));
    // Commands with their own usage keep it; one USAGE does not list, the full listing.
    assert!(
        r.ok(&r.main, &["ship", "--help"])
            .contains("usage: 5w ship")
    );
    assert!(r.ok(&r.main, &["wt", "--help"]).contains("wt"));
    // The full listing names every wt subcommand the binary dispatches.
    assert!(
        full.contains("wt <new|add|ls|path|rm|prune|link|install|setup|discard-copy>"),
        "{full}"
    );
    assert_eq!(r.ok(&r.main, &["branch", "--help"]), full);
    // The full listing links the README on the web; a command's help does not.
    assert!(
        full.contains(&format!("docs     {}#readme", env!("CARGO_PKG_REPOSITORY"))),
        "{full}"
    );
    assert!(!r.ok(&r.main, &["ready", "-h"]).contains("docs "));
    assert!(!r.ok(&r.main, &["show", "-h"]).contains("http"));
    // One example invocation under the usage line; a command's help does not repeat it.
    assert!(
        full.starts_with("usage: 5w <command> [args]\nexample  5w ready level:1\n"),
        "{full}"
    );
    assert!(!r.ok(&r.main, &["ready", "-h"]).contains("example"));
}

#[test]
fn symlinked_as_tasks_wt_ship() {
    let r = Repo::new("argv0");
    let bin = r.root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::os::unix::fs::symlink(bin5w(), bin.join("wt")).unwrap();
    let mut c = Command::new(bin.join("wt"));
    c.args(["new", "x/y"]).current_dir(&r.main);
    env(&mut c, &r.root);
    assert!(c.output().unwrap().status.success());
    assert!(r.git(&r.main, &["branch", "--list", "x/y"]).contains("x/y"));
}

// --- regressions from the adversarial review ---------------------------------------

#[test]
fn a_staged_edit_to_the_queue_keeps_the_new_row() {
    let r = Repo::new("staged");
    r.ok(&r.main, &["add", "first"]);
    let t = r
        .tasks()
        .replace("Ids are permanent", "NOTE staged. Ids are permanent");
    std::fs::write(r.main.join("TASKS.md"), t).unwrap();
    r.git(&r.main, &["add", "TASKS.md"]);
    r.ok(&r.main, &["add", "second"]);
    // The staged diff is the note alone; committing it keeps #2.
    let cached = r.git(&r.main, &["diff", "--cached"]);
    assert!(
        !cached.contains("+- [ ] #2") && !cached.contains("-- [ ] #2"),
        "{cached}"
    );
    r.git(&r.main, &["commit", "-qm", "note"]);
    let head = r.git(&r.main, &["show", "HEAD:TASKS.md"]);
    assert!(
        head.contains("#2 second") && head.contains("NOTE staged"),
        "{head}"
    );
}

#[test]
fn a_trunk_checked_out_in_a_linked_worktree_is_kept_in_step() {
    let r = Repo::new("linkedtrunk");
    r.git(&r.main, &["checkout", "-qb", "side"]);
    let mainwt = r.root.join("mainwt");
    r.git(
        &r.main,
        &["worktree", "add", "-q", mainwt.to_str().unwrap(), "main"],
    );
    r.ok(&r.main, &["add", "x"]);
    assert_eq!(r.git(&mainwt, &["status", "--porcelain"]), "");
    std::fs::write(mainwt.join("other"), "o\n").unwrap();
    r.git(&mainwt, &["add", "other"]);
    r.git(&mainwt, &["commit", "-qm", "work"]);
    assert!(r.git(&mainwt, &["show", "HEAD:TASKS.md"]).contains("#1 x"));
}

#[test]
fn a_closed_task_cannot_be_repointed_at_another_branch() {
    let r = Repo::new("repoint");
    r.ok(&r.main, &["add", "old chore"]);
    r.ok(&r.main, &["done", "1", "--self"]);
    assert!(
        r.fails(&r.main, &["set", "1", "branch", "evil"])
            .contains("closed")
    );
}

#[test]
fn field_values_cannot_inject_lines() {
    let r = Repo::new("inject");
    r.ok(&r.main, &["add", "a"]);
    r.fails(
        &r.main,
        &[
            "set",
            "1",
            "branch",
            "x\n- [x] #7 forged via:review branch:evil",
        ],
    );
    r.fails(&r.main, &["set", "1", "branch", "a b"]);
    r.fails(&r.main, &["add", "b", "branch:x\n- [x] #9 forged"]);
    r.fails(&r.main, &["add", "c\r- [x] #9 forged"]);
    assert!(!r.tasks().contains("forged"));
}

#[test]
fn a_whitespace_change_after_review_blocks_ship() {
    let r = Repo::new("ws");
    r.ok(&r.main, &["add", "feature"]);
    r.ok(&r.main, &["wt", "new", "f/a"]);
    let wt = r.wt("f/a");
    r.commit_in(&wt, "a.py", "if ok:\n    run()\nsafe()\n");
    r.ok(&wt, &["submit", "1"]);
    r.ok(&r.main, &["accept", "1"]);
    r.commit_in(&wt, "a.py", "if ok:\n    run()\n    safe()\n");
    let out = r.fails(&r.main, &["ship", "f/a", "--sync"]);
    assert!(out.contains("is not the change #1 accepted"), "{out}");
    // Refused before the rebase, so the branch was never rewritten.
    assert!(!out.contains("rebasing"), "{out}");
}

/// A post-review change to `f` in a repository whose own diff settings might
/// hide it: `setup` runs in the trunk checkout (whose .git/config the worktrees
/// share) before the work starts. Ship must still refuse.
fn a_change_after_review_hidden_by_diff_config_blocks_ship(
    setup: impl Fn(&Repo),
    change: impl Fn(&Repo, &Path),
) {
    let r = Repo::new("diffconfig");
    setup(&r);
    r.ok(&r.main, &["add", "feature"]);
    r.ok(&r.main, &["wt", "new", "f/a"]);
    let wt = r.wt("f/a");
    r.commit_in(&wt, "f", "1\n");
    r.ok(&wt, &["submit", "1"]);
    r.ok(&r.main, &["accept", "1"]);
    change(&r, &wt);
    let out = r.fails(&r.main, &["ship", "f/a", "--sync"]);
    assert!(out.contains("is not the change #1 accepted"), "{out}");
    assert!(!r.main.join("f").exists());
}

fn attributes(r: &Repo, text: &str) {
    r.commit_in(&r.main, ".gitattributes", text);
}

#[test]
fn a_textconv_driver_does_not_hide_a_change_after_review() {
    a_change_after_review_hidden_by_diff_config_blocks_ship(
        |r| {
            attributes(r, "f diff=hide\n");
            r.git(
                &r.main,
                &["config", "diff.hide.textconv", "sh -c \"echo same\""],
            );
        },
        |r, wt| r.commit_in(wt, "f", "evil\n"),
    );
}

#[test]
fn a_no_diff_attribute_does_not_hide_a_change_after_review() {
    a_change_after_review_hidden_by_diff_config_blocks_ship(
        |r| attributes(r, "f -diff\n"),
        |r, wt| r.commit_in(wt, "f", "evil\n"),
    );
}

#[test]
fn ignored_submodules_do_not_hide_a_gitlink_added_after_review() {
    a_change_after_review_hidden_by_diff_config_blocks_ship(
        |r| {
            r.git(&r.main, &["config", "diff.ignoreSubmodules", "all"]);
        },
        |r, wt| {
            let sha = r.git(wt, &["rev-parse", "HEAD"]);
            r.git(
                wt,
                &[
                    "update-index",
                    "--add",
                    "--cacheinfo",
                    &format!("160000,{sha},sub"),
                ],
            );
            r.git(wt, &["commit", "-qm", "gitlink"]);
        },
    );
}

/// A replace ref makes git show one object as another: the post-review commit
/// or its blob dressed up as the reviewed one must not pass as the same change.
fn a_replace_ref_does_not_disguise_a_change_after_review(commit: bool) {
    let r = Repo::new("replace");
    r.ok(&r.main, &["add", "feature"]);
    r.ok(&r.main, &["wt", "new", "f/a"]);
    let wt = r.wt("f/a");
    r.commit_in(&wt, "f", "f\n");
    r.ok(&wt, &["submit", "1"]);
    r.ok(&r.main, &["accept", "1"]);
    r.git(&wt, &["rebase", "-q", "main"]);
    let rebased = r.git(&r.main, &["rev-parse", "f/a"]);
    r.commit_in(&wt, "f", "evil\n");
    let evil = r.git(&r.main, &["rev-parse", "f/a"]);
    if commit {
        r.git(
            &r.main,
            &["worktree", "remove", "--force", wt.to_str().unwrap()],
        );
        r.git(&r.main, &["replace", &evil, &rebased]);
    } else {
        let blob = |c: &str| r.git(&r.main, &["rev-parse", &format!("{c}:f")]);
        r.git(&r.main, &["replace", &blob(&evil), &blob(&rebased)]);
    }
    let out = r.fails(&r.main, &["ship", "f/a"]);
    assert!(out.contains("is not the change #1 accepted"), "{out}");
    assert_ne!(
        r.git(&r.main, &["rev-parse", "main"]),
        evil,
        "main moved to the post-review commit"
    );
}

#[test]
fn a_replaced_blob_does_not_disguise_a_change_after_review() {
    a_replace_ref_does_not_disguise_a_change_after_review(false);
}

#[test]
fn a_replaced_commit_does_not_disguise_a_change_after_review() {
    a_replace_ref_does_not_disguise_a_change_after_review(true);
}

#[test]
fn zero_diff_context_still_refuses_a_rebase_that_changed_nearby_lines() {
    let r = Repo::new("context0");
    r.git(&r.main, &["config", "diff.context", "0"]);
    r.commit_in(&r.main, "g", "1\n2\n3\n4\n5\n6\n7\n");
    r.ok(&r.main, &["add", "feature"]);
    r.ok(&r.main, &["wt", "new", "f/a"]);
    let wt = r.wt("f/a");
    r.commit_in(&wt, "g", "1\n2\n3\n4\n55\n6\n7\n");
    r.ok(&wt, &["submit", "1"]);
    r.ok(&r.main, &["accept", "1"]);
    // The trunk changes a line the reviewed hunk showed as context. (Digits
    // only, so no hunk header names a function and tells the two apart.)
    r.commit_in(&r.main, "g", "1\n2\n33\n4\n5\n6\n7\n");
    let out = r.fails(&r.main, &["ship", "f/a", "--sync"]);
    assert!(out.contains("is not the change #1 accepted"), "{out}");
}

#[test]
fn a_failing_fast_forward_changes_nothing() {
    let r = Repo::new("ffail");
    r.ok(&r.main, &["add", "feature"]);
    r.ok(&r.main, &["wt", "new", "f/a"]);
    let wt = r.wt("f/a");
    r.commit_in(&wt, "f1", "1\n");
    r.commit_in(&wt, "f2", "2\n");
    r.ok(&wt, &["submit", "1"]);
    r.ok(&r.main, &["accept", "1"]);
    // An untracked file in the trunk checkout the merge would overwrite.
    std::fs::write(r.main.join("f2"), "u\n").unwrap();
    r.fails(&r.main, &["ship", "f/a", "--sync", "--squash"]);
    assert!(wt.exists());
    assert_eq!(
        r.git(&r.main, &["rev-list", "--count", "main..f/a"]),
        "2",
        "branch not squashed"
    );
}

#[test]
fn ignored_files_in_the_worktree_stop_ship_until_discarded() {
    let r = Repo::new("ignored");
    std::fs::write(r.main.join(".gitignore"), ".env\nnode_modules\n").unwrap();
    r.git(&r.main, &["add", ".gitignore"]);
    r.git(&r.main, &["commit", "-qm", "ignore"]);
    r.ok(&r.main, &["wt", "new", "f/a"]);
    let wt = r.wt("f/a");
    r.commit_in(&wt, "x", "x\n");
    std::fs::create_dir_all(wt.join("node_modules/pkg")).unwrap();
    std::fs::write(wt.join("node_modules/pkg/i.js"), "\n").unwrap();
    std::fs::write(wt.join(".env"), "SECRET=1\n").unwrap();
    let out = r.fails(&r.main, &["ship", "f/a"]);
    assert!(
        out.contains(".env") && !out.contains("node_modules"),
        "{out}"
    );
    r.ok(&r.main, &["ship", "f/a", "--discard-ignored"]);
}

#[test]
fn a_crlf_queue_still_gates_its_branches() {
    let r = Repo::new("crlf");
    r.ok(&r.main, &["add", "feature", "branch:f/a"]);
    r.ok(&r.main, &["wt", "new", "f/a"]);
    let wt = r.wt("f/a");
    r.commit_in(&wt, "x", "x\n");
    let crlf = r.tasks().replace('\n', "\r\n");
    std::fs::write(r.main.join("TASKS.md"), crlf).unwrap();
    r.git(&r.main, &["commit", "-qam", "crlf"]);
    assert!(
        r.fails(&r.main, &["ship", "f/a", "--sync"])
            .contains("not accepted")
    );
    r.ok(&wt, &["submit", "1"]);
    let t = r.tasks();
    assert!(
        t.contains("branch:f/a submitted:") && t.contains("\r\n") && !t.contains("\r "),
        "{t:?}"
    );
    assert!(
        !t.replace("\r\n", "").contains('\n'),
        "line endings stay CRLF"
    );
}

#[test]
fn state_checks_read_the_committed_queue() {
    let r = Repo::new("handedit");
    r.ok(&r.main, &["add", "x"]);
    // A hand edit claims it was submitted; the trunk says otherwise.
    let t = r.tasks().replace("- [ ] #1 x", "- [~] #1 x");
    std::fs::write(r.main.join("TASKS.md"), t).unwrap();
    r.fails(&r.main, &["accept", "1"]);
}

#[test]
fn an_unclosed_fence_does_not_reuse_ids() {
    let r = Repo::new("fenceid");
    r.ok(&r.main, &["add", "one"]);
    let t = r.tasks().replace("## Open", "```\n## Open");
    std::fs::write(r.main.join("TASKS.md"), &t).unwrap();
    r.git(&r.main, &["commit", "-qam", "break"]);
    r.fails(&r.main, &["doctor"]);
    r.ok(&r.main, &["add", "two"]);
    assert!(r.tasks().contains("#2 two"));
}

#[test]
fn a_rebase_that_changes_the_reviewed_context_is_rolled_back() {
    let r = Repo::new("rollback");
    std::fs::write(r.main.join("c"), "1\n2\n3\n4\n5\n6\n7\n8\n").unwrap();
    r.git(&r.main, &["add", "c"]);
    r.git(&r.main, &["commit", "-qm", "c"]);
    r.ok(&r.main, &["add", "x"]);
    r.ok(&r.main, &["wt", "new", "f/a"]);
    let wt = r.wt("f/a");
    r.commit_in(&wt, "c", "1\nTWO\n3\n4\n5\n6\n7\n8\n");
    r.ok(&wt, &["submit", "1"]);
    r.ok(&r.main, &["accept", "1"]);
    let before = r.git(&r.main, &["rev-parse", "f/a"]);
    // The trunk changes the line next to the reviewed one: the rebase is clean,
    // but what lands is no longer exactly what was read.
    std::fs::write(r.main.join("c"), "1\n2\n3\n4\nFIVE\n6\n7\n8\n").unwrap();
    r.git(&r.main, &["commit", "-qam", "neighbour"]);
    let o = r.cli(&r.main, &["ship", "f/a", "--sync"]);
    assert!(String::from_utf8_lossy(&o.stdout).contains("rebasing"));
    let err = refusal(&o, &["ship", "f/a", "--sync"]);
    assert!(err.contains("; f/a is back at "), "{err}");
    assert_eq!(r.git(&r.main, &["rev-parse", "f/a"]), before);
}

// --- context economy -----------------------------------------------------------------

#[test]
fn archive_moves_closed_tasks_and_keeps_every_guarantee() {
    let r = Repo::new("archive");
    r.ok(&r.main, &["add", "first"]);
    r.ok(&r.main, &["wt", "new", "a/x"]);
    let wt = r.wt("a/x");
    r.commit_in(&wt, "f", "1\n");
    r.ok(&wt, &["submit", "1"]);
    r.ok(&r.main, &["accept", "1"]);
    r.ok(&r.main, &["add", "second", "needs:#1"]);
    r.ok(&r.main, &["archive"]);
    // One commit, both files.
    assert_eq!(
        r.git(&r.main, &["show", "--name-only", "--format=", "HEAD"]),
        "DONE.md\nTASKS.md"
    );
    assert_eq!(r.git(&r.main, &["status", "--porcelain"]), "");
    assert!(!r.tasks().contains("#1 first"));
    let done = std::fs::read_to_string(r.main.join("DONE.md")).unwrap();
    assert!(
        done.contains("- [x] #1 first") && done.contains("reviewed:"),
        "{done}"
    );
    // Blockers still satisfied, ids never reused, show still finds it.
    assert!(r.ok(&r.main, &["ready"]).contains("#2"));
    r.ok(&r.main, &["add", "third"]);
    assert!(r.tasks().contains("#3 third"));
    assert!(r.ok(&r.main, &["show", "1"]).contains("archived"));
    // Ship's gate reads the archive: the accepted, archived row still authorises.
    let out = r.ok(&r.main, &["ship", "a/x", "--sync"]);
    assert!(out.contains("authorised by #1"), "{out}");
    r.ok(&r.main, &["doctor"]);
    r.lint_history();
}

#[test]
fn over_long_text_becomes_title_and_body() {
    let r = Repo::new("titles");
    let long = "Build the thing on the site so that every reader sees it. Then explain in detail why the thing matters, which takes a lot of words that do not belong on one line of a queue.";
    let out = r.ok(&r.main, &["add", long]);
    assert!(out.contains("went to the body"));
    let t = r.tasks();
    assert!(
        t.contains(
            "- [ ] #1 Build the thing on the site so that every reader sees it.\n  Then explain"
        ),
        "{t}"
    );
    // An old-style long row is split by `split`, fields kept.
    let t = t.replace(
        "- [ ] #1 Build",
        &format!("- [ ] #2 {long} @x !2\n- [ ] #1 Build"),
    );
    std::fs::write(r.main.join("TASKS.md"), t).unwrap();
    // The hint spells the tool as the repository does.
    let cfg = std::fs::read_to_string(r.main.join(".5w.toml")).unwrap()
        + "\n[commands]\ntasks = \"tasks\"\n";
    std::fs::write(r.main.join(".5w.toml"), cfg).unwrap();
    r.git(&r.main, &["commit", "-qam", "old row"]);
    // Until split, a listed row says how much it cut and where the rest is.
    let out = r.ok(&r.main, &["ready"]);
    let cut = long.chars().count() - 120;
    assert!(
        out.contains(&format!("…(+{cut} chars: tasks show 2)")),
        "{out}"
    );
    r.ok(&r.main, &["split"]);
    let t = r.tasks();
    assert!(t.contains("- [ ] #2 Build the thing on the site so that every reader sees it. @x !2\n  Then explain"), "{t}");
    r.ok(&r.main, &["doctor"]);
}

#[test]
fn compact_json_ids_limit_and_next() {
    let r = Repo::new("compact");
    r.ok(&r.main, &["add", "hard one", "level:3", "area:core"]);
    r.ok(
        &r.main,
        &["add", "easy one", "level:1", "--body", "the details"],
    );
    r.ok(&r.main, &["add", "a decision", "lane:owner"]);
    let out = r.ok(&r.main, &["ready"]);
    assert_eq!(
        out,
        "#2 !1 easy one +\n#1 !3 @core hard one\n2 ready · 1 >owner · 0 blocked → 5w delegate 2\n"
    );
    assert_eq!(r.ok(&r.main, &["ready", "--ids", "--limit", "1"]), "2\n");
    let hint = r.ok(&r.main, &["ready", "level:3"]);
    assert!(hint.ends_with("→ 5w delegate 1\n"), "{hint}");
    let none = r.ok(&r.main, &["ready", "area:nowhere"]);
    assert!(!none.contains('→'), "{none}");
    // A list's JSON row carries what the text row shows; --full adds the rest.
    let json = r.ok(&r.main, &["ready", "--json"]);
    assert_eq!(
        json.lines().next(),
        Some(
            "{\"id\":2,\"state\":\"open\",\"level\":1,\"area\":null,\"title\":\"easy one\",\"branch\":null,\"unmet\":[],\"rework\":null}"
        ),
        "{json}"
    );
    for cmd in ["ready", "ls", "all", "blocked"] {
        let rows = r.ok(&r.main, &[cmd, "--json"]);
        assert!(!rows.contains("\"needs\""), "{cmd}: {rows}");
    }
    let full = r.ok(&r.main, &["ready", "--json", "--full"]);
    assert!(
        full.starts_with("{\"id\":2,\"state\":\"open\",\"level\":1,\"area\":null,\"lane\":")
            && full.contains("\"kind\":")
            && full.contains("\"needs\":[],\"unmet\":[],\"rework\":null}"),
        "{full}"
    );
    // A blocked row names what blocks it.
    r.ok(&r.main, &["add", "waits", "needs:#1"]);
    assert_eq!(
        r.ok(&r.main, &["blocked", "--json"]),
        "{\"id\":4,\"state\":\"open\",\"level\":null,\"area\":null,\"title\":\"waits\",\"branch\":null,\"unmet\":[1],\"rework\":null}\n"
    );
    // next and show print one task: always every field, body included.
    assert!(
        r.ok(&r.main, &["next", "--json"])
            .contains("\"body\":[\"the details\"]")
    );
    let next = r.ok(&r.main, &["next"]);
    assert!(
        next.contains("#2") && next.contains("the details") && next.contains("delegate 2"),
        "{next}"
    );
    assert!(
        r.ok(&r.main, &["show", "1", "--json"])
            .contains("\"body\":[]")
    );
}

#[test]
fn a_closed_pipe_ends_quietly() {
    let r = Repo::new("pipe");
    for i in 0..30 {
        r.ok(&r.main, &["add", &format!("task {i}")]);
    }
    let mut c = Command::new(bin5w());
    c.args(["ls"])
        .current_dir(&r.main)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    env(&mut c, &r.root);
    let mut child = c.spawn().unwrap();
    drop(child.stdout.take()); // the reader is gone, as after `| head` exits
    let out = child.wait_with_output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!err.contains("panicked"), "{err}");
}

#[test]
fn briefs_point_at_sections_and_carry_the_steps() {
    let r = Repo::new("brief");
    r.ok(
        &r.main,
        &[
            "add",
            "fix the parser",
            "--body",
            "Evidence in client/FINDINGS.md #779 section 4 and PLAN.md. See faces/SCHEMA.md sect4 and faces/FINDINGS.md #483 #509 #521.",
        ],
    );
    let b = r.ok(&r.main, &["delegate", "1"]);
    assert!(
        b.contains("refs: client/FINDINGS.md #779, PLAN.md, faces/SCHEMA.md sect4, faces/FINDINGS.md #483 #509 #521"),
        "{b}"
    );
    assert!(
        b.contains("5w wt new work/task-1") && b.contains("5w submit 1 work/task-1"),
        "{b}"
    );
    assert!(b.lines().count() < 15, "{b}");
}

// --- the protocol, without the tool ---------------------------------------------------

fn hand_edit(r: &Repo, from: &str, to: &str) {
    let t = r.tasks();
    assert!(t.contains(from), "{from:?} not in:\n{t}");
    std::fs::write(r.main.join("TASKS.md"), t.replace(from, to)).unwrap();
    r.git(&r.main, &["add", "TASKS.md"]);
}

#[test]
fn lint_passes_a_correct_hand_edit_and_names_each_violation() {
    let r = Repo::new("lint");
    r.ok(&r.main, &["add", "agent work"]);
    r.ok(&r.main, &["add", "a call", "lane:owner"]);
    r.git(&r.main, &["branch", "a/x"]);
    let sha = r.git(&r.main, &["rev-parse", "a/x"]);

    // A correct hand submit passes.
    hand_edit(
        &r,
        "- [ ] #1 agent work",
        &format!("- [~] #1 agent work branch:a/x submitted:{sha}"),
    );
    r.ok(&r.main, &["lint"]);
    r.git(&r.main, &["commit", "-qm", "chore(tasks): submit #1"]);

    let cases: &[(&str, String, &str)] = &[
        (
            "- [ ] #2 a call >owner",
            "- [x] #2 a call >owner via:self".into(),
            "closes via:decided",
        ),
        ("- [ ] #2 a call >owner\n", "".into(), "deleted"),
        (
            &format!("- [~] #1 agent work branch:a/x submitted:{sha}"),
            "- [x] #1 agent work branch:a/x via:review".into(),
            "without reviewed",
        ),
        (
            &format!("- [~] #1 agent work branch:a/x submitted:{sha}"),
            "- [ ] #1 agent work branch:a/x".into(),
            "without rework",
        ),
        (
            "- [ ] #2 a call >owner",
            "- [ ] #2 a call >owner\n- [ ] #1 reused id".into(),
            "twice",
        ),
        (
            "- [ ] #2 a call >owner",
            "- [~] #2 a call >owner".into(),
            "without branch",
        ),
        // A sha written by hand is the full name; under 7 digits is none at all.
        (
            &format!("submitted:{sha}"),
            format!("submitted:{}", &sha[..12]),
            "is not a full sha",
        ),
        (
            &format!("- [~] #1 agent work branch:a/x submitted:{sha}"),
            format!(
                "- [x] #1 agent work branch:a/x via:review reviewed:{}",
                &sha[..12]
            ),
            "is not a full sha",
        ),
        (
            &format!("submitted:{sha}"),
            format!("submitted:{}", &sha[..6]),
            "submitted without submitted:<sha>",
        ),
    ];
    for (from, to, want) in cases {
        let before = r.tasks();
        hand_edit(&r, from, to);
        let out = r.fails(&r.main, &["lint"]);
        assert!(out.contains(want), "want {want:?} in:\n{out}");
        std::fs::write(r.main.join("TASKS.md"), before).unwrap();
        r.git(&r.main, &["add", "TASKS.md"]);
    }

    // A prefix the tool recorded (releases through 0.1.3 wrote 12 digits) passes
    // in its submit or accept commit, and stays valid while the row keeps it.
    // Any other length there is a hand edit.
    hand_edit(
        &r,
        &format!("submitted:{sha}"),
        &format!("submitted:{}", &sha[..8]),
    );
    r.git(
        &r.main,
        &["commit", "-qm", "chore(tasks): submit #1 for review"],
    );
    assert!(
        r.fails(&r.main, &["lint", "HEAD"])
            .contains("is not a full sha")
    );
    r.git(&r.main, &["reset", "-q", "--hard", "HEAD~1"]);
    let short = format!("submitted:{}", &sha[..12]);
    hand_edit(&r, &format!("submitted:{sha}"), &short);
    r.git(&r.main, &["commit", "-qm", "chore(tasks): shorten #1"]);
    assert!(
        r.fails(&r.main, &["lint", "HEAD"])
            .contains("is not a full sha")
    );
    r.git(
        &r.main,
        &[
            "commit",
            "-q",
            "--amend",
            "-m",
            "chore(tasks): submit #1 for review",
        ],
    );
    r.ok(&r.main, &["lint", "HEAD"]);
    hand_edit(&r, "- [~] #1 agent work", "- [~] #1 agent work again");
    r.ok(&r.main, &["lint"]);
    r.git(&r.main, &["reset", "-q", "--hard"]);

    // Shape: a queue edit mixed with code, and one made on a branch.
    hand_edit(&r, "- [ ] #2 a call", "- [ ] #2 a better call");
    std::fs::write(r.main.join("code.rs"), "x\n").unwrap();
    r.git(&r.main, &["add", "code.rs"]);
    assert!(r.fails(&r.main, &["lint"]).contains("its own commit"));
    r.git(&r.main, &["reset", "-q", "code.rs"]);
    r.ok(&r.main, &["lint"]);
    r.git(&r.main, &["reset", "-q", "--hard"]);
    r.git(&r.main, &["checkout", "-q", "a/x"]);
    hand_edit(&r, "- [ ] #2 a call", "- [ ] #2 a better call");
    assert!(
        r.fails(&r.main, &["lint"])
            .contains("queue edits go on main")
    );
}

#[test]
fn lint_flags_rework_gained_outside_a_reject() {
    let r = Repo::new("lint-rework");
    r.ok(&r.main, &["add", "agent work"]);
    r.ok(&r.main, &["add", "other"]);
    r.ok(&r.main, &["done", "2", "--self"]);

    // An open row that gains a reason was never rejected.
    hand_edit(
        &r,
        "- [ ] #1 agent work",
        "- [ ] #1 agent work rework:\"bad\"",
    );
    let out = r.fails(&r.main, &["lint"]);
    assert!(out.contains("gained rework:"), "{out}");
    r.git(&r.main, &["reset", "-q", "--hard"]);

    // Nor was a closed row reopened with one.
    hand_edit(
        &r,
        "- [x] #2 other via:self",
        "- [ ] #2 other rework:\"bad\"",
    );
    let out = r.fails(&r.main, &["lint"]);
    assert!(out.contains("gained rework:"), "{out}");
    r.git(&r.main, &["reset", "-q", "--hard"]);

    // A real reject passes, and so does fixing its reason afterwards.
    r.ok(&r.main, &["wt", "new", "a/x"]);
    let wt = r.wt("a/x");
    r.commit_in(&wt, "f", "1\n");
    r.ok(&wt, &["submit", "1"]);
    r.ok(&r.main, &["reject", "1", "bad"]);
    r.ok(&r.main, &["lint", "HEAD"]);
    hand_edit(&r, "rework:\"bad\"", "rework:\"worse\"");
    r.ok(&r.main, &["lint"]);
    r.git(&r.main, &["reset", "-q", "--hard"]);

    // Released versions through 0.1.3 let `5w reject` send back an open task:
    // that commit is the tool's, and a range lint of old history passes it.
    r.ok(&r.main, &["add", "third"]);
    hand_edit(&r, "- [ ] #3 third", "- [ ] #3 third rework:\"old\"");
    r.git(&r.main, &["commit", "-qm", "chore(tasks): reject #3"]);
    r.lint_history();
    // So does a batch commit whose subject names that reject among its edits.
    r.ok(&r.main, &["add", "fifth"]);
    let batch_edit = || {
        hand_edit(
            &r,
            "- [ ] #4 fifth",
            "- [ ] #4 fifth rework:\"old\"\n- [ ] #5 sixth",
        )
    };
    batch_edit();
    r.git(
        &r.main,
        &["commit", "-qm", "chore(tasks): add #5, reject #4"],
    );
    r.lint_history();
    // A queue subject names exactly the rows its commit changes, each once:
    // a batch's list, and a single edit's one row.
    for subject in [
        "chore(tasks): add #9, reject #4",
        "chore(tasks): add #5, reject #4, accept #1",
        "chore(tasks): add #5, reject #4, reject #4",
        "chore(tasks): reject #4",
        "chore(tasks): add #5 — sixth",
        "chore(tasks): set #100 level 1",
    ] {
        r.git(&r.main, &["reset", "-q", "--hard", "HEAD~1"]);
        batch_edit();
        r.git(&r.main, &["commit", "-qm", subject]);
        let out = r.fails(&r.main, &["lint", "HEAD"]);
        assert!(
            out.contains("its subject names") && out.contains("but it changes #4 #5"),
            "{subject}: {out}"
        );
    }
    r.git(&r.main, &["reset", "-q", "--hard", "HEAD~2"]);
    // The same edit under any other message is a hand edit, and a range names it.
    r.ok(&r.main, &["add", "fourth"]);
    hand_edit(&r, "- [ ] #4 fourth", "- [ ] #4 fourth rework:\"old\"");
    r.git(&r.main, &["commit", "-qm", "chore(tasks): note #4"]);
    let out = r.fails(&r.main, &["lint", "HEAD"]);
    assert!(out.contains("#4: gained rework:"), "{out}");
    r.git(&r.main, &["reset", "-q", "--hard", "HEAD~1"]);

    // A new row cannot arrive with a reason either.
    hand_edit(
        &r,
        "- [ ] #4 fourth",
        "- [ ] #4 fourth\n- [ ] #5 new rework:\"x\"",
    );
    let out = r.fails(&r.main, &["lint"]);
    assert!(out.contains("#5: a new row carries rework:"), "{out}");
}

#[test]
fn lint_of_a_commit_or_range_flags_an_id_in_both_files_even_from_a_merge() {
    let r = Repo::new("lint-both");
    r.ok(&r.main, &["add", "first"]);
    r.ok(&r.main, &["add", "second"]);
    r.ok(&r.main, &["done", "1", "--self"]);
    let closed = r.tasks();
    r.git(&r.main, &["branch", "side"]);
    r.ok(&r.main, &["archive"]);
    r.lint_history();
    let root = r.git(&r.main, &["rev-list", "--max-parents=0", "main"]);
    let range = format!("{root}..main");

    // A commit that puts the archived row back in TASKS.md.
    std::fs::write(r.main.join("TASKS.md"), &closed).unwrap();
    r.git(&r.main, &["commit", "-qam", "restore the row"]);
    for args in [&["lint", "HEAD"][..], &["lint", &range]] {
        let out = r.fails(&r.main, args);
        assert!(out.contains("#1: in both TASKS.md and DONE.md"), "{out}");
    }
    r.git(&r.main, &["reset", "-q", "--hard", "HEAD~1"]);

    // A merge whose resolution does the same, changing neither file against
    // one parent: lint reads the tree it makes, not only what it changed.
    r.git(&r.main, &["checkout", "-q", "side"]);
    r.commit_in(&r.main, "f", "1\n");
    r.git(&r.main, &["checkout", "-q", "main"]);
    r.git(
        &r.main,
        &["merge", "-q", "--no-commit", "-s", "ours", "side"],
    );
    std::fs::write(r.main.join("TASKS.md"), &closed).unwrap();
    r.git(&r.main, &["add", "TASKS.md"]);
    r.git(&r.main, &["commit", "-qm", "merge side"]);
    for args in [&["lint", "HEAD"][..], &["lint", &range]] {
        let out = r.fails(&r.main, args);
        assert!(out.contains("#1: in both TASKS.md and DONE.md"), "{out}");
    }
}

/// A queue as the trunk holds it: #1 closed, #2 in review on `feat`, #3 open.
fn merge_base_queue(r: &Repo) -> PathBuf {
    r.ok(&r.main, &["add", "first"]);
    r.ok(&r.main, &["add", "second"]);
    r.ok(&r.main, &["add", "third"]);
    r.ok(&r.main, &["done", "1", "--self"]);
    r.ok(&r.main, &["wt", "new", "feat"]);
    let wt = r.wt("feat");
    r.commit_in(&wt, "f", "1\n");
    r.ok(&wt, &["submit", "2"]);
    wt
}

#[test]
fn lint_passes_ordinary_merges_that_carry_a_parent_queue() {
    let r = Repo::new("lint-merges-clean");
    let wt = merge_base_queue(&r);
    // The trunk moves on; the feature branch merges it in, then more work.
    r.ok(&r.main, &["add", "fourth"]);
    r.git(&wt, &["merge", "-q", "--no-edit", "main"]);
    r.ok(&wt, &["lint", "HEAD"]);
    r.commit_in(&wt, "f", "2\n");
    r.ok(&r.main, &["add", "fifth"]);
    r.ok(&r.main, &["archive"]);
    // Rows the trunk archived move for the feature too.
    r.git(&wt, &["merge", "-q", "--no-edit", "main"]);
    r.ok(&wt, &["lint", "HEAD"]);
    // Two more branches, one cut before the archive.
    r.git(&r.main, &["branch", "a", "main~1"]);
    r.git(&r.main, &["branch", "b", "main"]);
    for (b, f) in [("a", "a"), ("b", "b")] {
        r.git(&r.main, &["checkout", "-q", b]);
        r.commit_in(&r.main, f, "x\n");
    }
    r.git(&r.main, &["checkout", "-q", "main"]);
    r.git(&r.main, &["merge", "-q", "--no-ff", "--no-edit", "feat"]);
    r.ok(&r.main, &["lint", "HEAD"]);
    r.git(&r.main, &["merge", "-q", "--no-ff", "--no-edit", "a", "b"]);
    assert!(r.git(&r.main, &["rev-parse", "--verify", "HEAD^3"]).len() >= 40);
    r.ok(&r.main, &["lint", "HEAD"]);
    r.ok(&r.main, &["add", "sixth"]);
    r.lint_history();
}

#[test]
fn lint_judges_a_merge_queue_against_its_parents() {
    let r = Repo::new("lint-merge-rows");
    merge_base_queue(&r);
    r.git(&r.main, &["branch", "side", "main~2"]);
    r.git(&r.main, &["checkout", "-q", "side"]);
    r.commit_in(&r.main, "s", "1\n");
    r.git(&r.main, &["checkout", "-q", "main"]);
    r.lint_history();
    let root = r.git(&r.main, &["rev-list", "--max-parents=0", "main"]);
    let range = format!("{root}..main");
    let merge = |tasks: &str| {
        r.git(
            &r.main,
            &["merge", "-q", "--no-commit", "-s", "ours", "side"],
        );
        std::fs::write(r.main.join("TASKS.md"), tasks).unwrap();
        r.git(&r.main, &["add", "TASKS.md"]);
        r.git(&r.main, &["commit", "-qm", "merge side", "--no-verify"]);
    };
    let tasks = r.tasks();

    // A resolution that drops a row the trunk has.
    let dropped: String = tasks
        .lines()
        .filter(|l| !l.contains("#3 third"))
        .map(|l| format!("{l}\n"))
        .collect();
    merge(&dropped);
    for args in [&["lint", "HEAD"][..], &["lint", &range]] {
        let out = r.fails(&r.main, args);
        assert!(out.contains("#3: deleted"), "{out}");
    }
    r.git(&r.main, &["reset", "-q", "--hard", "HEAD~1"]);

    // One that takes the side's older rows is judged as the trunk sees it.
    let side = r.git(&r.main, &["show", "side:TASKS.md"]);
    merge(&format!("{side}\n"));
    for args in [&["lint", "HEAD"][..], &["lint", &range]] {
        let out = r.fails(&r.main, args);
        assert!(out.contains("#2: rejected without rework"), "{out}");
    }
    r.git(&r.main, &["reset", "-q", "--hard", "HEAD~1"]);

    // A row edited as no parent had it is a queue edit inside a merge.
    merge(&tasks.replace("#3 third", "#3 third, retitled"));
    for args in [&["lint", "HEAD"][..], &["lint", &range]] {
        let out = r.fails(&r.main, args);
        assert!(out.contains("a merge changes #3 against"), "{out}");
    }
    r.git(&r.main, &["reset", "-q", "--hard", "HEAD~1"]);

    // The trunk's queue, unchanged, passes.
    merge(&tasks);
    r.lint_history();
}

/// Run git where it may stop on a conflict; true when it succeeded.
fn git_may_conflict(r: &Repo, cwd: &Path, args: &[&str]) -> bool {
    let mut c = Command::new("git");
    c.args(args).current_dir(cwd);
    env(&mut c, &r.root);
    c.output().unwrap().status.success()
}

/// A clone of the trunk that edits the queue on its own, as a second machine does.
fn clone_of(r: &Repo) -> PathBuf {
    let clone = r.root.join("clone");
    r.git(&r.root, &["clone", "-q", r.main.to_str().unwrap(), "clone"]);
    clone
}

#[test]
fn lint_passes_a_clean_merge_in_a_criss_cross_history() {
    let r = Repo::new("lint-criss-cross");
    r.ok(&r.main, &["add", "first"]);
    r.ok(&r.main, &["add", "second"]);
    r.ok(&r.main, &["wt", "new", "feat"]);
    let wt = r.wt("feat");
    r.commit_in(&wt, "f", "1\n");
    let f1 = r.git(&wt, &["rev-parse", "HEAD"]);
    r.ok(&r.main, &["add", "third"]);
    r.ok(&r.main, &["add", "fourth"]);
    // Each side merges the other as it was: two merge bases.
    r.git(&wt, &["merge", "-q", "--no-edit", "main"]);
    r.git(&r.main, &["merge", "-q", "--no-ff", "--no-edit", &f1]);
    r.ok(&r.main, &["set", "4", "level", "2"]);
    r.commit_in(&wt, "f", "2\n");
    let bases = r.git(&wt, &["merge-base", "--all", "HEAD", "main"]);
    assert_eq!(bases.lines().count(), 2, "{bases}");
    r.git(&wt, &["merge", "-q", "--no-edit", "main"]);
    r.ok(&wt, &["lint", "HEAD"]);
    let root = r.git(&r.main, &["rev-list", "--max-parents=0", "main"]);
    r.ok(&wt, &["lint", &format!("{root}..HEAD")]);
    r.git(&r.main, &["merge", "-q", "--no-ff", "--no-edit", "feat"]);
    r.lint_history();
}

#[test]
fn lint_passes_a_resolved_conflict_on_a_row_both_clones_edited() {
    let r = Repo::new("lint-merge-conflict");
    r.ok(&r.main, &["hook", "install"]);
    r.ok(&r.main, &["add", "first"]);
    r.ok(&r.main, &["add", "second"]);
    r.ok(&r.main, &["add", "third"]);
    let clone = clone_of(&r);
    r.ok(&clone, &["set", "3", "area", "ui"]);
    r.ok(&r.main, &["set", "3", "level", "1"]);
    let theirs = std::fs::read_to_string(clone.join("TASKS.md")).unwrap();
    let ours = r.tasks();
    let root = r.git(&r.main, &["rev-list", "--max-parents=0", "main"]);
    let range = format!("{root}..main");
    let pull = || {
        let pulled = git_may_conflict(
            &r,
            &r.main,
            &["pull", "-q", "--no-rebase", "--no-edit", "../clone", "main"],
        );
        assert!(!pulled, "the same row edited on both sides conflicts");
    };
    let both = ours
        .lines()
        .map(|l| match l.contains("#3 third") {
            true => format!("{l} @ui\n"),
            false => format!("{l}\n"),
        })
        .collect::<String>();
    // A resolution that keeps both edits passes the pre-commit hook, and lint
    // of the commit agrees.
    pull();
    std::fs::write(r.main.join("TASKS.md"), &both).unwrap();
    r.git(&r.main, &["add", "TASKS.md"]);
    r.ok(&r.main, &["lint", "--staged"]);
    r.git(&r.main, &["commit", "-q", "--no-edit"]);
    r.ok(&r.main, &["lint", "HEAD"]);
    r.ok(&r.main, &["lint", &range]);
    r.git(&r.main, &["reset", "-q", "--hard", "HEAD~1"]);

    // Taking their row whole discards the trunk's level: refused at commit
    // time as in a range.
    pull();
    std::fs::write(r.main.join("TASKS.md"), &theirs).unwrap();
    r.git(&r.main, &["add", "TASKS.md"]);
    let out = r.fails(&r.main, &["lint", "--staged"]);
    assert!(out.contains("a merge changes #3 against"), "{out}");
    r.git(&r.main, &["commit", "-q", "--no-edit", "--no-verify"]);
    let out = r.fails(&r.main, &["lint", "HEAD"]);
    assert!(out.contains("a merge changes #3 against"), "{out}");
    r.git(&r.main, &["reset", "-q", "--hard", "HEAD~1"]);

    // A resolution that also retitles a row neither side changed is a queue edit
    // inside a merge, at commit time as in a range.
    pull();
    std::fs::write(
        r.main.join("TASKS.md"),
        both.replace("#1 first", "#1 retitled"),
    )
    .unwrap();
    r.git(&r.main, &["add", "TASKS.md"]);
    let out = r.fails(&r.main, &["lint", "--staged"]);
    assert!(out.contains("a merge changes #1 against"), "{out}");
    r.git(&r.main, &["commit", "-q", "--no-edit", "--no-verify"]);
    let out = r.fails(&r.main, &["lint", "HEAD"]);
    assert!(out.contains("a merge changes #1 against"), "{out}");
}

/// Five rows, #4 submitted on `feat`, then a clone; `trunk` and `side` edit
/// the queue apart. Returns the clone.
fn apart(r: &Repo, trunk: &[&str], side: &[&str]) -> PathBuf {
    for t in ["first", "second", "third", "fourth", "fifth"] {
        r.ok(&r.main, &["add", t]);
    }
    r.ok(&r.main, &["wt", "new", "feat"]);
    let wt = r.wt("feat");
    r.commit_in(&wt, "f", "1\n");
    r.ok(&wt, &["submit", "4"]);
    let clone = clone_of(r);
    r.ok(&clone, side);
    r.ok(&r.main, trunk);
    clone
}

/// Pull the clone into the trunk and commit `tasks` as the merge's queue.
fn merge_clone_as(r: &Repo, tasks: &str) {
    git_may_conflict(
        r,
        &r.main,
        &[
            "pull",
            "-q",
            "--no-rebase",
            "--no-commit",
            "../clone",
            "main",
        ],
    );
    std::fs::write(r.main.join("TASKS.md"), tasks).unwrap();
    r.git(&r.main, &["add", "TASKS.md"]);
    r.git(&r.main, &["commit", "-q", "--no-edit", "--no-verify"]);
    r.git(&r.main, &["rev-parse", "--verify", "HEAD^2"]);
}

/// `tasks` with row `id`'s line replaced by `line`, where it stood.
fn with_row(tasks: &str, id: u64, line: &str) -> String {
    tasks
        .lines()
        .map(|l| match l.contains(&format!("] #{id} ")) {
            true => format!("{line}\n"),
            false => format!("{l}\n"),
        })
        .collect()
}

/// Row `id`'s line in a queue file.
fn row_of(tasks: &str, id: u64) -> String {
    tasks
        .lines()
        .find(|l| l.contains(&format!("] #{id} ")))
        .unwrap()
        .to_string()
}

/// A merge's queue made from the trunk's and the side's files.
type Resolve = fn(&str, &str) -> String;

#[test]
fn lint_refuses_a_merge_that_undoes_or_breaks_the_trunk_edit_of_a_row_both_sides_changed() {
    let theirs_whole: Resolve = |_, theirs| theirs.to_string();
    let close = &["done", "5", "--self"][..];
    let cases: [(&[&str], &[&str], Resolve); 5] = [
        // The side's row whole reverts the close.
        (close, &["set", "5", "level", "1"], theirs_whole),
        // The side's open state with the trunk's via.
        (close, &["set", "5", "level", "1"], |_, theirs| {
            with_row(theirs, 5, &format!("{} via:self", row_of(theirs, 5)))
        }),
        // The trunk's close with the side's level: a closed row changed.
        (close, &["set", "5", "level", "1"], |ours, _| {
            with_row(ours, 5, &format!("{} !1", row_of(ours, 5)))
        }),
        // The side's row undoes the accept.
        (&["accept", "4"], &["set", "4", "level", "1"], theirs_whole),
        // The side's row strips the reject's rework:.
        (
            &["reject", "4", "bad"],
            &["set", "4", "level", "1"],
            theirs_whole,
        ),
    ];
    for (i, (trunk, side, resolve)) in cases.into_iter().enumerate() {
        let r = Repo::new(&format!("lint-merge-undo-{i}"));
        let clone = apart(&r, trunk, side);
        let base = r.git(&r.main, &["rev-parse", "main~1"]);
        let theirs = std::fs::read_to_string(clone.join("TASKS.md")).unwrap();
        merge_clone_as(&r, &resolve(&r.tasks(), &theirs));
        let id = format!("#{}", trunk[1]);
        for args in [&["lint", "HEAD"][..], &["lint", &format!("{base}..main")]] {
            let out = r.fails(&r.main, args);
            assert!(out.contains(&id), "case {i}: {out}");
        }
    }
}

#[test]
fn lint_passes_a_merge_that_keeps_a_close_over_an_edit_made_while_open() {
    let r = Repo::new("lint-merge-close-wins");
    let clone = apart(&r, &["set", "5", "level", "1"], &["done", "5", "--self"]);
    let base = r.git(&r.main, &["rev-parse", "main~1"]);
    let theirs = std::fs::read_to_string(clone.join("TASKS.md")).unwrap();
    merge_clone_as(&r, &theirs);
    r.ok(&r.main, &["lint", "HEAD"]);
    r.ok(&r.main, &["lint", &format!("{base}..main")]);
    r.git(&r.main, &["reset", "-q", "--hard", "HEAD~1"]);
    // Keeping the level as well changes the closed row.
    let closed = row_of(&theirs, 5);
    merge_clone_as(&r, &with_row(&theirs, 5, &format!("{closed} !1")));
    let out = r.fails(&r.main, &["lint", "HEAD"]);
    assert!(
        out.contains(
            "#5: a closed row changed — reopen it first (take the closed side's row whole)"
        ),
        "{out}"
    );
}

#[test]
fn a_range_names_a_side_hand_edit_at_its_commit_not_at_the_merge_that_brings_it() {
    let r = Repo::new("lint-merge-side-edit");
    let clone = apart(&r, &["set", "3", "level", "1"], &["done", "1", "--self"]);
    let tasks = std::fs::read_to_string(clone.join("TASKS.md")).unwrap();
    std::fs::write(
        clone.join("TASKS.md"),
        tasks.replace("#1 first", "#1 first, rewritten"),
    )
    .unwrap();
    r.git(&clone, &["commit", "-qam", "tidy", "--no-verify"]);
    let bad = r.git(&clone, &["rev-parse", "--short=12", "HEAD"]);
    let base = r.git(&r.main, &["rev-parse", "main~1"]);
    assert!(git_may_conflict(
        &r,
        &r.main,
        &["pull", "-q", "--no-rebase", "--no-edit", "../clone", "main"]
    ));
    let merge = r.git(&r.main, &["rev-parse", "--short=12", "HEAD"]);
    let out = r.fails(&r.main, &["lint", &format!("{base}..main")]);
    assert!(
        out.contains(&format!("{bad} #1: a closed row changed")),
        "{out}"
    );
    assert!(!out.contains(&merge), "{out}");
    // The merge alone trusts its side: a side edit outside the range goes unseen.
    r.ok(&r.main, &["lint", "HEAD"]);
}

/// A trunk commit made with --no-verify that retitles closed #1; returns its
/// short sha and its parent.
fn bad_trunk_edit(r: &Repo, cwd: &Path) -> (String, String) {
    let tasks = std::fs::read_to_string(cwd.join("TASKS.md")).unwrap();
    std::fs::write(
        cwd.join("TASKS.md"),
        tasks.replace("#1 first", "#1 first, retitled"),
    )
    .unwrap();
    r.git(cwd, &["commit", "-qam", "retitle", "--no-verify"]);
    (
        r.git(cwd, &["rev-parse", "--short=12", "HEAD"]),
        r.git(cwd, &["rev-parse", "HEAD~1"]),
    )
}

/// `lint <before>..main` and `ci` on main name the bad commit, never a merge.
fn only_bad_is_named(r: &Repo, cwd: &Path, bad: &str, before: &str) {
    let merges = r.git(
        cwd,
        &[
            "rev-list",
            "--merges",
            "--abbrev=12",
            "--abbrev-commit",
            &format!("{before}..main"),
        ],
    );
    assert!(!merges.is_empty());
    for args in [
        &["lint", &format!("{before}..main")][..],
        &[
            "ci",
            "--ref",
            "refs/heads/main",
            "--base",
            before,
            "--head",
            "main",
        ],
    ] {
        let out = r.fails(cwd, args);
        assert!(
            out.contains(&format!("{bad} #1: a closed row changed")),
            "{args:?}: {out}"
        );
        for m in merges.lines() {
            assert!(!out.contains(m), "{args:?}: {out}");
        }
    }
}

#[test]
fn a_trunk_hand_edit_is_named_once_after_a_pull_merge() {
    let r = Repo::new("lint-merge-pull-bad");
    r.ok(&r.main, &["add", "first"]);
    r.ok(&r.main, &["add", "second"]);
    r.ok(&r.main, &["done", "1", "--self"]);
    let clone = clone_of(&r);
    let (bad, before) = bad_trunk_edit(&r, &r.main);
    // The clone commits locally, then pulls: its own commit is the first parent.
    r.ok(&clone, &["add", "third"]);
    r.git(
        &clone,
        &["pull", "-q", "--no-rebase", "--no-edit", "origin", "main"],
    );
    r.git(&clone, &["rev-parse", "--verify", "HEAD^2"]);
    // And pushes: the remote trunk is the merge, as ci's checkout sees it.
    r.git(&clone, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    only_bad_is_named(&r, &clone, &bad, &before);
}

#[test]
fn a_trunk_hand_edit_is_named_once_after_a_fast_forward_landing() {
    let r = Repo::new("lint-merge-ff-bad");
    r.ok(&r.main, &["add", "first"]);
    r.ok(&r.main, &["add", "second"]);
    r.ok(&r.main, &["done", "1", "--self"]);
    r.ok(&r.main, &["wt", "new", "feat"]);
    let wt = r.wt("feat");
    r.commit_in(&wt, "f", "1\n");
    let (bad, before) = bad_trunk_edit(&r, &r.main);
    r.git(&wt, &["merge", "-q", "--no-edit", "main"]);
    r.git(&r.main, &["merge", "-q", "--ff-only", "feat"]);
    only_bad_is_named(&r, &r.main, &bad, &before);
}

#[test]
fn lint_does_not_blame_a_merge_for_a_trunk_commit_it_brings_in() {
    let r = Repo::new("lint-merge-trunk-finding");
    r.ok(&r.main, &["add", "first"]);
    r.ok(&r.main, &["add", "second"]);
    r.ok(&r.main, &["done", "1", "--self"]);
    r.ok(&r.main, &["wt", "new", "feat"]);
    let wt = r.wt("feat");
    r.commit_in(&wt, "f", "1\n");
    // A hand edit lands on the trunk, flagged there, and stays in history.
    let tasks = r.tasks().replace("#1 first", "#1 first, retitled");
    std::fs::write(r.main.join("TASKS.md"), tasks).unwrap();
    r.git(&r.main, &["commit", "-qam", "retitle", "--no-verify"]);
    r.fails(&r.main, &["lint", "HEAD"]);
    let before = r.git(&r.main, &["rev-parse", "main"]);

    r.git(&wt, &["merge", "-q", "--no-edit", "main"]);
    r.ok(&wt, &["lint", "HEAD"]);
    r.ok(&r.main, &["add", "third"]);
    let before_landing = r.git(&r.main, &["rev-parse", "main"]);
    r.git(&r.main, &["merge", "-q", "--no-ff", "--no-edit", "feat"]);
    r.ok(&r.main, &["lint", "HEAD"]);
    r.ok(&r.main, &["lint", &format!("{before}..main")]);
    r.ok(
        &r.main,
        &[
            "ci",
            "--ref",
            "refs/heads/main",
            "--base",
            &before_landing,
            "--head",
            "main",
        ],
    );
}

/// Two clones that each add a row give it the same id; the merge renumbers one
/// past every parent's highest id — a new row, as an add is. Renumbering into
/// an id either side holds is not.
#[test]
fn lint_passes_a_merge_that_renumbers_a_colliding_new_row() {
    let r = Repo::new("lint-merge-renumber");
    r.ok(&r.main, &["add", "first"]);
    r.ok(&r.main, &["add", "second"]);
    let clone = clone_of(&r);
    r.ok(&clone, &["add", "from the clone"]);
    r.ok(&r.main, &["add", "from main"]);
    let ours = r.tasks();
    let row = |id: u64| {
        ours.lines()
            .find(|l| l.contains("from main"))
            .unwrap()
            .replace("#3 from main", &format!("#{id} from the clone"))
    };
    let root = r.git(&r.main, &["rev-list", "--max-parents=0", "main"]);
    let range = format!("{root}..main");
    let merge = |tasks: String| {
        assert!(!git_may_conflict(
            &r,
            &r.main,
            &["pull", "-q", "--no-rebase", "--no-edit", "../clone", "main"]
        ));
        std::fs::write(r.main.join("TASKS.md"), tasks).unwrap();
        r.git(&r.main, &["add", "TASKS.md"]);
        r.git(&r.main, &["commit", "-q", "--no-edit", "--no-verify"]);
    };
    let with = |line: String| ours.replace("#3 from main", &format!("#3 from main\n{line}"));

    merge(with(row(4)));
    r.ok(&r.main, &["lint", "HEAD"]);
    r.ok(&r.main, &["lint", &range]);
    r.git(&r.main, &["reset", "-q", "--hard", "HEAD~1"]);

    merge(with(row(2)));
    let out = r.fails(&r.main, &["lint", "HEAD"]);
    assert!(out.contains("#2"), "{out}");
}
#[test]
fn a_single_edit_subject_may_rewrite_its_own_row_but_changes_no_other() {
    let r = Repo::new("lint-single-subject");
    r.ok(&r.main, &["add", "first"]);
    r.ok(&r.main, &["add", "second", "area:core"]);
    r.ok(&r.main, &["set", "2", "needs", "1"]);

    hand_edit(&r, "@core needs:1", "needs:#1 @core");
    r.git(&r.main, &["commit", "-qm", "tidy the queue by hand"]);

    // `set` to the value a row has commits nothing, and says so — alone or in
    // a batch, though it would spell the line differently.
    let tip = r.git(&r.main, &["rev-parse", "main"]);
    let out = r.ok(&r.main, &["set", "2", "needs", "#1"]);
    assert!(out.contains("nothing to commit"), "{out}");
    let (ok, out) = r.batch("set 2 needs 1\n");
    assert!(ok && out.contains("nothing to commit"), "{out}");
    assert_eq!(r.git(&r.main, &["rev-parse", "main"]), tip);
    assert_eq!(r.git(&r.main, &["status", "--porcelain"]), "");
    assert!(r.line(2).contains("needs:#1 @core"));

    // Releases through 0.1.3 committed it as a rewrite of the row's line that
    // reads the same: that history lints, and audit counts it as 5w's.
    hand_edit(&r, "needs:#1 @core", "@core needs:1");
    r.git(&r.main, &["commit", "-qm", "chore(tasks): set #2 needs 1"]);
    r.lint_history();
    let out = r.ok(&r.main, &["audit"]);
    assert!(out.contains("outside 1 queue commits"), "{out}");

    // A subject naming one row on a commit that changes another is a hand edit.
    hand_edit(&r, "- [ ] #1 first", "- [ ] #1 first !1");
    r.git(
        &r.main,
        &["commit", "-qm", "chore(tasks): set #100 level 1"],
    );
    let out = r.fails(&r.main, &["lint", "HEAD"]);
    assert!(
        out.contains("its subject names #100, but it changes #1"),
        "{out}"
    );
    let out = r.ok(&r.main, &["audit"]);
    assert!(out.contains("outside 2 queue commits"), "{out}");
    assert!(
        out.contains("set #100 level 1 — its subject and the rows it changes disagree"),
        "{out}"
    );
}

#[test]
fn a_same_value_edit_still_fixes_the_checkout_and_the_rows_section() {
    let r = Repo::new("same-value-writes");
    let cfg = std::fs::read_to_string(r.main.join(".5w.toml"))
        .unwrap()
        .replace(
            "[lanes.manual]\nkind = \"manual\"",
            "[lanes.manual]\nkind = \"manual\"\nsection = \"## By hand\"",
        );
    std::fs::write(r.main.join(".5w.toml"), cfg).unwrap();
    r.git(
        &r.main,
        &["commit", "-qam", "manual rows sit under By hand"],
    );
    r.ok(&r.main, &["add", "first", "level:2"]);
    r.ok(&r.main, &["add", "by hand", "lane:manual"]);
    assert!(r.tasks().contains("## By hand\n\n- [ ] #2 by hand >manual"));

    // The committed row already reads so, but the checkout's copy does not:
    // it is rewritten, alone or in a batch, and nothing is left dirty.
    for batch in [false, true] {
        let t = r.tasks().replace("#1 first !2", "#1 first !4");
        std::fs::write(r.main.join("TASKS.md"), t).unwrap();
        let out = match batch {
            false => r.ok(&r.main, &["set", "1", "level", "2"]),
            true => r.batch("set 1 level 2\n").1,
        };
        assert!(!out.contains("nothing to commit"), "{out}");
        assert!(r.line(1).ends_with("#1 first !2"), "{}", r.tasks());
        assert_eq!(r.git(&r.main, &["status", "--porcelain"]), "");
    }

    // A row outside its lane's section reads the same, but `set` to its lane
    // moves it there, alone or in a batch, and lint passes the commit.
    let moved = "## Open\n\n- [ ] #1 first !2\n\n## By hand\n\n- [ ] #2 by hand >manual";
    let astray = "## Open\n\n- [ ] #1 first !2\n- [ ] #2 by hand >manual\n\n## By hand\n";
    for input in ["", "set 2 lane manual\nset 1 level 3\n"] {
        let t = r.tasks();
        assert!(t.contains(moved), "{t}");
        std::fs::write(r.main.join("TASKS.md"), t.replace(moved, astray)).unwrap();
        r.git(&r.main, &["commit", "-qam", "a row astray"]);
        let out = match input {
            "" => r.ok(&r.main, &["set", "2", "lane", "manual"]),
            i => r.batch(i).1,
        };
        assert!(!out.contains("nothing to commit"), "{out}");
        assert!(
            r.tasks().contains("## By hand\n\n- [ ] #2 by hand >manual"),
            "{}",
            r.tasks()
        );
        r.ok(&r.main, &["lint", "HEAD"]);
        r.ok(&r.main, &["set", "1", "level", "2"]);
    }
}

#[test]
fn a_same_value_edit_fixes_a_stale_staged_row() {
    let r = Repo::new("same-value-staged");
    r.ok(&r.main, &["add", "first", "level:2"]);
    r.ok(&r.main, &["add", "second"]);
    let head = r.git(&r.main, &["rev-parse", "HEAD"]);
    let staged = || r.git(&r.main, &["show", ":TASKS.md"]);

    // The committed row reads !2 but the index holds !4: the edit commits
    // nothing, yet the index takes it, so a plain commit cannot bring !4 back.
    for batch in [false, true] {
        let t = r.tasks().replace("#1 first !2", "#1 first !4");
        std::fs::write(r.main.join("TASKS.md"), t).unwrap();
        r.git(&r.main, &["add", "TASKS.md"]);
        let out = match batch {
            false => r.ok(&r.main, &["set", "1", "level", "2"]),
            true => r.batch("set 1 level 2\n").1,
        };
        assert!(out.contains("  checkout fixed: #1 matches main"), "{out}");
        assert!(!out.contains("committed"), "{out}");
        assert_eq!(r.git(&r.main, &["rev-parse", "HEAD"]), head);
        assert_eq!(staged(), r.git(&r.main, &["show", "HEAD:TASKS.md"]));
        assert_eq!(r.git(&r.main, &["status", "--porcelain"]), "");
    }

    // A peer's unrelated staged row stays staged; only #1 is fixed.
    let t = r
        .tasks()
        .replace("#1 first !2", "#1 first !4")
        .replace("#2 second", "#2 second, by a peer");
    std::fs::write(r.main.join("TASKS.md"), t).unwrap();
    r.git(&r.main, &["add", "TASKS.md"]);
    let out = r.ok(&r.main, &["set", "1", "level", "2"]);
    assert!(out.contains("  checkout fixed: #1 matches main"), "{out}");
    let s = staged();
    assert!(
        s.contains("#1 first !2") && s.contains("#2 second, by a peer"),
        "{s}"
    );
    assert_eq!(r.git(&r.main, &["status", "--porcelain"]), "M  TASKS.md");
}

#[test]
fn an_archive_that_commits_nothing_stages_both_files_together() {
    let r = Repo::new("archive-staged-only");
    r.ok(&r.main, &["add", "first"]);
    r.ok(&r.main, &["done", "1", "--self"]);
    r.ok(&r.main, &["archive"]);
    r.ok(&r.main, &["add", "second"]);
    r.ok(&r.main, &["add", "third"]);
    let head = r.git(&r.main, &["rev-parse", "HEAD"]);

    // A peer closes #3 by hand and stages only that: nothing is closed on the
    // trunk, so archive commits nothing, but the staged move of #3 takes both
    // files, and the line does not claim the checkout matches main.
    let t = r
        .tasks()
        .replace("- [ ] #3 third", "- [x] #3 third via:self");
    std::fs::write(r.main.join("TASKS.md"), t).unwrap();
    r.git(&r.main, &["add", "TASKS.md"]);
    let out = r.ok(&r.main, &["archive"]);
    assert_eq!(r.git(&r.main, &["rev-parse", "HEAD"]), head);
    assert!(out.contains("  checkout updated: #3"), "{out}");
    assert!(!out.contains("matches main"), "{out}");
    let (sq, sa) = (
        r.git(&r.main, &["show", ":TASKS.md"]),
        r.git(&r.main, &["show", ":DONE.md"]),
    );
    assert!(
        !sq.contains("#3 third") && sa.contains("- [x] #3 third"),
        "{sq}\n{sa}"
    );
    r.ok(&r.main, &["lint", "--staged"]);
}

const ARCHIVE_HEADER: &str = "# Archive\n\nClosed tasks moved out of the queue by `5w archive`. Their ids stay taken, they still\nsatisfy `needs:`, and ship still reads their `branch:` and `reviewed:`.\n";

#[test]
fn an_archive_of_a_staged_close_stages_a_done_file_the_trunk_lacks() {
    let r = Repo::new("archive-staged-no-done");
    r.ok(&r.main, &["add", "first"]);
    r.ok(&r.main, &["add", "second"]);
    let head = r.git(&r.main, &["rev-parse", "HEAD"]);
    assert!(r.git(&r.main, &["ls-files", "DONE.md"]).is_empty());

    // #2 is closed only in the staged copy and DONE.md is on neither the trunk
    // nor the index: the staged move takes a new DONE.md holding just that row.
    hand_edit(&r, "- [ ] #2 second", "- [x] #2 second via:self");
    let out = r.ok(&r.main, &["archive"]);
    assert_eq!(r.git(&r.main, &["rev-parse", "HEAD"]), head, "{out}");
    let (sq, sa) = (
        r.git(&r.main, &["show", ":TASKS.md"]),
        r.git(&r.main, &["show", ":DONE.md"]),
    );
    assert!(
        !sq.contains("#2 second") && sa.contains("- [x] #2 second") && !sa.contains("#1"),
        "{sq}\n{sa}"
    );
    r.ok(&r.main, &["lint", "--staged"]);
    r.lint_history();

    // An untracked DONE.md holding only archive's own header has nothing to
    // leave half-tracked: the staged move goes ahead the same.
    let r = Repo::new("archive-staged-header-done");
    r.ok(&r.main, &["add", "first"]);
    hand_edit(&r, "- [ ] #1 first", "- [x] #1 first via:self");
    std::fs::write(r.main.join("DONE.md"), ARCHIVE_HEADER).unwrap();
    r.ok(&r.main, &["archive"]);
    assert!(
        r.git(&r.main, &["show", ":DONE.md"])
            .contains("- [x] #1 first")
    );
    r.ok(&r.main, &["lint", "--staged"]);
}

#[test]
fn an_archive_refuses_an_untracked_done_file_of_its_own_and_names_a_clean_fix() {
    let r = Repo::new("archive-staged-untracked-done");
    r.ok(&r.main, &["add", "first"]);
    r.ok(&r.main, &["add", "second"]);
    // A checkout-only archive leaves #1 in an untracked DONE.md.
    let t = r
        .tasks()
        .replace("- [ ] #1 first", "- [x] #1 first via:self");
    std::fs::write(r.main.join("TASKS.md"), t).unwrap();
    r.ok(&r.main, &["archive"]);
    assert!(r.git(&r.main, &["ls-files", "DONE.md"]).is_empty());

    // #2 is then closed in both the working copy and the index, which still
    // has #1: staging DONE.md would stage rows that are not the plan's.
    let close2 = |t: &str| t.replace("- [ ] #2 second", "- [x] #2 second via:self");
    std::fs::write(r.main.join("TASKS.md"), close2(&r.tasks())).unwrap();
    let staged = close2(&r.git(&r.main, &["show", "HEAD:TASKS.md"]));
    let path = r.main.join(".git").join("staged-tasks");
    std::fs::write(&path, format!("{staged}\n")).unwrap();
    let blob = r.git(&r.main, &["hash-object", "-w", path.to_str().unwrap()]);
    std::fs::remove_file(&path).unwrap();
    r.git(
        &r.main,
        &[
            "update-index",
            "--cacheinfo",
            &format!("100644,{blob},TASKS.md"),
        ],
    );
    let (tasks, done, index) = (
        r.tasks(),
        std::fs::read_to_string(r.main.join("DONE.md")).unwrap(),
        r.git(&r.main, &["show", ":TASKS.md"]),
    );
    let err = r.fails(&r.main, &["archive"]);
    assert_eq!(err.trim().lines().count(), 1, "{err}");
    assert!(
        err.contains("`git add -f DONE.md && git add -p TASKS.md`"),
        "{err}"
    );
    assert_eq!(r.tasks(), tasks);
    assert_eq!(
        std::fs::read_to_string(r.main.join("DONE.md")).unwrap(),
        done
    );
    assert_eq!(r.git(&r.main, &["show", ":TASKS.md"]), index);

    // Following the fix line (here every hunk `add -p` offers is #1's removal)
    // leaves a staged copy lint passes, and archive then runs.
    r.git(&r.main, &["add", "-f", "DONE.md"]);
    r.git(&r.main, &["add", "TASKS.md"]);
    r.ok(&r.main, &["lint", "--staged"]);
    r.ok(&r.main, &["archive"]);
    r.ok(&r.main, &["lint", "--staged"]);

    // Plain notes in an untracked DONE.md are refused the same, untouched.
    let r = Repo::new("archive-staged-notes-done");
    r.ok(&r.main, &["add", "first"]);
    hand_edit(&r, "- [ ] #1 first", "- [x] #1 first via:self");
    std::fs::write(r.main.join("DONE.md"), "my notes\n").unwrap();
    let (tasks, index) = (r.tasks(), r.git(&r.main, &["show", ":TASKS.md"]));
    let err = r.fails(&r.main, &["archive"]);
    assert!(err.contains("untracked DONE.md has content"), "{err}");
    assert_eq!(r.tasks(), tasks);
    assert_eq!(r.git(&r.main, &["show", ":TASKS.md"]), index);
    assert_eq!(
        std::fs::read_to_string(r.main.join("DONE.md")).unwrap(),
        "my notes\n"
    );
}

#[test]
fn an_archive_into_a_missing_directory_creates_it_and_moves_the_row() {
    let r = Repo::new("archive-missing-dir");
    let cfg = std::fs::read_to_string(r.main.join(".5w.toml"))
        .unwrap()
        .replace("archive = \"DONE.md\"", "archive = \"docs/CLOSED.md\"");
    assert!(cfg.contains("docs/CLOSED.md"), "{cfg}");
    std::fs::write(r.main.join(".5w.toml"), cfg).unwrap();
    r.git(&r.main, &["commit", "-qam", "archive into docs"]);
    r.ok(&r.main, &["add", "first"]);
    r.ok(&r.main, &["add", "second"]);
    r.ok(&r.main, &["done", "1", "--self"]);
    assert!(!r.main.join("docs").exists());

    let out = r.ok(&r.main, &["archive"]);
    let closed = std::fs::read_to_string(r.main.join("docs/CLOSED.md")).unwrap();
    assert!(closed.contains("- [x] #1 first"), "{out}\n{closed}");
    assert!(!r.tasks().contains("#1 first"), "{}", r.tasks());
    assert!(
        r.git(&r.main, &["show", "main:docs/CLOSED.md"])
            .contains("- [x] #1 first")
    );
    assert_eq!(r.git(&r.main, &["status", "--porcelain"]), "");
    r.ok(&r.main, &["lint"]);
}

#[test]
fn an_archive_that_cannot_write_the_archive_file_writes_nothing() {
    let r = Repo::new("archive-unwritable");
    r.ok(&r.main, &["add", "first"]);
    r.ok(&r.main, &["done", "1", "--self"]);
    // The archive's path is a directory: no file can land there, so neither
    // working file changes and the trunk does not move.
    std::fs::create_dir_all(r.main.join("DONE.md/inside")).unwrap();
    let (head, tasks) = (r.git(&r.main, &["rev-parse", "main"]), r.tasks());
    let err = r.refuses(&r.main, &["archive"]);
    assert!(err.contains("cannot write DONE.md"), "{err}");
    assert!(err.contains("nothing written"), "{err}");
    assert_eq!(r.git(&r.main, &["rev-parse", "main"]), head);
    assert_eq!(r.tasks(), tasks);
    assert!(r.main.join("DONE.md/inside").is_dir());
    let stray = std::fs::read_dir(&r.main)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains("5w-"))
        .collect::<Vec<_>>();
    assert!(stray.is_empty(), "{stray:?}");
}

/// Run the fix a refusal names between backticks, in the checkout.
fn follow_fix(r: &Repo, err: &str) {
    follow_fix_in(r, &r.main, err)
}

/// Run the fix a refusal names between backticks, in `cwd`.
fn follow_fix_in(r: &Repo, cwd: &Path, err: &str) {
    let fix = err
        .split('`')
        .nth(1)
        .unwrap_or_else(|| panic!("no fix in {err}"));
    let mut c = Command::new("sh");
    c.args(["-c", fix]).current_dir(cwd);
    env(&mut c, &r.root);
    let o = c.output().unwrap();
    assert!(
        o.status.success(),
        "{fix}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
}

/// Move the queue to docs/TASKS.md and track TASKS.md as a symlink to it.
fn link_queue(r: &Repo) {
    std::fs::create_dir_all(r.main.join("docs")).unwrap();
    r.git(&r.main, &["mv", "TASKS.md", "docs/TASKS.md"]);
    std::os::unix::fs::symlink("docs/TASKS.md", r.main.join("TASKS.md")).unwrap();
    r.git(&r.main, &["add", "TASKS.md"]);
    r.git(
        &r.main,
        &["commit", "-qm", "queue lives in docs", "--no-verify"],
    );
}

#[test]
fn a_queue_tracked_as_a_symlink_refuses_writes_and_names_the_fix() {
    let r = Repo::new("symlinked-queue");
    r.ok(&r.main, &["add", "first"]);
    let (wt, local) = wt_with_local_queue_line(&r);
    link_queue(&r);
    let head = r.git(&r.main, &["rev-parse", "main"]);
    let err = r.refuses(&wt, &["add", "second"]);
    assert!(err.contains("TASKS.md is a symlink on main"), "{err}");
    assert_eq!(r.git(&r.main, &["rev-parse", "main"]), head);
    assert!(r.main.join("TASKS.md").is_symlink());
    assert_eq!(r.git(&r.main, &["status", "--porcelain"]), "");
    // Reads still go through the link.
    assert!(r.ok(&r.main, &["show", "1"]).contains("first"));

    // Typed in a worktree, the fix still lands in the checkout.
    follow_fix_in(&r, &wt, &err);
    assert_eq!(std::fs::read_to_string(wt.join("TASKS.md")).unwrap(), local);
    // The fix itself passes lint.
    r.ok(&r.main, &["lint", "--staged"]);
    r.git(&r.main, &["commit", "-qm", "queue back in place"]);
    r.ok(&r.main, &["add", "second"]);
    let entry = r.git(&r.main, &["ls-tree", "main", "TASKS.md"]);
    assert!(entry.starts_with("100644 "), "{entry}");
    assert!(r.tasks().contains("#2 second"), "{}", r.tasks());
    assert_eq!(r.git(&r.main, &["status", "--porcelain"]), "");
}

/// A worktree whose own TASKS.md carries an uncommitted line a fix must not touch.
fn wt_with_local_queue_line(r: &Repo) -> (PathBuf, String) {
    r.ok(&r.main, &["wt", "new", "a/x"]);
    let wt = r.wt("a/x");
    let mut local = std::fs::read_to_string(wt.join("TASKS.md")).unwrap();
    local.push_str("<!-- local -->\n");
    std::fs::write(wt.join("TASKS.md"), &local).unwrap();
    (wt, local)
}

#[test]
fn a_queue_symlink_to_a_non_queue_file_names_a_path_scoped_restore() {
    let r = Repo::new("symlinked-queue-source");
    r.ok(&r.main, &["add", "first"]);
    let (wt, local) = wt_with_local_queue_line(&r);
    // One commit points the link at a source file and adds another file.
    std::fs::remove_file(r.main.join("TASKS.md")).unwrap();
    std::os::unix::fs::symlink("README", r.main.join("TASKS.md")).unwrap();
    std::fs::write(r.main.join("app.rs"), "fn main() {}\n").unwrap();
    r.git(&r.main, &["add", "TASKS.md", "app.rs"]);
    r.git(&r.main, &["commit", "-qm", "retarget", "--no-verify"]);
    let err = r.refuses(&wt, &["add", "second"]);
    assert!(err.contains("not a queue file"), "{err}");
    assert!(!err.contains("cp ") && !err.contains("revert"), "{err}");

    follow_fix_in(&r, &wt, &err);
    assert_eq!(std::fs::read_to_string(wt.join("TASKS.md")).unwrap(), local);
    assert!(!r.main.join("TASKS.md").is_symlink());
    assert_eq!(r.git(&r.main, &["status", "--porcelain"]), "T  TASKS.md");
    r.ok(&r.main, &["lint", "--staged"]);
    r.git(&r.main, &["commit", "-qm", "queue back in place"]);
    assert!(r.main.join("app.rs").exists());
    r.ok(&r.main, &["add", "second"]);
    assert!(r.tasks().contains("#2 second"), "{}", r.tasks());
}

/// A side branch cut after #1 and #2 points TASKS.md at README; main adds #3,
/// then a --no-ff merge takes the link.
fn merge_a_side_link(r: &Repo) {
    r.ok(&r.main, &["add", "first"]);
    r.ok(&r.main, &["add", "second"]);
    r.git(&r.main, &["checkout", "-q", "-b", "side"]);
    std::fs::remove_file(r.main.join("TASKS.md")).unwrap();
    std::os::unix::fs::symlink("README", r.main.join("TASKS.md")).unwrap();
    std::fs::write(r.main.join("app.rs"), "fn main() {}\n").unwrap();
    r.git(&r.main, &["add", "TASKS.md", "app.rs"]);
    r.git(&r.main, &["commit", "-qm", "retarget", "--no-verify"]);
    r.git(&r.main, &["checkout", "-q", "main"]);
    r.ok(&r.main, &["add", "third"]);
    let mut c = Command::new("git");
    c.args([
        "merge",
        "-q",
        "--no-ff",
        "--no-verify",
        "side",
        "-m",
        "merge side",
    ])
    .current_dir(&r.main);
    env(&mut c, &r.root);
    // Git records each type on its own path; keep the link.
    c.output().unwrap();
    r.git(&r.main, &["rm", "-q", "--cached", "TASKS.md~HEAD"]);
    std::fs::remove_file(r.main.join("TASKS.md~HEAD")).unwrap();
    std::fs::remove_file(r.main.join("TASKS.md")).unwrap();
    r.git(&r.main, &["checkout", "side", "--", "TASKS.md"]);
    r.git(&r.main, &["add", "TASKS.md"]);
    r.git(&r.main, &["commit", "-qm", "merge side", "--no-verify"]);
    r.git(&r.main, &["rev-parse", "--verify", "HEAD^2"]);
    assert!(r.main.join("TASKS.md").is_symlink());
}

#[test]
fn a_queue_symlink_merged_from_a_side_branch_restores_the_trunk_queue() {
    let r = Repo::new("symlinked-queue-merged");
    merge_a_side_link(&r);
    let err = r.refuses(&r.main, &["add", "fourth"]);
    assert!(err.contains("~1 -- TASKS.md"), "{err}");
    follow_fix(&r, &err);
    r.ok(&r.main, &["lint", "--staged"]);
    r.git(&r.main, &["commit", "-qm", "queue back in place"]);
    r.ok(&r.main, &["lint", "HEAD"]);
    let tasks = r.tasks();
    for row in ["#1 first", "#2 second", "#3 third"] {
        assert!(tasks.contains(row), "{tasks}");
    }
    r.ok(&r.main, &["add", "fourth"]);
    assert!(r.tasks().contains("#4 fourth"), "{}", r.tasks());
    assert!(r.main.join("app.rs").exists());
}

#[test]
fn lint_judges_a_queue_link_made_a_file_against_the_file_before_the_link() {
    let r = Repo::new("lint-unlinked-queue");
    merge_a_side_link(&r);
    // The merge that took the link is flagged, as a commit making one is.
    let err = r.fails(&r.main, &["lint", "HEAD"]);
    assert!(err.contains("makes it a symlink"), "{err}");
    // So is a merge whose resolution restores the stale file.
    r.git(&r.main, &["branch", "stale", "side~1"]);
    r.git(
        &r.main,
        &["merge", "-q", "--no-commit", "-s", "ours", "stale"],
    );
    std::fs::remove_file(r.main.join("TASKS.md")).unwrap();
    r.git(&r.main, &["checkout", "side~1", "--", "TASKS.md"]);
    r.git(&r.main, &["commit", "-qm", "merge stale", "--no-verify"]);
    let err = r.fails(&r.main, &["lint", "HEAD"]);
    assert!(err.contains("#3: deleted"), "{err}");
    r.git(&r.main, &["reset", "-q", "--hard", "HEAD~1"]);
    // The side commit's parent predates #3: restoring from it drops a row.
    std::fs::remove_file(r.main.join("TASKS.md")).unwrap();
    r.git(&r.main, &["checkout", "side~1", "--", "TASKS.md"]);
    let err = r.fails(&r.main, &["lint", "--staged"]);
    assert!(err.contains("#3") && err.contains("deleted"), "{err}");
    r.git(&r.main, &["commit", "-qm", "stale restore", "--no-verify"]);
    let err = r.fails(&r.main, &["lint", "HEAD"]);
    assert!(err.contains("#3") && err.contains("deleted"), "{err}");
}

#[test]
fn a_queue_symlink_set_by_a_root_commit_names_a_fix_by_hand() {
    let r = Repo::new("symlinked-queue-root");
    r.ok(&r.main, &["add", "first"]);
    // A new history whose first commit already tracks the link.
    r.git(&r.main, &["checkout", "-q", "--orphan", "fresh"]);
    std::fs::remove_file(r.main.join("TASKS.md")).unwrap();
    std::os::unix::fs::symlink("README", r.main.join("TASKS.md")).unwrap();
    r.git(&r.main, &["add", "-A"]);
    r.git(&r.main, &["commit", "-qm", "fresh", "--no-verify"]);
    r.git(&r.main, &["branch", "-M", "fresh", "main"]);
    let err = r.refuses(&r.main, &["add", "second"]);
    assert!(err.contains("by hand"), "{err}");
    assert!(!err.contains('`'), "{err}");
}

#[test]
fn a_queue_symlinked_only_in_the_checkout_refuses_writes() {
    let r = Repo::new("working-symlinked-queue");
    r.ok(&r.main, &["add", "first"]);
    let (wt, local) = wt_with_local_queue_line(&r);
    // TASKS.md stays a file on main; the checkout's copy is a link outside it.
    let outside = r.root.join("elsewhere.md");
    std::fs::copy(r.main.join("TASKS.md"), &outside).unwrap();
    std::fs::remove_file(r.main.join("TASKS.md")).unwrap();
    std::os::unix::fs::symlink(&outside, r.main.join("TASKS.md")).unwrap();
    let before = std::fs::read_to_string(&outside).unwrap();
    let head = r.git(&r.main, &["rev-parse", "main"]);
    let err = r.refuses(&wt, &["add", "second"]);
    assert!(err.contains("TASKS.md in "), "{err}");
    assert!(
        err.contains(&format!("symlink to {}", outside.display())),
        "{err}"
    );
    assert_eq!(std::fs::read_to_string(&outside).unwrap(), before);
    assert_eq!(r.git(&r.main, &["rev-parse", "main"]), head);

    follow_fix_in(&r, &wt, &err);
    assert_eq!(std::fs::read_to_string(wt.join("TASKS.md")).unwrap(), local);
    assert!(!r.main.join("TASKS.md").is_symlink());
    r.ok(&r.main, &["add", "second"]);
    assert_eq!(std::fs::read_to_string(&outside).unwrap(), before);
    assert!(r.tasks().contains("#2 second"), "{}", r.tasks());
    assert_eq!(r.git(&r.main, &["status", "--porcelain"]), "");
}

#[test]
fn lint_flags_a_queue_file_made_a_symlink() {
    let r = Repo::new("lint-symlinked-queue");
    r.ok(&r.main, &["add", "first"]);
    link_queue(&r);
    let err = r.fails(&r.main, &["lint", "HEAD"]);
    assert!(
        err.contains("TASKS.md") && err.contains("makes it a symlink"),
        "{err}"
    );
}

#[test]
fn lint_flags_a_queue_symlink_pointed_elsewhere() {
    let r = Repo::new("lint-retargeted-queue");
    r.ok(&r.main, &["add", "first"]);
    link_queue(&r);
    // Retargeted at another file, the link shows no row change at all.
    std::fs::remove_file(r.main.join("TASKS.md")).unwrap();
    std::os::unix::fs::symlink("README", r.main.join("TASKS.md")).unwrap();
    r.git(&r.main, &["add", "TASKS.md"]);
    r.git(&r.main, &["commit", "-qm", "retarget", "--no-verify"]);
    let err = r.fails(&r.main, &["lint", "HEAD"]);
    assert!(err.contains("points its symlink elsewhere"), "{err}");

    // The same change staged.
    std::fs::remove_file(r.main.join("TASKS.md")).unwrap();
    std::os::unix::fs::symlink(".5w.toml", r.main.join("TASKS.md")).unwrap();
    r.git(&r.main, &["add", "TASKS.md"]);
    let err = r.fails(&r.main, &["lint", "--staged"]);
    assert!(err.contains("points its symlink elsewhere"), "{err}");
}

#[test]
fn a_queue_write_removes_a_private_index_a_killed_run_left() {
    let r = Repo::new("stale-private-index");
    // A killed run (say, at the pinentry during commit-tree) leaves its private
    // index in the git dir; the next write removes it under the lock.
    let stale = r.main.join(".git/5w-index-99999");
    std::fs::write(&stale, "stale\n").unwrap();
    // The user's look-alikes stay.
    let mine: Vec<_> = ["5w-index-mine", "5w-index-99999.bak", "5w-index-1-2"]
        .iter()
        .map(|n| r.main.join(".git").join(n))
        .collect();
    for m in &mine {
        std::fs::write(m, "mine\n").unwrap();
    }
    r.ok(&r.main, &["add", "first"]);
    assert!(r.tasks().contains("#1 first"));
    assert!(!stale.exists());
    assert!(mine.iter().all(|m| m.exists()));
}

#[test]
fn an_archive_whose_checkout_index_is_locked_says_it_committed_and_keeps_a_hand_line() {
    let r = Repo::new("archive-index-locked");
    r.ok(&r.main, &["add", "first"]);
    r.ok(&r.main, &["add", "second"]);
    r.ok(&r.main, &["done", "1", "--self"]);
    // A hand line of the checkout's own, away from the rows.
    let t = r.tasks().replacen("# Tasks\n", "# Tasks\n\nmy note\n", 1);
    std::fs::write(r.main.join("TASKS.md"), t).unwrap();
    // A temporary file a killed run left in the git dir goes under the lock.
    let stale = r.main.join(".git/5w-write-99999-0-TASKS.md");
    std::fs::write(&stale, "stale\n").unwrap();
    // One of the user's that only looks alike stays.
    let mine = r.main.join(".git/5w-write-mine-TASKS.md");
    std::fs::write(&mine, "mine\n").unwrap();
    std::fs::write(r.main.join(".git/index.lock"), "").unwrap();
    let head = r.git(&r.main, &["rev-parse", "main"]);

    let err = r.refuses(&r.main, &["archive"]);
    assert_ne!(r.git(&r.main, &["rev-parse", "main"]), head);
    assert!(
        err.contains("committed #1 to main, but the checkout was not updated"),
        "{err}"
    );
    assert!(
        err.contains("| git -C") && err.contains("apply --cached"),
        "{err}"
    );
    assert!(!err.contains("checkout main --"), "{err}");
    assert!(!stale.exists() && mine.exists());
    assert!(err.contains("--3way"), "{err}");
    assert!(r.tasks().contains("#1 first") && r.tasks().contains("my note"));

    // Following the fix once the lock is gone catches the checkout up to main
    // and keeps the hand line.
    std::fs::remove_file(r.main.join(".git/index.lock")).unwrap();
    follow_fix(&r, &err);
    let t = r.tasks();
    assert!(!t.contains("#1 first") && t.contains("my note"), "{t}");
    let done = std::fs::read_to_string(r.main.join("DONE.md")).unwrap();
    assert!(done.contains("- [x] #1 first"), "{done}");
    assert_eq!(r.git(&r.main, &["diff", "--cached", "--name-only"]), "");
    assert_eq!(r.git(&r.main, &["status", "--porcelain"]), " M TASKS.md");
}

#[test]
fn a_first_archive_the_checkout_missed_names_the_fix_in_the_duplicate_ids_refusal() {
    let r = Repo::new("first-archive-behind");
    r.ok(&r.main, &["add", "first"]);
    r.ok(&r.main, &["add", "second"]);
    r.ok(&r.main, &["done", "1", "--self"]);
    std::fs::write(r.main.join(".git/index.lock"), "").unwrap();
    r.refuses(&r.main, &["archive"]);
    std::fs::remove_file(r.main.join(".git/index.lock")).unwrap();

    // The checkout still has #1 in its queue and no archive of its own, so every
    // read sees #1 twice before any write could catch it up: the refusal names
    // the diff that does.
    let err = r.refuses(&r.main, &["ready"]);
    assert!(
        err.contains("duplicate ids") && err.contains("#1 in both TASKS.md and DONE.md"),
        "{err}"
    );
    assert!(
        err.contains("diff ") && err.contains("| git -C") && err.contains("apply --cached"),
        "{err}"
    );
    assert!(r.main.join(".git/5w-missed-main").exists());
    follow_fix(&r, &err);
    r.ok(&r.main, &["ready"]);
    assert_eq!(r.git(&r.main, &["status", "--porcelain"]), "");
    let done = std::fs::read_to_string(r.main.join("DONE.md")).unwrap();
    assert!(done.contains("- [x] #1 first"), "{done}");
    // The fix typed by hand leaves the marker of the missed commit; the next
    // `lint --staged` (the hook) finds the checkout caught up and removes it.
    assert!(r.main.join(".git/5w-missed-main").exists());
    r.ok(&r.main, &["lint", "--staged"]);
    assert!(!r.main.join(".git/5w-missed-main").exists());
}

#[test]
fn a_commit_over_a_missed_first_archive_does_not_delete_the_archive() {
    let r = Repo::new("first-archive-commit");
    r.ok(&r.main, &["add", "first"]);
    r.ok(&r.main, &["add", "second"]);
    r.ok(&r.main, &["done", "1", "--self"]);
    r.ok(&r.main, &["hook", "install"]);
    std::fs::write(r.main.join(".git/index.lock"), "").unwrap();
    r.refuses(&r.main, &["archive"]);
    std::fs::remove_file(r.main.join(".git/index.lock")).unwrap();
    // The checkout's index still holds the queue before the archive, and no
    // archive: committed as it stands, it would take DONE.md off main and put
    // #1 back in TASKS.md.
    assert_eq!(
        r.git(&r.main, &["status", "--porcelain", "--", "DONE.md"]),
        "D  DONE.md"
    );
    let commit = |args: &[&str]| {
        let mut c = Command::new("git");
        c.args(args).current_dir(&r.main);
        env(&mut c, &r.root);
        let o = c.output().unwrap();
        (
            o.status.success(),
            String::from_utf8_lossy(&o.stderr).to_string(),
        )
    };
    // The queue files alone pass every other rule: the hook refuses the move back
    // and names the diff that catches the checkout up.
    let (ok, err) = commit(&["commit", "-qm", "wip"]);
    assert!(!ok, "{err}");
    assert!(
        err.contains("#1: archived in DONE.md, back in TASKS.md") && err.contains("apply --cached"),
        "{err}"
    );
    std::fs::write(r.main.join("README"), "changed\n").unwrap();
    let (ok, err) = commit(&["commit", "-qam", "unrelated"]);
    assert!(!ok && err.contains("back in TASKS.md"), "{err}");
    assert!(
        r.git(&r.main, &["ls-tree", "--name-only", "main"])
            .contains("DONE.md")
    );
    let out = String::from_utf8_lossy(&r.cli(&r.main, &["doctor"]).stdout).to_string();
    assert!(
        out.contains("#1 is in both") && out.contains("apply --cached"),
        "{out}"
    );

    // A commit that skipped the hook is caught by linting it.
    let (ok, err) = commit(&[
        "commit",
        "-qm",
        "wip",
        "--no-verify",
        "--",
        "DONE.md",
        "TASKS.md",
    ]);
    assert!(ok, "{err}");
    let err = r.fails(&r.main, &["lint", "HEAD"]);
    assert!(
        err.contains("#1: archived in DONE.md, back in TASKS.md"),
        "{err}"
    );
}

#[test]
fn a_commit_over_a_missed_later_archive_is_refused_by_the_hook() {
    let r = Repo::new("later-archive-commit");
    r.ok(&r.main, &["add", "first"]);
    r.ok(&r.main, &["add", "second"]);
    r.ok(&r.main, &["done", "1", "--self"]);
    r.ok(&r.main, &["archive"]);
    r.ok(&r.main, &["done", "2", "--self"]);
    r.ok(&r.main, &["hook", "install"]);
    std::fs::write(r.main.join(".git/index.lock"), "").unwrap();
    r.refuses(&r.main, &["archive"]);
    std::fs::remove_file(r.main.join(".git/index.lock")).unwrap();
    let marker = r.main.join(".git/5w-missed-main");
    assert!(marker.exists());
    let commit = |args: &[&str]| {
        let mut c = Command::new("git");
        c.args(args).current_dir(&r.main);
        env(&mut c, &r.root);
        let o = c.output().unwrap();
        (
            o.status.success(),
            String::from_utf8_lossy(&o.stderr).to_string(),
        )
    };
    // DONE.md is still there: only the marker tells the hook this is the missed
    // archive and not an unarchive.
    for args in [&["commit", "-qm", "wip"][..], &["commit", "-qam", "wip"]] {
        let (ok, err) = commit(args);
        assert!(!ok, "{args:?}: {err}");
        assert!(
            err.contains("#2: archived in DONE.md, back in TASKS.md")
                && err.contains("missed a commit to main")
                && err.contains("apply --cached"),
            "{err}"
        );
    }
    assert!(
        r.git(&r.main, &["show", "main:DONE.md"])
            .contains("#2 second")
    );

    // The next write catches the checkout up and removes the marker.
    let out = r.ok(&r.main, &["add", "third"]);
    assert!(out.contains("checkout caught up: #2"), "{out}");
    assert!(!marker.exists());
    assert_eq!(r.git(&r.main, &["status", "--porcelain"]), "");
}

#[test]
fn a_checkout_fixed_by_hand_after_a_missed_archive_can_unarchive() {
    let r = Repo::new("missed-archive-fixed-by-hand");
    r.ok(&r.main, &["add", "first"]);
    r.ok(&r.main, &["add", "second"]);
    r.ok(&r.main, &["done", "1", "--self"]);
    r.ok(&r.main, &["archive"]);
    r.ok(&r.main, &["done", "2", "--self"]);
    r.ok(&r.main, &["hook", "install"]);
    std::fs::write(r.main.join(".git/index.lock"), "").unwrap();
    r.refuses(&r.main, &["archive"]);
    std::fs::remove_file(r.main.join(".git/index.lock")).unwrap();
    let marker = r.main.join(".git/5w-missed-main");
    assert!(marker.exists());

    // Doctor names the fix while the checkout is behind, and writes nothing.
    let out = r.fails(&r.main, &["doctor"]);
    assert!(
        out.contains("missed a commit to main") && out.contains("apply --cached"),
        "{out}"
    );
    assert!(marker.exists());

    // The checkout fixed by hand: doctor notes the marker left over, and leaves
    // it for a write or the hook.
    r.git(
        &r.main,
        &[
            "restore",
            "--staged",
            "--worktree",
            "--source=HEAD",
            "--",
            "TASKS.md",
            "DONE.md",
        ],
    );
    assert_eq!(r.git(&r.main, &["status", "--porcelain"]), "");
    let out = r.ok(&r.main, &["doctor"]);
    assert!(
        !out.contains("missed a commit") && out.contains("marker for main is left over"),
        "{out}"
    );
    assert!(marker.exists());

    // A deliberate unarchive staged over the fixed checkout passes the hook,
    // which removes the marker: #2, the row the missed archive moved, is in
    // DONE.md as main has it.
    let row = "- [x] #1 first via:self";
    let done = std::fs::read_to_string(r.main.join("DONE.md")).unwrap();
    assert!(done.contains(row), "{done}");
    std::fs::write(
        r.main.join("DONE.md"),
        done.replace(&format!("{row}\n"), ""),
    )
    .unwrap();
    let tasks = r
        .tasks()
        .replace("## Done\n\n", &format!("## Done\n\n{row}\n"));
    std::fs::write(r.main.join("TASKS.md"), tasks).unwrap();
    r.git(&r.main, &["add", "TASKS.md", "DONE.md"]);
    let mut c = Command::new("git");
    c.args(["commit", "-qm", "chore(tasks): unarchive #1"])
        .current_dir(&r.main);
    env(&mut c, &r.root);
    let o = c.output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(!marker.exists());
    r.ok(&r.main, &["lint", "HEAD"]);
}

#[test]
fn a_row_staged_over_a_missed_archive_keeps_the_hook_refusing() {
    let r = Repo::new("missed-archive-new-row");
    r.ok(&r.main, &["add", "first"]);
    r.ok(&r.main, &["add", "second"]);
    r.ok(&r.main, &["done", "1", "--self"]);
    r.ok(&r.main, &["archive"]);
    r.ok(&r.main, &["done", "2", "--self"]);
    r.ok(&r.main, &["hook", "install"]);
    std::fs::write(r.main.join(".git/index.lock"), "").unwrap();
    r.refuses(&r.main, &["archive"]);
    std::fs::remove_file(r.main.join(".git/index.lock")).unwrap();
    let marker = r.main.join(".git/5w-missed-main");

    // A new row staged by hand changes the index, not #2, which the checkout
    // still has back in the queue: the marker stands.
    let tasks = r
        .tasks()
        .replace("## Open\n\n", "## Open\n\n- [ ] #3 third\n");
    assert!(tasks.contains("#3 third"), "{tasks}");
    std::fs::write(r.main.join("TASKS.md"), tasks).unwrap();
    r.git(&r.main, &["add", "TASKS.md"]);
    let mut c = Command::new("git");
    c.args(["commit", "-qm", "add third"]).current_dir(&r.main);
    env(&mut c, &r.root);
    let o = c.output().unwrap();
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(!o.status.success(), "{err}");
    assert!(
        err.contains("#2: archived in DONE.md, back in TASKS.md")
            && err.contains("missed a commit to main"),
        "{err}"
    );
    assert!(marker.exists());
    assert!(
        r.git(&r.main, &["show", "main:DONE.md"])
            .contains("#2 second")
    );
}

#[test]
fn a_checkout_index_update_that_fails_changes_neither_queue_entry() {
    let r = Repo::new("index-both-or-neither");
    r.ok(&r.main, &["add", "first"]);
    r.ok(&r.main, &["add", "second"]);
    r.ok(&r.main, &["done", "1", "--self"]);
    // A git that refuses to stage the archive in the checkout's own index: an
    // update of both entries that takes the queue's alone leaves #1 in neither.
    let real = Command::new("sh")
        .args(["-c", "command -v git"])
        .output()
        .unwrap();
    let real = String::from_utf8_lossy(&real.stdout).trim().to_string();
    let dir = r.root.join("fake-git");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("git"),
        format!(
            "#!/bin/sh\n\
             if [ -z \"$GIT_INDEX_FILE\" ]; then case \" $* \" in *\" update-index \"*)\n\
             case \" $* \" in *\" --index-info \"*) in=$(cat)\n\
             case $in in *DONE.md*) echo 'busy' >&2; exit 1;; esac\n\
             printf '%s\\n' \"$in\" | exec {real} \"$@\";;\n\
             *DONE.md*) echo 'busy' >&2; exit 1;; esac;; esac; fi\n\
             exec {real} \"$@\"\n"
        ),
    )
    .unwrap();
    std::fs::set_permissions(
        dir.join("git"),
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();
    let mut c = Command::new(bin5w());
    c.args(["archive"]).current_dir(&r.main);
    env(&mut c, &r.root);
    c.env("PATH", format!("{}:{}", dir.display(), path_with_5w()));
    let err = refusal(&c.output().unwrap(), &["archive"]);
    assert!(err.contains("committed #1 to main"), "{err}");

    // Neither entry moved, so the fix's diff applies to both.
    follow_fix(&r, &err);
    assert_eq!(r.git(&r.main, &["status", "--porcelain"]), "");
}

#[test]
fn a_write_after_a_locked_index_catches_the_checkout_up_to_the_trunk() {
    let r = Repo::new("catch-up");
    r.ok(&r.main, &["add", "first"]);
    std::fs::write(r.main.join(".git/index.lock"), "").unwrap();
    let err = r.refuses(&r.main, &["add", "second"]);
    assert!(err.contains("committed #2 to main"), "{err}");
    std::fs::remove_file(r.main.join(".git/index.lock")).unwrap();
    // A hand line of the checkout's own stays.
    let t = r.tasks().replacen("# Tasks\n", "# Tasks\n\nmy note\n", 1);
    std::fs::write(r.main.join("TASKS.md"), t).unwrap();

    // The next write puts the row the checkout missed back in before its own
    // edit: nothing staged or working reads as a revert of #2.
    let out = r.ok(&r.main, &["add", "third"]);
    assert!(out.contains("checkout caught up: #2"), "{out}");
    let t = r.tasks();
    assert!(
        t.contains("#1 first") && t.contains("#2 second") && t.contains("#3 third"),
        "{t}"
    );
    assert!(t.contains("my note"), "{t}");
    r.ok(&r.main, &["lint", "--staged"]);
    assert_eq!(r.git(&r.main, &["diff", "--cached", "--name-only"]), "");
    assert_eq!(r.git(&r.main, &["status", "--porcelain"]), " M TASKS.md");

    // Caught up, the next write says nothing of it.
    let out = r.ok(&r.main, &["add", "fourth"]);
    assert!(!out.contains("caught up"), "{out}");
}

#[test]
fn catching_the_checkout_up_moves_a_row_the_missed_archive_moved() {
    let r = Repo::new("catch-up-archive");
    r.ok(&r.main, &["add", "zero"]);
    r.ok(&r.main, &["done", "1", "--self"]);
    r.ok(&r.main, &["archive"]);
    r.ok(&r.main, &["add", "first"]);
    r.ok(&r.main, &["done", "2", "--self"]);
    // A peer's uncommitted row stays uncommitted.
    let t = r.tasks().replace("## Done", "- [ ] #9 peer\n\n## Done");
    std::fs::write(r.main.join("TASKS.md"), t).unwrap();
    std::fs::write(r.main.join(".git/index.lock"), "").unwrap();
    r.refuses(&r.main, &["archive"]);
    std::fs::remove_file(r.main.join(".git/index.lock")).unwrap();

    let out = r.ok(&r.main, &["add", "second"]);
    assert!(out.contains("checkout caught up: #2"), "{out}");
    r.ok(&r.main, &["lint", "--staged"]);
    let t = r.tasks();
    assert!(!t.contains("#2 first") && t.contains("#9 peer"), "{t}");
    let done = std::fs::read_to_string(r.main.join("DONE.md")).unwrap();
    assert!(done.contains("- [x] #2 first"), "{done}");
    assert_eq!(r.git(&r.main, &["status", "--porcelain"]), " M TASKS.md");
}

#[test]
fn catching_the_checkout_up_leaves_a_row_edited_there_by_hand() {
    let r = Repo::new("catch-up-edited");
    r.ok(&r.main, &["add", "first"]);
    std::fs::write(r.main.join(".git/index.lock"), "").unwrap();
    r.refuses(&r.main, &["done", "1", "--self"]);
    std::fs::remove_file(r.main.join(".git/index.lock")).unwrap();
    // The checkout's #1 is edited by hand after the missed commit.
    let t = r.tasks().replace("#1 first", "#1 first, reworded");
    std::fs::write(r.main.join("TASKS.md"), t).unwrap();

    let out = r.ok(&r.main, &["add", "second"]);
    assert!(out.contains("checkout caught up: #1"), "{out}");
    // The index takes the trunk's #1; the working file keeps the hand edit.
    r.ok(&r.main, &["lint", "--staged"]);
    assert_eq!(r.git(&r.main, &["diff", "--cached", "--name-only"]), "");
    let t = r.tasks();
    assert!(
        t.contains("- [ ] #1 first, reworded") && t.contains("#2 second"),
        "{t}"
    );
}

/// A trunk checkout mid-merge with TASKS.md in conflict: the side branch
/// rewords #1, the trunk replaces its line with `ours`.
fn queue_in_conflict(name: &str, ours: &str) -> Repo {
    let r = Repo::new(name);
    r.ok(&r.main, &["add", "first"]);
    let side = r.root.join("side");
    r.git(
        &r.main,
        &["worktree", "add", "-qb", "side", side.to_str().unwrap()],
    );
    let theirs = r
        .tasks()
        .replace("- [ ] #1 first\n", "- [ ] #1 first, theirs\n");
    std::fs::write(side.join("TASKS.md"), theirs).unwrap();
    r.git(&side, &["commit", "-qam", "theirs", "--no-verify"]);
    let ours = r.tasks().replace("- [ ] #1 first\n", ours);
    std::fs::write(r.main.join("TASKS.md"), ours).unwrap();
    r.git(&r.main, &["commit", "-qam", "ours", "--no-verify"]);
    let mut c = Command::new("git");
    c.args(["merge", "-q", "side"]).current_dir(&r.main);
    env(&mut c, &r.root);
    assert!(!c.output().unwrap().status.success());
    assert_ne!(r.git(&r.main, &["ls-files", "-u", "--", "TASKS.md"]), "");
    r
}

fn is_conflict_refusal(err: &str) {
    assert!(
        err.starts_with("5w: TASKS.md has an unresolved conflict in ")
            && err.ends_with("; resolve it (git add) first\n"),
        "{err}"
    );
}

#[test]
fn a_queue_write_refuses_while_the_queue_has_an_unresolved_conflict() {
    // Ours drops the row: the markers hold one #1, so reads pass.
    let r = queue_in_conflict("queue-conflict", "");
    let head = r.git(&r.main, &["rev-parse", "main"]);
    // Taken as it is, the write would collapse the conflict's stages into one
    // entry and leave the markers in the file.
    is_conflict_refusal(&r.refuses(&r.main, &["add", "second"]));
    assert_eq!(r.git(&r.main, &["rev-parse", "main"]), head);
    assert_ne!(r.git(&r.main, &["ls-files", "-u", "--", "TASKS.md"]), "");
    assert!(r.tasks().contains("<<<<<<<") && !r.tasks().contains("#2 second"));

    // Both sides keep a #1: the conflict, not its duplicate, is what to fix.
    let r = queue_in_conflict("queue-conflict-dup", "- [ ] #1 first, ours\n");
    is_conflict_refusal(&r.refuses(&r.main, &["ready"]));
}

#[test]
fn doctor_names_an_unresolved_queue_conflict() {
    let r = queue_in_conflict("doctor-conflict", "");
    let out = r.fails(&r.main, &["doctor"]);
    assert!(
        out.contains("TASKS.md has an unresolved conflict in ")
            && out.contains("TASKS.md line ")
            && out.contains("holds a git conflict marker"),
        "{out}"
    );
    // Markers removed by hand but never added: the stages still stand.
    let clean = r
        .tasks()
        .lines()
        .filter(|l| {
            !l.starts_with("<<<<<<<") && !l.starts_with("=======") && !l.starts_with(">>>>>>>")
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    std::fs::write(r.main.join("TASKS.md"), clean).unwrap();
    let out = r.fails(&r.main, &["doctor"]);
    assert!(
        out.contains("TASKS.md has an unresolved conflict in ") && !out.contains("marker"),
        "{out}"
    );
    r.git(&r.main, &["add", "TASKS.md"]);
    r.git(&r.main, &["commit", "-qm", "merged", "--no-verify"]);
    r.ok(&r.main, &["doctor"]);
}

#[test]
fn an_archive_that_cannot_rename_out_of_the_git_dir_copies_beside_each_file() {
    let r = Repo::new("archive-exdev");
    r.ok(&r.main, &["add", "first"]);
    r.ok(&r.main, &["add", "second"]);
    r.ok(&r.main, &["done", "1", "--self"]);
    // A stale temporary file beside the queue goes; a user's look-alike stays.
    let stale = r.main.join(".TASKS.md.5w-write-99999-0");
    let mine = r.main.join(".TASKS.md.5w-write-mine");
    std::fs::write(&stale, "stale\n").unwrap();
    std::fs::write(&mine, "mine\n").unwrap();

    // One filesystem bind-mounted twice shares a device id, yet a rename from
    // the git dir into the checkout fails EXDEV; the test hook fails it so.
    let mut c = Command::new(bin5w());
    c.args(["archive"]).current_dir(&r.main);
    env(&mut c, &r.root);
    c.env("FIVEW_TEST_EXDEV", "1");
    let o = c.output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));

    let t = r.tasks();
    assert!(!t.contains("#1 first") && t.contains("#2 second"), "{t}");
    let done = std::fs::read_to_string(r.main.join("DONE.md")).unwrap();
    assert!(done.contains("- [x] #1 first"), "{done}");
    assert!(!stale.exists() && mine.exists());
    assert_eq!(
        r.git(&r.main, &["status", "--porcelain"]),
        "?? .TASKS.md.5w-write-mine"
    );
    let left: Vec<_> = std::fs::read_dir(r.main.join(".git"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("5w-write-"))
        .collect();
    assert!(left.is_empty(), "{left:?}");
    r.ok(&r.main, &["lint"]);
}

#[test]
fn an_archive_whose_queue_rename_fails_leaves_the_row_in_both_files() {
    let r = Repo::new("archive-rename-fails");
    r.ok(&r.main, &["add", "first"]);
    r.ok(&r.main, &["add", "second"]);
    r.ok(&r.main, &["done", "1", "--self"]);
    // A hand line of the checkout's own, away from the rows.
    let t = r.tasks().replacen("# Tasks\n", "# Tasks\n\nmy note\n", 1);
    std::fs::write(r.main.join("TASKS.md"), t).unwrap();
    // The signing program runs between writing the files aside and renaming
    // them: it takes away the queue's, so only that rename fails.
    let git_dir = r.main.join(".git");
    let gpg = r.root.join("fake-gpg");
    std::fs::write(
        &gpg,
        format!(
            "#!/bin/sh\nrm -f {}/5w-write-*TASKS.md\ncat >/dev/null\nprintf '\\n[GNUPG:] SIG_CREATED D 1 8 00 0 0\\n' >&2\nprintf -- '-----BEGIN PGP SIGNATURE-----\\n\\nx\\n-----END PGP SIGNATURE-----\\n'\n",
            git_dir.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&gpg, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    r.git(&r.main, &["config", "gpg.program", gpg.to_str().unwrap()]);
    r.git(&r.main, &["config", "commit.gpgsign", "true"]);

    let err = r.refuses(&r.main, &["archive"]);
    assert!(
        err.contains("committed #1 to main") && err.contains("cannot write TASKS.md"),
        "{err}"
    );
    assert!(
        !err.contains("--cached") && !err.contains("DONE.md |"),
        "{err}"
    );
    let done = std::fs::read_to_string(r.main.join("DONE.md")).unwrap();
    assert!(done.contains("- [x] #1 first"), "{done}");
    assert!(r.tasks().contains("#1 first"));

    follow_fix(&r, &err);
    let t = r.tasks();
    assert!(!t.contains("#1 first") && t.contains("my note"), "{t}");
    assert_eq!(r.git(&r.main, &["diff", "--cached", "--name-only"]), "");
    assert_eq!(r.git(&r.main, &["status", "--porcelain"]), " M TASKS.md");
    let left: Vec<_> = std::fs::read_dir(&git_dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("5w-write-"))
        .collect();
    assert!(left.is_empty(), "{left:?}");
}

#[test]
fn an_archive_skips_a_trunk_closed_row_the_checkout_reopened() {
    let r = Repo::new("archive-reopened-by-hand");
    r.ok(&r.main, &["add", "first"]);
    r.ok(&r.main, &["add", "second"]);
    r.ok(&r.main, &["done", "1", "--self"]);
    r.ok(&r.main, &["done", "2", "--self"]);
    // #1 is reopened by hand in the working copy: it stays where it is on the
    // trunk and in the checkout, and only #2 is archived.
    let t = r
        .tasks()
        .replace("- [x] #1 first via:self", "- [ ] #1 first");
    std::fs::write(r.main.join("TASKS.md"), t).unwrap();
    let out = r.ok(&r.main, &["archive"]);
    assert!(
        out.contains(
            "  note: #1 is closed on main but open in the checkout; not archived — `5w reopen 1` reopens it on main, or restore the checkout's row"
        ),
        "{out}"
    );
    assert!(out.contains("  archived 1 → DONE.md"), "{out}");
    let (tq, ta) = (
        r.git(&r.main, &["show", "HEAD:TASKS.md"]),
        r.git(&r.main, &["show", "HEAD:DONE.md"]),
    );
    assert!(
        tq.contains("- [x] #1 first") && !ta.contains("#1 first") && ta.contains("#2 second"),
        "{tq}\n{ta}"
    );
    assert!(r.tasks().contains("- [ ] #1 first"));
    // The note's fix works and leaves no id in both files.
    r.ok(&r.main, &["reopen", "1"]);
    r.ok(&r.main, &["list"]);
    r.lint_history();
    r.ok(&r.main, &["lint", "--staged"]);
}

#[test]
fn an_edit_of_a_row_archived_on_the_trunk_is_refused_even_when_the_checkout_still_has_it() {
    let r = Repo::new("reopen-archived");
    r.ok(&r.main, &["add", "first"]);
    r.ok(&r.main, &["add", "second"]);
    r.ok(&r.main, &["done", "1", "--self"]);
    r.ok(&r.main, &["archive"]);
    let head = r.git(&r.main, &["rev-parse", "HEAD"]);
    let fix = "5w: #1 is archived; to reopen it, move its block from DONE.md back to TASKS.md unchanged and commit it as `chore(tasks): unarchive #1`, then `5w reopen 1`\n";
    // Archived in the checkout too: refused, naming the move back.
    assert_eq!(r.refuses(&r.main, &["reopen", "1"]), fix);
    // The checkout's copies still show #1 open in TASKS.md and not in DONE.md:
    // no edit may carry the row onto the trunk beside its archived copy.
    let tasks = r.tasks().replace("## Done", "- [ ] #1 first\n\n## Done");
    std::fs::write(r.main.join("TASKS.md"), tasks).unwrap();
    let done = r
        .git(&r.main, &["show", "HEAD:DONE.md"])
        .replace("- [x] #1 first via:self", "");
    std::fs::write(r.main.join("DONE.md"), done).unwrap();
    r.ok(&r.main, &["list"]);
    for args in [
        &["reopen", "1"][..],
        &["open", "1"],
        &["set", "1", "level", "2"],
    ] {
        assert_eq!(r.refuses(&r.main, args), fix, "{args:?}");
    }
    assert_eq!(r.git(&r.main, &["rev-parse", "HEAD"]), head);
    r.lint_history();
}

#[test]
fn an_unarchive_commit_moves_an_archived_row_back_to_be_reopened() {
    let r = Repo::new("unarchive");
    for t in ["a", "b", "c"] {
        r.ok(&r.main, &["add", t]);
    }
    r.ok(&r.main, &["done", "1", "--self"]);
    r.ok(&r.main, &["archive"]);
    r.ok(&r.main, &["done", "2", "--self"]);
    r.ok(&r.main, &["archive"]);
    r.ok(&r.main, &["hook", "install"]);
    let head = r.git(&r.main, &["rev-parse", "HEAD"]);
    assert!(
        r.refuses(&r.main, &["reopen", "2"])
            .contains("unarchive #2")
    );
    let git = |args: &[&str]| {
        let mut c = Command::new("git");
        c.args(args).current_dir(&r.main);
        env(&mut c, &r.root);
        let o = c.output().unwrap();
        (
            o.status.success(),
            String::from_utf8_lossy(&o.stderr).to_string(),
        )
    };
    // The move the refusal names: #2's block, as DONE.md has it, under TASKS.md's
    // `## Done` — which leaves both files as the trunk had them before the
    // second archive, so the content alone reads like a missed archive.
    let move_back = |row: &str| {
        let tasks = r
            .tasks()
            .replace("## Done\n\n", &format!("## Done\n\n{row}\n"));
        std::fs::write(r.main.join("TASKS.md"), tasks).unwrap();
        // `git` trims the output's last newline.
        let done = format!("{}\n", r.git(&r.main, &["show", "HEAD:DONE.md"]))
            .replace("- [x] #2 b via:self\n", "");
        std::fs::write(r.main.join("DONE.md"), done).unwrap();
    };
    let reset = || r.git(&r.main, &["reset", "-q", "--hard", &head]);

    // No checkout missed a commit, so the hook passes the move, staged or with
    // `commit -a`, and the unarchive subject passes once committed; #2 reopens.
    move_back("- [x] #2 b via:self");
    r.git(&r.main, &["add", "TASKS.md", "DONE.md"]);
    assert_eq!(
        r.git(
            &r.main,
            &["diff", "--cached", "HEAD~1", "--", "TASKS.md", "DONE.md"]
        ),
        ""
    );
    let (ok, err) = git(&["commit", "-qm", "chore(tasks): unarchive #2"]);
    assert!(ok, "{err}");
    r.ok(&r.main, &["lint", "HEAD"]);
    reset();
    move_back("- [x] #2 b via:self");
    let (ok, err) = git(&["commit", "-qam", "chore(tasks): unarchive #2"]);
    assert!(ok, "{err}");
    r.ok(&r.main, &["lint", "HEAD"]);
    r.ok(&r.main, &["reopen", "2"]);
    assert!(r.tasks().contains("- [ ] #2 b"), "{}", r.tasks());
    r.lint_history();
    reset();

    // Under another subject the same move is flagged once committed.
    move_back("- [x] #2 b via:self");
    let (ok, err) = git(&["commit", "-qam", "reopen b"]);
    assert!(ok, "{err}");
    let err = r.fails(&r.main, &["lint", "HEAD"]);
    assert!(
        err.contains("#2: archived in DONE.md, back in TASKS.md")
            && err.contains("`chore(tasks): unarchive #2`"),
        "{err}"
    );
    reset();

    // An unarchive commit that also edits the row is refused, by the hook and
    // once committed.
    move_back("- [x] #2 b, edited via:self");
    let (ok, err) = git(&["commit", "-qam", "chore(tasks): unarchive #2"]);
    assert!(!ok && err.contains("a closed row changed"), "{err}");
    let (ok, err) = git(&[
        "commit",
        "-qam",
        "chore(tasks): unarchive #2",
        "--no-verify",
    ]);
    assert!(ok, "{err}");
    let err = r.fails(&r.main, &["lint", "HEAD"]);
    assert!(
        err.contains("#2: an unarchive commit moves rows back unchanged"),
        "{err}"
    );
    reset();

    // An unarchive subject on a commit that moves no row is flagged too.
    std::fs::write(r.main.join("README"), "changed\n").unwrap();
    let (ok, err) = git(&["commit", "-qam", "chore(tasks): unarchive #2"]);
    assert!(ok, "{err}");
    let err = r.fails(&r.main, &["lint", "HEAD"]);
    assert!(
        err.contains("#2: named by an unarchive commit that does not move it"),
        "{err}"
    );
}

#[test]
fn an_archive_of_a_row_deleted_by_hand_gives_the_checkout_the_archived_row() {
    // #1 is closed on the trunk but deleted by hand from the checkout's queue,
    // working or staged: the commit archives it, and the checkout's DONE.md
    // gets the row too — whether the commit creates the file or changes it —
    // so no copy of the checkout would delete it from the trunk.
    for (name, on_trunk) in [
        ("archive-deleted-new", false),
        ("archive-deleted-old", true),
    ] {
        for stage in [false, true] {
            let r = Repo::new(name);
            r.ok(&r.main, &["add", "first"]);
            r.ok(&r.main, &["add", "second"]);
            r.ok(&r.main, &["add", "third"]);
            r.ok(&r.main, &["add", "fourth"]);
            if on_trunk {
                r.ok(&r.main, &["done", "3", "--self"]);
                r.ok(&r.main, &["archive"]);
            }
            // #4, closed after #1 and kept by the checkout, stays after it.
            r.ok(&r.main, &["done", "1", "--self"]);
            r.ok(&r.main, &["done", "4", "--self"]);
            let t = r
                .tasks()
                .replace("- [x] #1 first via:self\n", "")
                .replace("#2 second", "#2 second, edited");
            std::fs::write(r.main.join("TASKS.md"), t).unwrap();
            if stage {
                r.git(&r.main, &["add", "TASKS.md"]);
            }
            let out = r.ok(&r.main, &["archive"]);
            assert!(out.contains("  archived 2 → DONE.md"), "{out}");
            let head = r.git(&r.main, &["show", "HEAD:DONE.md"]);
            let (one, four) = (head.find("- [x] #1 first"), head.find("- [x] #4 fourth"));
            assert!(one.is_some() && one < four, "{head}");
            let done = std::fs::read_to_string(r.main.join("DONE.md")).unwrap();
            assert_eq!(done.trim_end(), head, "{out}");
            assert_eq!(r.git(&r.main, &["show", ":DONE.md"]), head);
            assert!(r.tasks().contains("#2 second, edited"));
            let status = r.git(&r.main, &["status", "--porcelain"]);
            let want = if stage { "M  TASKS.md" } else { " M TASKS.md" };
            assert_eq!(status, want, "{out}");
            r.ok(&r.main, &["lint", "--staged"]);
            r.lint_history();
        }
    }
}

#[test]
fn an_archive_with_every_trunk_closed_row_reopened_commits_nothing() {
    let r = Repo::new("archive-all-reopened");
    r.ok(&r.main, &["add", "first"]);
    r.ok(&r.main, &["done", "1", "--self"]);
    let head = r.git(&r.main, &["rev-parse", "HEAD"]);
    let reopen = |t: &str| t.replace("- [x] #1 first via:self", "- [ ] #1 first");

    // Reopened in the working copy only, then staged: neither run commits,
    // and DONE.md appears in no copy.
    std::fs::write(r.main.join("TASKS.md"), reopen(&r.tasks())).unwrap();
    for stage in [false, true] {
        if stage {
            r.git(&r.main, &["add", "TASKS.md"]);
        }
        let out = r.ok(&r.main, &["archive"]);
        assert!(out.contains("  note: #1 is closed on main"), "{out}");
        assert!(out.contains("  nothing closed on main to archive"), "{out}");
        assert_eq!(r.git(&r.main, &["rev-parse", "HEAD"]), head, "{out}");
        assert!(!r.main.join("DONE.md").exists());
        assert!(r.git(&r.main, &["ls-files", "DONE.md"]).is_empty());
        let status = r.git(&r.main, &["status", "--porcelain"]);
        let want = if stage { "M  TASKS.md" } else { " M TASKS.md" };
        assert_eq!(status, want);
        r.ok(&r.main, &["lint", "--staged"]);
    }

    // Reopened only in the index, the working copy still closed: skipped the same.
    r.git(&r.main, &["checkout", "--", "TASKS.md"]);
    std::fs::write(
        r.main.join("TASKS.md"),
        r.git(&r.main, &["show", "HEAD:TASKS.md"]) + "\n",
    )
    .unwrap();
    let out = r.ok(&r.main, &["archive"]);
    assert!(out.contains("  note: #1 is closed on main"), "{out}");
    assert_eq!(r.git(&r.main, &["rev-parse", "HEAD"]), head, "{out}");
    assert!(r.git(&r.main, &["ls-files", "DONE.md"]).is_empty());
    r.ok(&r.main, &["lint", "--staged"]);
}

#[test]
fn an_archive_with_nothing_closed_on_the_trunk_commits_nothing() {
    let r = Repo::new("archive-working-only");
    r.ok(&r.main, &["add", "first"]);
    let root = r.git(&r.main, &["rev-list", "--max-parents=0", "HEAD"]);
    let head = r.git(&r.main, &["rev-parse", "HEAD"]);

    // #1 is closed only in the working copy: the trunk has nothing closed, so
    // no commit (not even DONE.md's header) and no claim that anything was archived.
    let t = r
        .tasks()
        .replace("- [ ] #1 first", "- [x] #1 first via:self");
    std::fs::write(r.main.join("TASKS.md"), t).unwrap();
    let out = r.ok(&r.main, &["archive"]);
    assert_eq!(r.git(&r.main, &["rev-parse", "HEAD"]), head, "{out}");
    assert!(!out.contains("archived"), "{out}");
    assert!(out.contains("  nothing closed on main to archive"), "{out}");
    assert!(out.contains("  checkout updated: #1"), "{out}");
    r.ok(&r.main, &["lint", "--staged"]);
    r.ok(&r.main, &["lint", &format!("{root}..main")]);
}

#[test]
fn an_archive_counts_only_the_rows_closed_on_the_trunk() {
    let r = Repo::new("archive-count");
    r.ok(&r.main, &["add", "first"]);
    r.ok(&r.main, &["add", "second"]);
    r.ok(&r.main, &["done", "1", "--self"]);
    let t = r
        .tasks()
        .replace("- [ ] #2 second", "- [x] #2 second via:self");
    std::fs::write(r.main.join("TASKS.md"), t).unwrap();
    let out = r.ok(&r.main, &["archive"]);
    assert!(out.contains("archive 1 closed tasks"), "{out}");
    assert!(out.contains("  archived 1 → DONE.md"), "{out}");
    let done = r.git(&r.main, &["show", "HEAD:DONE.md"]);
    assert!(
        done.contains("#1 first") && !done.contains("#2 second"),
        "{done}"
    );
    r.lint_history();
}

#[test]
fn a_closed_row_is_immutable_but_may_be_reflowed_or_archived() {
    let r = Repo::new("immutable");
    r.ok(&r.main, &["add", "One sentence here. And a second sentence that is long enough to push well past the title limit of the queue for sure."]);
    r.ok(&r.main, &["done", "1", "--self"]);
    hand_edit(&r, "- [x] #1 One", "- [x] #1 Uno");
    assert!(r.fails(&r.main, &["lint"]).contains("closed row changed"));
    r.git(&r.main, &["checkout", "--", "."]);
    r.git(&r.main, &["reset", "-q", "--hard"]);
    r.ok(&r.main, &["split", "--all"]);
    r.ok(&r.main, &["archive"]);
    r.lint_history();
}

#[test]
fn the_hook_blocks_a_bad_hand_edit_and_warns_without_the_binary() {
    let r = Repo::new("hook");
    r.ok(&r.main, &["hook", "install"]);
    r.ok(&r.main, &["add", "x"]);
    hand_edit(&r, "- [ ] #1 x", "- [x] #1 x");
    let bin = std::path::Path::new(&bin5w())
        .parent()
        .unwrap()
        .to_path_buf();
    let commit = |path: String| {
        let mut c = Command::new("git");
        c.args(["commit", "-qm", "bad"]).current_dir(&r.main);
        env(&mut c, &r.root);
        c.env("PATH", path);
        c.output().unwrap()
    };
    let with = commit(format!("{}:/usr/bin:/bin", bin.display()));
    assert!(!with.status.success());
    assert!(String::from_utf8_lossy(&with.stderr).contains("closed without via"));
    let without = commit("/usr/bin:/bin".into());
    assert!(without.status.success());
    assert!(String::from_utf8_lossy(&without.stderr).contains("Follow PROTOCOL.md"));
    // And range lint catches what the missing binary let through.
    assert!(
        r.fails(&r.main, &["lint", "HEAD"])
            .contains("closed without via")
    );
    assert!(r.main.join("PROTOCOL.md").exists());
}

#[test]
fn the_hook_lints_what_commit_a_and_commit_path_are_committing() {
    // git hands the hook a temporary index (GIT_INDEX_FILE) for these: the
    // commit is that index, not .git/index.
    let r = Repo::new("hook-index");
    r.ok(&r.main, &["hook", "install"]);
    r.ok(&r.main, &["add", "x"]);
    let bad = r.tasks().replace("- [ ] #1 x", "- [x] #1 x");
    let commit = |args: &[&str]| {
        let mut c = Command::new("git");
        c.args(args).current_dir(&r.main);
        env(&mut c, &r.root);
        c.output().unwrap()
    };
    std::fs::write(r.main.join("TASKS.md"), &bad).unwrap();
    let o = commit(&["commit", "-qam", "chore(tasks): bad"]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        !o.status.success() && err.contains("closed without via"),
        "{err}"
    );

    // Only TASKS.md is committed; the staged README is not, and is no queue edit.
    std::fs::write(r.main.join("README"), "changed\n").unwrap();
    r.git(&r.main, &["add", "README"]);
    let o = commit(&["commit", "-qm", "chore(tasks): bad", "TASKS.md"]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        !o.status.success() && err.contains("closed without via"),
        "{err}"
    );

    // A good edit committed the same ways goes through.
    let good = r.tasks().replace("- [x] #1 x", "- [x] #1 x via:self");
    std::fs::write(r.main.join("TASKS.md"), &good).unwrap();
    let o = commit(&["commit", "-qm", "chore(tasks): done #1", "TASKS.md"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(
        r.git(&r.main, &["diff", "--cached", "--name-only"]),
        "README"
    );
}

#[test]
fn a_hook_in_a_linked_worktree_leaves_every_index_alone() {
    // git sets GIT_DIR (the worktree's gitdir) and GIT_INDEX_FILE for a hook in a
    // linked worktree; 5w's calls in other worktrees must not inherit them.
    let r = Repo::new("hook-wt");
    r.ok(&r.main, &["add", "one"]);
    r.ok(&r.main, &["wt", "new", "a/x"]);
    let wt = r.wt("a/x");
    std::fs::write(
        r.main.join(".git/hooks/post-commit"),
        "#!/bin/sh\n[ \"$(git symbolic-ref --short HEAD)\" = a/x ] && exec 5w submit 1\nexit 0\n",
    )
    .unwrap();
    std::fs::set_permissions(
        r.main.join(".git/hooks/post-commit"),
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();
    r.commit_in(&wt, "f", "1\n");
    assert!(r.line(1).starts_with("- [~] #1"), "{}", r.tasks());
    assert_eq!(r.git(&r.main, &["status", "--porcelain"]), "");
    assert_eq!(r.git(&wt, &["status", "--porcelain"]), "");
}

#[test]
fn a_git_dir_naming_another_repository_is_refused() {
    let r = Repo::new("gitdir");
    let other = r.root.join("other");
    std::fs::create_dir_all(&other).unwrap();
    r.git(&other, &["init", "-q", "-b", "main"]);
    let run = |k: &str, v: &Path, cwd: &Path| {
        let mut c = Command::new(bin5w());
        c.args(["ready"]).current_dir(cwd);
        env(&mut c, &r.root);
        c.env(k, v);
        c.output().unwrap()
    };
    let err = refusal(&run("GIT_DIR", &other.join(".git"), &r.main), &["ready"]);
    assert!(err.contains("unset GIT_DIR"), "{err}");
    let err = refusal(&run("GIT_WORK_TREE", &other, &r.main), &["ready"]);
    assert!(err.contains("unset GIT_WORK_TREE"), "{err}");
    // Naming the repository it runs in, relative or not, is no conflict.
    assert!(run("GIT_DIR", Path::new(".git"), &r.main).status.success());
    assert!(
        run("GIT_DIR", &r.main.join(".git"), &r.main)
            .status
            .success()
    );
}

// --- ci: forge-neutral checks ---------------------------------------------------------

fn path_with_5w() -> String {
    let bin = std::path::Path::new(&bin5w())
        .parent()
        .unwrap()
        .to_path_buf();
    format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    )
}

impl Repo {
    fn git_path(&self, cwd: &Path, path: &str, args: &[&str]) -> Output {
        let mut c = Command::new("git");
        c.args(args).current_dir(cwd);
        env(&mut c, &self.root);
        c.env("PATH", path);
        c.output().unwrap()
    }
}

#[test]
fn pre_receive_accepts_the_protocol_and_rejects_the_rest() {
    pre_receive_checks_pushes_to("server.git");
}

#[test]
fn pre_receive_reads_the_quarantine_of_a_repository_whose_path_holds_a_colon() {
    // git C-quotes such a path in GIT_ALTERNATE_OBJECT_DIRECTORIES.
    pre_receive_checks_pushes_to("q:x/server.git");
}

fn pre_receive_checks_pushes_to(name: &str) {
    let r = Repo::new("prereceive");
    let server = r.root.join(name);
    r.git(
        &r.root,
        &[
            "clone",
            "-q",
            "--bare",
            r.main.to_str().unwrap(),
            server.to_str().unwrap(),
        ],
    );
    r.ok(&server, &["hook", "install", "pre-receive"]);
    r.git(
        &r.main,
        &["remote", "add", "origin", server.to_str().unwrap()],
    );
    let push = |args: &[&str]| {
        r.git_path(
            &r.main,
            &path_with_5w(),
            &[&["push", "-q", "origin"], args].concat(),
        )
    };

    // Tool-made queue commits go through.
    r.ok(&r.main, &["add", "one"]);
    r.ok(&r.main, &["done", "1", "--self"]);
    assert!(push(&["main"]).status.success());

    // A hand edit breaking the protocol is refused at the server.
    let t = r
        .tasks()
        .replace("- [x] #1 one via:self", "- [ ] #1 one via:self");
    std::fs::write(r.main.join("TASKS.md"), t).unwrap();
    r.git(&r.main, &["commit", "-qam", "chore(tasks): bad"]);
    let out = push(&["main"]);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("reopened, but still carries"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    r.git(&r.main, &["reset", "-q", "--hard", "HEAD~1"]);

    // A branch carrying a queue edit is refused; a clean branch is not.
    r.git(&r.main, &["checkout", "-qb", "f/a"]);
    std::fs::write(r.main.join("code"), "x\n").unwrap();
    r.git(&r.main, &["add", "code"]);
    r.git(&r.main, &["commit", "-qm", "code"]);
    assert!(push(&["f/a"]).status.success());
    let t = r.tasks().replace(
        "- [x] #1 one via:self",
        "- [x] #1 one via:self\n- [ ] #2 sneaked",
    );
    std::fs::write(r.main.join("TASKS.md"), t).unwrap();
    r.git(&r.main, &["commit", "-qam", "queue on a branch"]);
    let out = push(&["f/a"]);
    assert!(String::from_utf8_lossy(&out.stderr).contains("queue edits go on main"));
    assert!(!out.status.success());

    // No 5w on the server: refused, not waved through.
    let out = r.git_path(
        &r.main,
        "/usr/bin:/bin",
        &["push", "-q", "origin", "HEAD~1:refs/heads/f/b"],
    );
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("not installed on the server"));

    // A branch that merged the trunk brings its queue commits along: those are the
    // trunk's, not the branch's. Its own queue edit after the merge is still refused.
    r.git(&r.main, &["reset", "-q", "--hard", "HEAD~1"]);
    r.git(&r.main, &["checkout", "-q", "main"]);
    r.ok(&r.main, &["add", "two"]);
    assert!(push(&["main"]).status.success());
    r.git(&r.main, &["checkout", "-q", "f/a"]);
    r.git(&r.main, &["merge", "-q", "--no-edit", "main"]);
    let out = push(&["f/a"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let t = r.tasks().replace("#2 two", "#2 two sneaked");
    std::fs::write(r.main.join("TASKS.md"), t).unwrap();
    r.git(&r.main, &["commit", "-qam", "queue on a branch"]);
    let out = push(&["f/a"]);
    assert!(String::from_utf8_lossy(&out.stderr).contains("queue edits go on main"));
    assert!(!out.status.success());

    // A branch that merged a trunk tip the server lacks is judged against the server's
    // trunk, even when the same push moves the trunk: push the trunk first.
    r.git(&r.main, &["reset", "-q", "--hard", "HEAD~1"]);
    r.git(&r.main, &["checkout", "-q", "main"]);
    r.ok(&r.main, &["add", "three"]);
    r.git(&r.main, &["checkout", "-q", "f/a"]);
    r.git(&r.main, &["merge", "-q", "--no-edit", "main"]);
    let out = push(&["main", "f/a"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("queue edits go on main"));
    assert!(push(&["main"]).status.success());
    let out = push(&["f/a"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn gate_trunk_lets_only_recorded_landings_of_reviewed_changes_onto_the_trunk() {
    let r = Repo::new("gate-trunk");
    let server = r.root.join("server.git");
    r.git(
        &r.root,
        &[
            "clone",
            "-q",
            "--bare",
            r.main.to_str().unwrap(),
            server.to_str().unwrap(),
        ],
    );
    r.ok(&server, &["hook", "install", "pre-receive"]);
    r.git(
        &r.main,
        &["remote", "add", "origin", server.to_str().unwrap()],
    );
    let push = |args: &[&str]| {
        let o = r.git_path(
            &r.main,
            &path_with_5w(),
            &[&["push", "-q", "origin"], args].concat(),
        );
        (
            o.status.success(),
            String::from_utf8_lossy(&o.stderr).to_string(),
        )
    };
    let code = |file: &str| {
        std::fs::write(r.main.join(file), "x\n").unwrap();
        r.git(&r.main, &["add", file]);
        r.git(&r.main, &["commit", "-qm", &format!("code {file}")]);
    };

    // History from before the gate, and the commit that enables it, pass as they are.
    r.git(&r.main, &["branch", "early"]);
    code("before.txt");
    let cfg = std::fs::read_to_string(r.main.join(".5w.toml")).unwrap();
    assert!(cfg.contains("gate_trunk = false"), "{cfg}");
    std::fs::write(
        r.main.join(".5w.toml"),
        cfg.replace("gate_trunk = false", "gate_trunk = true"),
    )
    .unwrap();
    r.git(&r.main, &["commit", "-qam", "gate the trunk"]);
    let (ok, err) = push(&["main"]);
    assert!(ok, "{err}");

    // Code pushed straight to the trunk is refused; so is turning the gate off.
    code("straight.txt");
    let (ok, err) = push(&["main"]);
    assert!(!ok && err.contains("no landing record covers"), "{err}");
    r.git(&r.main, &["reset", "-q", "--hard", "HEAD~1"]);
    std::fs::write(r.main.join(".5w.toml"), &cfg).unwrap();
    r.git(&r.main, &["commit", "-qam", "ungate"]);
    let (ok, err) = push(&["main"]);
    assert!(!ok && err.contains("no landing record covers"), "{err}");
    r.git(&r.main, &["reset", "-q", "--hard", "HEAD~1"]);
    // A branch from before the gate that merged the trunk is still new to the trunk.
    r.git(&r.main, &["checkout", "-q", "early"]);
    code("early.txt");
    r.git(&r.main, &["merge", "-q", "--no-edit", "main"]);
    let (ok, err) = push(&["early:main"]);
    assert!(!ok && err.contains("no landing record covers"), "{err}");
    r.git(&r.main, &["checkout", "-q", "main"]);

    // Two tasks shipped, one rebased and one squashed, pushed at once with their accepts.
    r.ok(&r.main, &["add", "one"]);
    r.ok(&r.main, &["add", "two"]);
    r.ok(&r.main, &["wt", "new", "f/one"]);
    r.commit_in(&r.wt("f/one"), "one.txt", "1\n");
    r.ok(&r.main, &["wt", "new", "f/two"]);
    let two = r.wt("f/two");
    r.commit_in(&two, "two.txt", "2\n");
    r.commit_in(&two, "two-more.txt", "2\n");
    // Pushed for review: the server has its reviewed commit.
    let (ok, err) = push(&["main"]);
    assert!(ok, "{err}");
    let (ok, err) = push(&["f/two"]);
    assert!(ok, "{err}");
    r.ok(&r.main, &["submit", "1", "f/one"]);
    r.ok(&r.main, &["submit", "2", "f/two"]);
    r.ok(&r.main, &["accept", "1", "2"]);
    let accepted = r.git(&r.main, &["rev-parse", "HEAD"]);
    let out = r.ok(&r.main, &["ship", "f/one", "--sync"]);
    assert!(out.contains("landing of #1 recorded"), "{out}");
    assert!(out.contains("refs/5w/reviewed/1"), "{out}");
    let out = r.ok(&r.main, &["ship", "f/two", "--sync", "--squash"]);
    assert!(out.contains("landing of #2 recorded"), "{out}");
    assert_eq!(r.git(&r.main, &["status", "--porcelain"]), "");
    r.lint_history();

    // #1 was rebased and never pushed: the server cannot recompute its review.
    let (ok, err) = push(&["main"]);
    assert!(!ok && err.contains("refs/5w/reviewed/1"), "{err}");
    let reviewed = r
        .line(1)
        .split_whitespace()
        .find_map(|w| w.strip_prefix("reviewed:"))
        .unwrap()
        .to_string();
    let (ok, err) = push(&["main", &format!("{reviewed}:refs/5w/reviewed/1")]);
    assert!(ok, "{err}");

    // A record cannot reach back over landings the trunk already has: from before
    // both, "trunk + #1" is #1's change, and would cover taking #2 back out.
    let pushed = r.git(&r.main, &["rev-parse", "HEAD"]);
    r.git(&r.main, &["rm", "-q", "two.txt", "two-more.txt"]);
    r.git(&r.main, &["commit", "-qm", "roll back two"]);
    let tip = r.git(&r.main, &["rev-parse", "HEAD"]);
    r.git(
        &r.main,
        &[
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            &format!("chore(tasks): land #1\n\nLanded: {accepted}..{tip}"),
        ],
    );
    let (ok, err) = push(&["main"]);
    assert!(
        !ok && err.contains("already on main") && err.contains("no landing record covers"),
        "{err}"
    );
    r.git(&r.main, &["reset", "-q", "--hard", &pushed]);

    // A record naming an accepted task over code it did not accept covers nothing.
    let base = r.git(&r.main, &["rev-parse", "HEAD"]);
    code("forged.txt");
    let tip = r.git(&r.main, &["rev-parse", "HEAD"]);
    r.git(
        &r.main,
        &[
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            &format!("chore(tasks): land #1\n\nLanded: {base}..{tip}"),
        ],
    );
    let (ok, err) = push(&["main"]);
    assert!(
        !ok && err.contains("land #1 covers nothing") && err.contains("no landing record covers"),
        "{err}"
    );
    r.git(&r.main, &["reset", "-q", "--hard", &base]);

    // A merge that adds nothing of its own passes when what it brings in is covered.
    r.ok(&r.main, &["add", "three"]);
    r.ok(&r.main, &["wt", "new", "f/three"]);
    r.commit_in(&r.wt("f/three"), "three.txt", "3\n");
    let (ok, err) = push(&["main"]);
    assert!(ok, "{err}");
    let (ok, err) = push(&["f/three"]);
    assert!(ok, "{err}");
    r.ok(&r.main, &["submit", "3", "f/three"]);
    r.ok(&r.main, &["accept", "3"]);
    let accepted3 = r.git(&r.main, &["rev-parse", "HEAD"]);
    r.ok(&r.main, &["ship", "f/three", "--sync"]);
    let landed = r.git(&r.main, &["rev-parse", "HEAD"]);
    r.git(&r.main, &["reset", "-q", "--hard", &accepted3]);
    r.ok(&r.main, &["add", "four"]);
    r.git(&r.main, &["merge", "-q", "--no-edit", &landed]);
    let (ok, err) = push(&["main"]);
    assert!(ok, "{err}");

    // Deleting the trunk, to push an orphan history in its place, is refused; a branch is not.
    r.git(&server, &["config", "receive.denyDeleteCurrent", "false"]);
    let (ok, err) = push(&[":main"]);
    assert!(!ok && err.contains("refused under gate_trunk"), "{err}");
    assert!(
        !r.git(&server, &["rev-parse", "--verify", "main"])
            .is_empty()
    );
    let (ok, err) = push(&[":f/two"]);
    assert!(ok, "{err}");

    // A merge of an unreviewed branch brings code no record covers.
    r.git(&r.main, &["checkout", "-qb", "side"]);
    code("side.txt");
    r.git(&r.main, &["checkout", "-q", "main"]);
    r.git(&r.main, &["merge", "-q", "--no-ff", "--no-edit", "side"]);
    let (ok, err) = push(&["main"]);
    assert!(!ok && err.contains("no landing record covers"), "{err}");
}

#[test]
fn pre_receive_does_not_let_a_branch_land_on_a_trunk_update_git_refuses() {
    // git runs pre-receive before its own per-ref checks, and a push that is not
    // atomic applies the refs that pass: judged against the pushed trunk, a branch
    // that merged it would land its queue commits while the trunk update is refused.
    for refuse in ["deny-non-fast-forward", "update-hook"] {
        let r = Repo::new("prereceive-refused-trunk");
        r.ok(&r.main, &["add", "one"]);
        let server = r.root.join("server.git");
        r.git(
            &r.root,
            &[
                "clone",
                "-q",
                "--bare",
                r.main.to_str().unwrap(),
                server.to_str().unwrap(),
            ],
        );
        r.ok(&server, &["hook", "install", "pre-receive"]);
        r.git(
            &r.main,
            &["remote", "add", "origin", server.to_str().unwrap()],
        );
        let mut update = "+main";
        if refuse == "deny-non-fast-forward" {
            r.git(&server, &["config", "receive.denyNonFastForwards", "true"]);
            // Rewrite main's history: the server's main is no ancestor of the new one.
            r.git(&r.main, &["reset", "-q", "--hard", "HEAD~1"]);
            r.ok(&r.main, &["add", "uno"]);
        } else {
            let hook = server.join("hooks/update");
            std::fs::write(
                &hook,
                "#!/bin/sh\n[ \"$1\" = refs/heads/main ] && exit 1\nexit 0\n",
            )
            .unwrap();
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
            update = "main";
        }
        r.ok(&r.main, &["add", "two"]);
        r.git(&r.main, &["checkout", "-qb", "f/b"]);
        let out = r.git_path(
            &r.main,
            &path_with_5w(),
            &["push", "-q", "origin", update, "f/b"],
        );
        assert!(!out.status.success(), "{refuse}");
        assert!(
            r.git(&server, &["branch", "--list", "f/b"]).is_empty(),
            "{refuse}: f/b landed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[test]
fn object_directories_outside_a_push_quarantine_are_not_inherited() {
    // Only receive-pack's quarantine — a directory in the repository's own object
    // store, which git names as the alternate — keeps GIT_OBJECT_DIRECTORY.
    let r = Repo::new("quarantine");
    r.ok(&r.main, &["add", "one"]);
    let stray = r.root.join("stray-objects");
    let elsewhere = r.root.join("elsewhere");
    std::fs::create_dir_all(&stray).unwrap();
    std::fs::create_dir_all(&elsewhere).unwrap();
    let mut c = Command::new(bin5w());
    c.args(["lint", "HEAD"]).current_dir(&r.main);
    env(&mut c, &r.root);
    c.env("GIT_OBJECT_DIRECTORY", &stray)
        .env("GIT_QUARANTINE_PATH", &stray)
        .env("GIT_ALTERNATE_OBJECT_DIRECTORIES", &elsewhere);
    let o = c.output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
}

#[test]
fn pre_receive_takes_sha256_zeros_as_a_new_or_deleted_ref() {
    // SHA-256 names are 64 hex digits: a create or delete carries 64 zeros, not 40.
    let r = Repo::new("prereceive-sha256");
    let src = r.root.join("sha256");
    std::fs::create_dir_all(&src).unwrap();
    r.git(
        &src,
        &["init", "-q", "-b", "main", "--object-format=sha256"],
    );
    std::fs::write(src.join("README"), "hi\n").unwrap();
    r.git(&src, &["add", "README"]);
    r.git(&src, &["commit", "-qm", "init"]);
    r.ok(&src, &["init"]);
    let server = r.root.join("server256.git");
    r.git(
        &r.root,
        &[
            "clone",
            "-q",
            "--bare",
            src.to_str().unwrap(),
            server.to_str().unwrap(),
        ],
    );
    r.ok(&server, &["hook", "install", "pre-receive"]);
    r.git(&src, &["remote", "add", "origin", server.to_str().unwrap()]);
    let push = |args: &[&str]| {
        let out = r.git_path(
            &src,
            &path_with_5w(),
            &[&["push", "-q", "origin"], args].concat(),
        );
        assert!(
            out.status.success(),
            "push {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };

    // A new branch (old sha all zeros), then its deletion (new sha all zeros).
    r.git(&src, &["checkout", "-qb", "f/a"]);
    std::fs::write(src.join("code"), "x\n").unwrap();
    r.git(&src, &["add", "code"]);
    r.git(&src, &["commit", "-qm", "code"]);
    push(&["f/a"]);
    push(&[":f/a"]);
}

#[test]
fn ci_refuses_a_revision_that_is_not_a_commit_in_one_line() {
    let r = Repo::new("ci-revs");
    r.ok(&r.main, &["add", "x"]);
    r.ok(&r.main, &["add", "y"]);
    // `^HEAD` resolves to `^<sha>`: as --head it once checked nothing and passed.
    for rev in ["^HEAD", "nope", "HEAD~1..HEAD"] {
        assert_eq!(
            r.fails(&r.main, &["ci", "--head", rev]),
            format!("5w: ci: --head {rev} is not a commit — pass a branch, tag or sha\n"),
            "{rev}"
        );
        assert_eq!(
            r.fails(&r.main, &["ci", "--base", rev]),
            format!(
                "5w: ci: --base {rev} is not a commit — pass a branch, tag or sha, with full history fetched\n"
            ),
            "{rev}"
        );
        // A bad --trunk once read an empty queue: with require_task off, an unreviewed pass.
        assert_eq!(
            r.fails(&r.main, &["ci", "--branch", "x", "--trunk", rev]),
            format!("5w: ci: --trunk {rev} is not a commit — pass a branch, tag or sha\n"),
            "{rev}"
        );
    }
    // Plain shas and a new ref's all-zeros base still resolve.
    let (head, prev) = (
        r.git(&r.main, &["rev-parse", "HEAD"]),
        r.git(&r.main, &["rev-parse", "HEAD~1"]),
    );
    let push = ["--ref", "refs/heads/main", "--head", &head];
    let out = r.ok(&r.main, &[&["ci", "--base", &prev][..], &push].concat());
    assert!(out.contains("1 commit(s)"), "{out}");
    r.ok(
        &r.main,
        &[&["ci", "--base", &"0".repeat(40)][..], &push].concat(),
    );
}

#[test]
fn ci_push_of_a_branch_that_merged_the_trunk_judges_only_its_own_commits() {
    let r = Repo::new("ci-merged-trunk");
    r.ok(&r.main, &["add", "x"]);
    r.git(&r.main, &["branch", "f/a"]);
    let old = r.git(&r.main, &["rev-parse", "f/a"]);
    r.ok(&r.main, &["add", "y"]);
    r.ok(&r.main, &["done", "1", "--self"]);
    r.git(&r.main, &["checkout", "-q", "f/a"]);
    std::fs::write(r.main.join("code"), "x\n").unwrap();
    r.git(&r.main, &["add", "code"]);
    r.git(&r.main, &["commit", "-qm", "code"]);
    r.git(&r.main, &["merge", "-q", "--no-edit", "main"]);
    let push = |r: &Repo| {
        r.cli(
            &r.main,
            &[
                "ci",
                "--base",
                &old,
                "--head",
                "f/a",
                "--ref",
                "refs/heads/f/a",
            ],
        )
    };
    let o = push(&r);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(String::from_utf8_lossy(&o.stdout).contains("2 commit(s)"));

    hand_edit(&r, "- [ ] #2 y", "- [ ] #2 y sneaked");
    r.git(&r.main, &["commit", "-qm", "queue on a branch"]);
    let o = push(&r);
    assert!(!o.status.success());
    let err = String::from_utf8_lossy(&o.stderr);
    assert_eq!(err.matches("queue edits go on main").count(), 1, "{err}");
}

#[test]
fn ci_plain_range_judges_every_commit_the_trunk_holds_too() {
    // No --ref or --branch: no branch to set the trunk's commits apart from.
    let r = Repo::new("ci-plain-range");
    r.ok(&r.main, &["add", "x"]);
    let old = r.git(&r.main, &["rev-parse", "HEAD"]);
    std::fs::write(r.main.join("code"), "x\n").unwrap();
    r.git(&r.main, &["add", "code"]);
    r.git(&r.main, &["commit", "-qm", "code"]);
    let o = r.cli(&r.main, &["ci", "--base", &old, "--head", "main"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(
        String::from_utf8_lossy(&o.stdout),
        "5w ci: 1 commit(s), range: ok\n"
    );
    r.ok(&r.main, &["add", "y"]);
    let o = r.cli(&r.main, &["ci", "--base", &old, "--head", "main"]);
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("queue edits go on main"));
}

#[test]
fn ci_branch_mode_is_the_ship_check() {
    let r = Repo::new("cibranch");
    r.ok(&r.main, &["add", "x", "branch:a/x"]);
    r.ok(&r.main, &["wt", "new", "a/x"]);
    let wt = r.wt("a/x");
    r.commit_in(&wt, "f", "1\n");
    let base = r.git(&r.main, &["merge-base", "main", "a/x"]);
    let ci = |r: &Repo| {
        r.cli(
            &r.main,
            &["ci", "--base", &base, "--head", "a/x", "--branch", "a/x"],
        )
    };
    let text = |o: Output| {
        format!(
            "{}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        )
    };

    let o = ci(&r);
    assert!(!o.status.success());
    assert!(text(o).contains("not accepted yet"));

    r.ok(&wt, &["submit", "1"]);
    r.ok(&r.main, &["accept", "1"]);
    let o = ci(&r);
    assert!(o.status.success(), "{}", text(o));
    let o = r.cli(
        &r.main,
        &["ci", "--head", "a/x", "--branch", "a/x", "--trunk", "main"],
    );
    assert!(o.status.success(), "{}", text(o));

    // Rebased onto the moved trunk: still the reviewed change.
    r.git(&wt, &["rebase", "-q", "main"]);
    let o = r.cli(&r.main, &["ci", "--head", "a/x", "--branch", "a/x"]);
    assert!(o.status.success(), "{}", text(o));

    r.commit_in(&wt, "f", "sneaky\n");
    let o = r.cli(&r.main, &["ci", "--head", "a/x", "--branch", "a/x"]);
    assert!(!o.status.success());
    assert!(text(o).contains("not the change accepted"));
}

#[test]
fn ci_branch_mode_flags_a_queue_symlinked_on_the_trunk() {
    let r = Repo::new("cibranch-link");
    r.ok(&r.main, &["add", "x", "branch:a/x"]);
    r.ok(&r.main, &["wt", "new", "a/x"]);
    r.commit_in(&r.wt("a/x"), "f", "1\n");
    link_queue(&r);
    let o = r.cli(&r.main, &["ci", "--head", "a/x", "--branch", "a/x"]);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(!o.status.success(), "{text}");
    assert!(text.contains("TASKS.md is a symlink on main"), "{text}");
    assert!(!text.contains("no task names"), "{text}");
}

/// A CI job's clone of a forge: detached at the change request, the trunk fetched.
fn ci_clone(r: &Repo, forge: &Path, name: &str) -> PathBuf {
    let dir = r.root.join(name);
    r.git(
        &r.root,
        &[
            "clone",
            "-q",
            forge.to_str().unwrap(),
            dir.to_str().unwrap(),
        ],
    );
    r.git(&dir, &["checkout", "-q", "--detach", "origin/a/x"]);
    dir
}

#[test]
fn ci_events_submit_and_accept_a_change_request() {
    let r = Repo::new("cievents");
    r.ok(&r.main, &["add", "x", "branch:a/x"]);
    r.ok(&r.main, &["wt", "new", "a/x"]);
    let wt = r.wt("a/x");
    r.commit_in(&wt, "f", "1\n");
    let forge = r.root.join("forge.git");
    let fp = forge.to_str().unwrap();
    r.git(
        &r.root,
        &["clone", "-q", "--bare", r.main.to_str().unwrap(), fp],
    );
    let ci = ci_clone(&r, &forge, "ci");
    let tip = |d: &Path, rev: &str| r.git(d, &["rev-parse", rev]);
    let row = |d: &Path| {
        r.git(d, &["show", "main:TASKS.md"])
            .lines()
            .find(|l| l.contains("] #1 "))
            .unwrap()
            .to_string()
    };
    let submit = ["ci", "--event", "submit", "--branch", "a/x"];

    // The trunk only as origin's: an event has nowhere to commit, and says how to fix it.
    r.git(&ci, &["branch", "-qD", "main"]);
    assert!(
        r.refuses(&ci, &submit)
            .contains("`git fetch origin +main:main` first")
    );
    r.git(&ci, &["fetch", "-q", "origin", "+main:main"]);
    let b = ci_clone(&r, &forge, "ci-b");

    let head = tip(&ci, "origin/a/x");
    let out = r.ok(&ci, &submit);
    assert!(out.contains("#1 submitted — a/x at"), "{out}");
    assert!(row(&ci).starts_with("- [~] #1") && row(&ci).ends_with(&format!(" submitted:{head}")));
    // A re-run is a no-op, not an error, and commits nothing.
    let before = tip(&ci, "main");
    assert!(r.ok(&ci, &submit).contains("#1 already submitted at"));
    assert_eq!(tip(&ci, "main"), before);
    r.git(&ci, &["push", "-q", "origin", "main"]);

    // A second job raced it: its push is refused; it re-fetches and re-runs, a no-op.
    // (A second later: in the same second both jobs would write the identical commit.)
    let mut c = Command::new(bin5w());
    c.args(submit).current_dir(&b);
    env(&mut c, &r.root);
    assert!(
        c.env("GIT_COMMITTER_DATE", "2001-01-01T00:00:00Z")
            .status()
            .unwrap()
            .success()
    );
    let o = Command::new("git")
        .args(["push", "-q", "origin", "main"])
        .current_dir(&b)
        .output()
        .unwrap();
    assert!(!o.status.success(), "the racing push must not fast-forward");
    r.git(&b, &["fetch", "-q", "origin", "+main:main"]);
    assert!(r.ok(&b, &submit).contains("already submitted"));
    assert_eq!(tip(&b, "main"), before);

    // Rejected; the job re-run at the rejected head does not resubmit it.
    r.ok(&ci, &["reject", "1", "no"]);
    let out = r.ok(&ci, &submit);
    assert!(out.contains("#1 was rejected at"), "{out}");
    assert!(row(&ci).starts_with("- [ ] #1"));
    // A new commit on the change request submits it again.
    r.commit_in(&wt, "f", "2\n");
    r.git(&r.main, &["push", "-q", fp, "a/x"]);
    r.git(&ci, &["fetch", "-q", "origin"]);
    let old = head;
    let head = tip(&ci, "origin/a/x");
    assert!(r.ok(&ci, &submit).contains("#1 submitted"));

    // Approved at a commit the change request has moved past: refused.
    assert!(
        r.refuses(
            &ci,
            &["ci", "--event", "accept", "--branch", "a/x", "--at", &old]
        )
        .contains("the review approved")
    );
    let out = r.ok(
        &ci,
        &["ci", "--event", "accept", "--branch", "a/x", "--at", &head],
    );
    assert!(
        out.contains(&format!("#1 accepted at {}", &head[..12])),
        "{out}"
    );
    assert!(row(&ci).starts_with("- [x] #1") && row(&ci).contains("via:review"));
    let before = tip(&ci, "main");
    assert!(
        r.ok(
            &ci,
            &["ci", "--event", "accept", "--branch", "a/x", "--at", &head]
        )
        .contains("#1 already accepted at")
    );
    assert_eq!(tip(&ci, "main"), before);
    // The change request's check now passes, and every event commit passes lint.
    r.ok(&ci, &["ci", "--branch", "a/x", "--head", &head]);
    let root = r.git(&ci, &["rev-list", "--max-parents=0", "main"]);
    r.ok(&ci, &["lint", &format!("{root}..main")]);
}

#[test]
fn ci_events_refuse_in_one_line() {
    let r = Repo::new("cievrefuse");
    r.ok(&r.main, &["add", "x", "branch:a/x"]);
    r.ok(&r.main, &["add", "y", "branch:a/x"]);
    r.ok(&r.main, &["add", "z", "branch:b/z"]);
    r.ok(&r.main, &["add", "w"]);
    r.git(&r.main, &["branch", "a/x"]);
    r.git(&r.main, &["branch", "b/z"]);
    let sha = r.git(&r.main, &["rev-parse", "HEAD"]);
    for (args, want) in [
        (
            &["ci", "--event", "merge", "--branch", "a/x"][..],
            "--event is submit or accept",
        ),
        (&["ci", "--event", "submit"][..], "needs --branch"),
        (
            &["ci", "--event", "submit", "--branch", "a/x"][..],
            "#1, #2 name a/x — pass --task",
        ),
        (
            &["ci", "--event", "submit", "--branch", "b/z", "--at", "HEAD"][..],
            "goes with --event accept",
        ),
        (
            &["ci", "--event", "accept", "--branch", "b/z"][..],
            "needs --at",
        ),
        (
            &[
                "ci", "--event", "submit", "--branch", "b/z", "--base", "HEAD",
            ][..],
            "not --base",
        ),
        (&["ci", "--at", "HEAD"][..], "go with --event"),
        (
            &["ci", "--event", "submit", "--branch", "main"][..],
            "not a task branch",
        ),
        (
            &["ci", "--event", "submit", "--branch", "c/y", "--task", "1"][..],
            "#1 names branch a/x, not c/y",
        ),
        (
            &["ci", "--event", "submit", "--branch", "b/z", "--task", "4"][..],
            "#3 already names b/z, not #4 — drop --task",
        ),
        (
            &["ci", "--event", "accept", "--branch", "b/z", "--at", &sha][..],
            "#3 is not submitted",
        ),
    ] {
        assert!(r.refuses(&r.main, args).contains(want), "{args:?}");
    }
    // A branch no task names: nothing to do, not a failure.
    r.git(&r.main, &["branch", "c/free"]);
    let out = r.ok(&r.main, &["ci", "--event", "submit", "--branch", "c/free"]);
    assert!(
        out.contains("no task names c/free — nothing to submit"),
        "{out}"
    );
    r.lint_history();
}

#[test]
fn lane_kinds_set_behaviour_whatever_the_lane_is_called() {
    let r = Repo::new("kinds");
    let cfg = std::fs::read_to_string(r.main.join(".5w.toml"))
        .unwrap()
        .replace(
            "[lanes.manual]\nkind = \"manual\"",
            "[lanes.game]\nkind = \"manual\"\nsection = \"## In-game capture\"",
        );
    std::fs::write(r.main.join(".5w.toml"), cfg).unwrap();
    r.git(&r.main, &["commit", "-qam", "game is a manual lane here"]);
    r.ok(&r.main, &["add", "capture the menu", "lane:game"]);
    assert!(
        r.tasks()
            .contains("## In-game capture\n\n- [ ] #1 capture the menu >game")
    );
    assert!(
        r.fails(&r.main, &["delegate", "1"])
            .contains(">game (manual)")
    );
    assert!(r.ok(&r.main, &["levels"]).contains(">game (manual) 1"));
    assert!(
        r.ok(&r.main, &["show", "1", "--json"])
            .contains("\"kind\":\"manual\"")
    );
    // Submitting a manual row warns: nothing can check who did it.
    r.git(&r.main, &["branch", "capture/menu"]);
    let o = r.cli(&r.main, &["submit", "1", "capture/menu"]);
    assert!(
        String::from_utf8_lossy(&o.stderr).contains("only a person, by hand, can have done this")
    );
    // A decision lane closes --decided; restricted is delegable.
    r.ok(&r.main, &["add", "decide", "lane:owner"]);
    assert!(
        r.fails(&r.main, &["done", "2", "--self"])
            .contains("--decided")
    );
    r.ok(&r.main, &["add", "deploy", "lane:restricted"]);
    assert!(
        r.ok(&r.main, &["delegate", "3"])
            .contains("needs access an agent may not have")
    );
}

// --- report: feedback about 5W itself -------------------------------------------------

#[test]
fn a_report_carries_the_last_failure_and_sends_nothing_by_itself() {
    let r = Repo::new("report");
    r.ok(&r.main, &["add", "x"]);
    let refused = r.fails(&r.main, &["accept", "1"]);
    assert!(refused.contains("never submitted"));

    let out = r.ok(
        &r.main,
        &[
            "report",
            "accept refused a task I had just submitted",
            "--expected",
            "it accepts",
        ],
    );
    assert!(
        out.contains("saved report 1") && out.contains("nothing was sent"),
        "{out}"
    );
    let shown = r.ok(&r.main, &["report", "show", "1"]);
    assert!(
        shown.starts_with("# accept refused a task I had just submitted\n"),
        "{shown}"
    );
    assert!(shown.contains("## Expected\n\nit accepts"));
    assert!(
        shown.contains("5w accept 1") && shown.contains("never submitted"),
        "last failure attached:\n{shown}"
    );
    assert!(shown.contains(&format!("- 5w {}", env!("CARGO_PKG_VERSION"))));
    assert!(
        r.ok(&r.main, &["report", "list"])
            .contains("1 accept refused")
    );

    // The report command's own failure does not replace the recorded one.
    r.fails(&r.main, &["report", "show", "9"]);
    r.ok(&r.main, &["report", "second", "--no-last"]);
    assert!(
        !r.ok(&r.main, &["report", "show", "2"])
            .contains("Last failure")
    );

    let url = r.ok(&r.main, &["report", "send", "1", "--print"]);
    assert!(url.starts_with("https://github.com/mmmeon/5W/issues/new?labels=report&title=accept%20refused%20a%20task"), "{url}");
    assert!(url.contains("%23%23%20What%20happened"), "{url}");
    let mut c = Command::new(bin5w());
    c.args(["report", "send", "1", "--print"])
        .current_dir(&r.main);
    env(&mut c, &r.root);
    c.env("FIVEW_ISSUES", "someone/fork");
    let o = c.output().unwrap();
    assert!(
        String::from_utf8_lossy(&o.stdout)
            .starts_with("https://github.com/someone/fork/issues/new?")
    );

    // Reports live under .git, never in the working tree.
    assert_eq!(r.git(&r.main, &["status", "--porcelain"]), "");
    r.ok(&r.main, &["report", "rm", "2"]);
    assert!(!r.ok(&r.main, &["report", "list"]).contains("second"));
}

#[test]
fn a_crash_records_last_failure_and_says_how_to_report_it() {
    let r = Repo::new("crash");
    let mut c = Command::new(bin5w());
    c.args(["ready"]).current_dir(&r.main);
    env(&mut c, &r.root);
    c.env("FIVEW_TEST_PANIC", "1");
    let o = c.output().unwrap();
    assert!(!o.status.success());
    let err = String::from_utf8_lossy(&o.stderr).to_string();
    assert!(
        err.contains("5w crashed — this is a bug") && err.contains("5w report"),
        "{err}"
    );

    let last =
        std::fs::read_to_string(r.main.join(".git/5w/last-failure.md")).expect("last-failure.md");
    assert!(last.contains("5w ready"), "{last}");
    assert!(
        last.contains("panic: ") && last.contains("FIVEW_TEST_PANIC"),
        "{last}"
    );

    // The crash is attached to a report exactly like any other last failure.
    let out = r.ok(&r.main, &["report", "ready crashed on me"]);
    assert!(out.contains("saved report 1"), "{out}");
    let shown = r.ok(&r.main, &["report", "show", "1"]);
    assert!(
        shown.contains("panic: ") && shown.contains("FIVEW_TEST_PANIC"),
        "{shown}"
    );
}

// --- staying current --------------------------------------------------------------------

fn set_requires(r: &Repo, v: &str) {
    let cfg = std::fs::read_to_string(r.main.join(".5w.toml")).unwrap();
    let cfg = cfg
        .lines()
        .map(|l| {
            if l.starts_with("requires = ") {
                format!("requires = \"{v}\"")
            } else {
                l.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    std::fs::write(r.main.join(".5w.toml"), cfg).unwrap();
    r.git(&r.main, &["commit", "-qam", "pin"]);
}

#[test]
fn a_project_requiring_a_newer_5w_is_refused_clearly() {
    let r = Repo::new("requires");
    // init pins the version that wrote it.
    let cfg = std::fs::read_to_string(r.main.join(".5w.toml")).unwrap();
    assert!(
        cfg.contains(&format!("requires = \"{}\"", env!("CARGO_PKG_VERSION"))),
        "{cfg}"
    );
    r.ok(&r.main, &["doctor"]);

    set_requires(&r, "99.0.0");
    let out = r.refuses(&r.main, &["ready"]);
    assert!(
        out.contains("requires 5w 99.0.0 or later") && out.contains("releases/tag/v99.0.0"),
        "{out}"
    );
    // A newer key alongside does not mask the pin.
    let cfg = std::fs::read_to_string(r.main.join(".5w.toml")).unwrap() + "future_key = true\n";
    std::fs::write(r.main.join(".5w.toml"), cfg).unwrap();
    r.git(&r.main, &["commit", "-qam", "future"]);
    assert!(r.fails(&r.main, &["ready"]).contains("requires 5w 99.0.0"));
    // Nothing that does not need the project is blocked.
    r.ok(&r.main, &["--version"]);
}

#[test]
fn an_unknown_key_says_a_newer_5w_may_know_it() {
    let r = Repo::new("unknownkey");
    let cfg = std::fs::read_to_string(r.main.join(".5w.toml")).unwrap() + "future_key = true\n";
    std::fs::write(r.main.join(".5w.toml"), cfg).unwrap();
    r.git(&r.main, &["commit", "-qam", "future"]);
    assert!(
        r.fails(&r.main, &["ready"])
            .contains("a newer one may know it")
    );
}

#[test]
fn doctor_finds_stale_installed_files_and_update_files_refreshes_them() {
    let r = Repo::new("stale");
    r.ok(&r.main, &["hook", "install"]);
    let proto = std::fs::read_to_string(r.main.join("PROTOCOL.md")).unwrap();
    assert!(proto.starts_with(&format!(
        "<!-- 5w {} protocol -->",
        env!("CARGO_PKG_VERSION")
    )));
    assert!(!r.ok(&r.main, &["doctor"]).contains("note: PROTOCOL.md"));

    // As if an older 5w had installed both.
    let old = proto
        .replacen(env!("CARGO_PKG_VERSION"), "0.0.9", 1)
        .replace("## Commits", "## Commit rules");
    std::fs::write(r.main.join("PROTOCOL.md"), old).unwrap();
    r.git(&r.main, &["commit", "-qam", "old protocol"]);
    let hook = r.main.join(".git/hooks/pre-commit");
    let h = std::fs::read_to_string(&hook)
        .unwrap()
        .replace(env!("CARGO_PKG_VERSION"), "0.0.9");
    std::fs::write(&hook, h).unwrap();
    set_requires(&r, "0.0.9");

    let doc = r.ok(&r.main, &["doctor"]);
    assert!(doc.contains("PROTOCOL.md is from 0.0.9"), "{doc}");
    assert!(doc.contains("pre-commit hook is from an older 5w"), "{doc}");
    assert!(doc.contains("requires = \"0.0.9\""), "{doc}");

    let up = r.ok(&r.main, &["update-files", "--pin"]);
    assert!(
        up.contains("updated PROTOCOL.md")
            && up.contains("updated pre-commit hook")
            && up.contains("requires"),
        "{up}"
    );
    // Left for review, not committed.
    let status = r.git(&r.main, &["status", "--porcelain"]);
    assert!(
        status.contains("PROTOCOL.md") && status.contains(".5w.toml"),
        "{status}"
    );
    r.git(&r.main, &["commit", "-qam", "update 5w files"]);
    let doc = r.ok(&r.main, &["doctor"]);
    assert!(
        !doc.contains("note: PROTOCOL")
            && !doc.contains("hook is from")
            && !doc.contains("requires ="),
        "{doc}"
    );
    assert!(r.ok(&r.main, &["update-files"]).contains("up to date"));
}

/// A copy of the binary under test in a scratch `bin/`, so self-update replaces
/// the copy and never the binary the suite runs; and a release server under
/// `releases/` laid out as GitHub serves one, reached through file://.
struct Releases {
    root: PathBuf,
    exe: PathBuf,
}

const FAKE: &str = "9.9.9";

impl Releases {
    fn new(name: &str) -> Releases {
        let root = std::env::temp_dir().join(format!(
            "5w-test-{name}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("bin")).unwrap();
        let exe = root.join("bin/5w");
        std::fs::copy(bin5w(), &exe).unwrap();
        let dir = root.join(format!("releases/download/v{FAKE}"));
        std::fs::create_dir_all(&dir).unwrap();
        // Over one SHA-256 block, so the hash is exercised past its padding.
        let script = format!("#!/bin/sh\necho \"5w {FAKE}\"\n# {}\n", "x".repeat(200));
        std::fs::write(dir.join(Self::asset()), script).unwrap();
        Releases { root, exe }
    }

    fn asset() -> String {
        format!("5w-{FAKE}-{}-unknown-linux-musl", std::env::consts::ARCH)
    }

    fn dir(&self) -> PathBuf {
        self.root.join(format!("releases/download/v{FAKE}"))
    }

    /// SHA256SUMS as sha256sum writes it, published as the latest too.
    fn write_sums(&self) {
        let o = Command::new("sha256sum")
            .arg(Self::asset())
            .current_dir(self.dir())
            .output()
            .unwrap();
        assert!(o.status.success());
        std::fs::write(self.dir().join("SHA256SUMS"), &o.stdout).unwrap();
        let latest = self.root.join("releases/latest/download");
        std::fs::create_dir_all(&latest).unwrap();
        std::fs::write(latest.join("SHA256SUMS"), &o.stdout).unwrap();
    }

    fn run(&self, cwd: &Path, path: &str, args: &[&str]) -> (bool, String) {
        self.run_with(cwd, path, args, &[])
    }

    fn run_with(
        &self,
        cwd: &Path,
        path: &str,
        args: &[&str],
        extra: &[(&str, &str)],
    ) -> (bool, String) {
        let o = Command::new(&self.exe)
            .args(args)
            .current_dir(cwd)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("NO_COLOR", "1")
            .env("PATH", path)
            .env(
                "FIVEW_RELEASES_URL",
                format!("file://{}", self.root.join("releases").display()),
            )
            .env("FIVEW_RELEASE_KEY", self.root.join("key.asc"))
            .envs(extra.iter().copied())
            .output()
            .unwrap();
        let out = format!(
            "{}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        );
        (o.status.success(), out)
    }
}

impl Drop for Releases {
    fn drop(&mut self) {
        if let Ok(dirs) = std::fs::read_dir(&self.root) {
            for d in dirs.flatten() {
                if d.file_name().to_string_lossy().starts_with("gnupg-") {
                    let _ = Command::new("gpgconf")
                        .args(["--kill", "all"])
                        .env("GNUPGHOME", d.path())
                        .output();
                }
            }
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// A throwaway gpg home with one signing key, exported to `<root>/<name>.asc`.
fn gpg_key(root: &Path, name: &str) -> impl Fn(&[&str]) -> Output {
    let home = root.join(format!("gnupg-{name}"));
    std::fs::create_dir_all(&home).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
    let gpg = move |args: &[&str]| {
        Command::new("gpg")
            .env("GNUPGHOME", &home)
            .args(["--batch", "--pinentry-mode", "loopback", "--passphrase", ""])
            .args(args)
            .output()
            .unwrap()
    };
    let uid = format!("{name} <{name}@example.com>");
    assert!(
        gpg(&["--quick-generate-key", &uid, "ed25519", "sign", "never"])
            .status
            .success()
    );
    let out = root.join(format!("{name}.asc"));
    assert!(
        gpg(&["--armor", "--export", "-o", out.to_str().unwrap()])
            .status
            .success()
    );
    gpg
}

#[test]
fn version_latest_reports_the_newest_release_and_changes_nothing() {
    let rel = Releases::new("latest");
    let path = std::env::var("PATH").unwrap();
    let before = std::fs::read(&rel.exe).unwrap();

    let (ok, out) = rel.run(&rel.root, &path, &["version"]);
    assert!(
        ok && out.trim() == format!("5w {}", env!("CARGO_PKG_VERSION")),
        "{out}"
    );

    // No release published yet: said so, in one line.
    let (ok, out) = rel.run(&rel.root, &path, &["version", "--latest"]);
    assert!(!ok && out.contains("no release found at file://"), "{out}");

    let o = Command::new("sha256sum").arg("--version").output();
    if o.is_err() {
        eprintln!("skipped the rest: no sha256sum to write the fixture");
        return;
    }
    rel.write_sums();
    let (ok, out) = rel.run(&rel.root, &path, &["version", "--latest"]);
    assert!(
        ok && out.contains(&format!("newest release {FAKE}"))
            && out.contains("5w self-update --latest")
            && out.lines().count() == 1,
        "{out}"
    );
    assert_eq!(std::fs::read(&rel.exe).unwrap(), before);

    // A release with no SHA256SUMS.asc is not installed.
    std::fs::write(rel.root.join("key.asc"), "").unwrap();
    let (ok, out) = rel.run(&rel.root, &path, &["self-update", "--latest"]);
    assert!(!ok && out.contains("no SHA256SUMS.asc"), "{out}");
    assert_eq!(std::fs::read(&rel.exe).unwrap(), before);

    // Without the tools it shells out to: one line naming the missing one.
    let empty = rel.root.join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    let empty = empty.to_str().unwrap();
    let (ok, out) = rel.run(&rel.root, empty, &["version", "--latest"]);
    assert!(!ok && out.contains("needs curl on PATH"), "{out}");
    let (ok, out) = rel.run(&rel.root, empty, &["self-update", "--latest"]);
    assert!(
        !ok && out.contains("needs curl on PATH") && out.lines().count() == 1,
        "{out}"
    );
    // gpg missing while curl is there.
    let only_curl = rel.root.join("only-curl");
    std::fs::create_dir_all(&only_curl).unwrap();
    let curl = String::from_utf8(
        Command::new("sh")
            .args(["-c", "command -v curl"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    std::os::unix::fs::symlink(curl.trim(), only_curl.join("curl")).unwrap();
    let (ok, out) = rel.run(
        &rel.root,
        only_curl.to_str().unwrap(),
        &["self-update", "--latest"],
    );
    assert!(!ok && out.contains("needs gpg on PATH"), "{out}");
    assert_eq!(std::fs::read(&rel.exe).unwrap(), before);
}

#[test]
fn self_update_installs_only_a_signed_matching_release() {
    for tool in ["gpg", "sha256sum", "curl"] {
        if Command::new(tool).arg("--version").output().is_err() {
            eprintln!("skipped: no {tool} in the test environment");
            return;
        }
    }
    let rel = Releases::new("selfupdate");
    let path = std::env::var("PATH").unwrap();
    let before = std::fs::read(&rel.exe).unwrap();
    let release = gpg_key(&rel.root, "key");
    let other = gpg_key(&rel.root, "other");
    rel.write_sums();
    let sums = rel.dir().join("SHA256SUMS");
    let sign = |gpg: &dyn Fn(&[&str]) -> Output| {
        let asc = rel.dir().join("SHA256SUMS.asc");
        let o = gpg(&[
            "--yes",
            "--armor",
            "--detach-sign",
            "-o",
            asc.to_str().unwrap(),
            sums.to_str().unwrap(),
        ]);
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    };
    let untouched = |out: &str| {
        assert_eq!(std::fs::read(&rel.exe).unwrap(), before, "{out}");
        let left: Vec<_> = std::fs::read_dir(rel.root.join("bin")).unwrap().collect();
        assert_eq!(left.len(), 1, "{out}");
    };

    // A key from the environment is not trusted for a download from the network.
    let (ok, out) = rel.run_with(
        &rel.root,
        &path,
        &["self-update", "--latest"],
        &[("FIVEW_RELEASES_URL", "https://example.invalid/releases")],
    );
    assert!(
        !ok && out.contains("FIVEW_RELEASE_KEY is honoured only with a file://")
            && out.lines().count() == 1,
        "{out}"
    );
    untouched(&out);

    // Signed by a key that is not the release key.
    sign(&other);
    let (ok, out) = rel.run(&rel.root, &path, &["self-update", "--latest"]);
    assert!(
        !ok && out.contains("not signed by the 5W release key"),
        "{out}"
    );
    untouched(&out);

    // Two signatures, one of them the release key's.
    let asc = rel.dir().join("SHA256SUMS.asc");
    let other_sig = std::fs::read(&asc).unwrap();
    sign(&release);
    let mut both = std::fs::read(&asc).unwrap();
    both.extend(other_sig);
    std::fs::write(&asc, both).unwrap();
    let (ok, out) = rel.run(&rel.root, &path, &["self-update", "--latest"]);
    assert!(
        !ok && out.contains("not signed by the 5W release key"),
        "{out}"
    );
    untouched(&out);

    // Signed by the release key, but not SHA256SUMS as a release writes it:
    // every such file is refused whole.
    let good_sums = std::fs::read_to_string(&sums).unwrap();
    let line = good_sums.trim_end();
    for bad in [
        format!("{good_sums}\n"),
        format!("{good_sums}# note\n"),
        format!("{good_sums}{good_sums}"),
        line.to_string(),
        good_sums.replacen("  ", " ", 1),
        good_sums.replacen("  5w-", "  ../5w-", 1),
        good_sums.replacen("  5w-", " *5w-", 1),
        good_sums.replacen(FAKE, "9.9.9-rc1", 1),
    ] {
        std::fs::write(&sums, &bad).unwrap();
        sign(&release);
        let (ok, out) = rel.run(&rel.root, &path, &["self-update", "--latest"]);
        assert!(
            !ok && out.contains("not in the release format"),
            "{bad:?}: {out}"
        );
        untouched(&out);
    }

    // A signed commit is text the release key signed that can carry a sums line:
    // its payload as SHA256SUMS, its signature as SHA256SUMS.asc.
    let asset = rel.dir().join(Releases::asset());
    let good = std::fs::read(&asset).unwrap();
    let evil = b"#!/bin/sh\necho evil\n";
    let o = Command::new("sha256sum")
        .arg("-")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut c| {
            use std::io::Write;
            c.stdin.take().unwrap().write_all(evil)?;
            c.wait_with_output()
        })
        .unwrap();
    let evil_hash = String::from_utf8_lossy(&o.stdout)[..64].to_string();
    let git = |args: &[&str]| {
        let o = Command::new("git")
            .args(args)
            .current_dir(rel.root.join("commit"))
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GNUPGHOME", rel.root.join("gnupg-key"))
            .env("GIT_AUTHOR_NAME", "key")
            .env("GIT_AUTHOR_EMAIL", "key@example.com")
            .env("GIT_COMMITTER_NAME", "key")
            .env("GIT_COMMITTER_EMAIL", "key@example.com")
            .output()
            .unwrap();
        assert!(
            o.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        String::from_utf8_lossy(&o.stdout).to_string()
    };
    std::fs::create_dir_all(rel.root.join("commit")).unwrap();
    git(&["init", "-q"]);
    let fpr = String::from_utf8_lossy(&release(&["--with-colons", "--list-secret-keys"]).stdout)
        .lines()
        .find_map(|l| {
            l.strip_prefix("fpr:")
                .map(|f| f.trim_matches(':').to_string())
        })
        .unwrap();
    let msg = format!("{evil_hash}  {}\n", Releases::asset());
    git(&[
        "-c",
        &format!("user.signingkey={fpr}"),
        "commit",
        "-q",
        "-S",
        "--allow-empty",
        "-m",
        &msg,
    ]);
    let raw = git(&["cat-file", "commit", "HEAD"]);
    let (mut payload, mut signature, mut in_sig) = (String::new(), String::new(), false);
    for l in raw.split_inclusive('\n') {
        if let Some(first) = l.strip_prefix("gpgsig ") {
            signature.push_str(first);
            in_sig = true;
        } else if in_sig && l.starts_with(' ') {
            signature.push_str(&l[1..]);
        } else {
            in_sig = false;
            payload.push_str(l);
        }
    }
    assert!(
        payload.contains(&msg) && signature.contains("BEGIN PGP SIGNATURE"),
        "{raw}"
    );
    std::fs::write(&sums, &payload).unwrap();
    std::fs::write(&asc, &signature).unwrap();
    std::fs::write(&asset, evil).unwrap();
    let (ok, out) = rel.run(&rel.root, &path, &["self-update", "--latest"]);
    // The signature is good; the text is not a release's.
    assert!(!ok && out.contains("not in the release format"), "{out}");
    untouched(&out);
    // Served as the latest release's sums too.
    let latest = rel.root.join("releases/latest/download/SHA256SUMS");
    let good_latest = std::fs::read(&latest).unwrap();
    std::fs::write(&latest, &payload).unwrap();
    let (ok, out) = rel.run(&rel.root, &path, &["version", "--latest"]);
    assert!(!ok && out.contains("is not a 5w SHA256SUMS"), "{out}");
    std::fs::write(&latest, good_latest).unwrap();
    std::fs::write(&asset, &good).unwrap();
    std::fs::write(&sums, &good_sums).unwrap();

    // Signed, but the binary is not the one the sums name.
    sign(&release);
    std::fs::write(&asset, evil).unwrap();
    let (ok, out) = rel.run(&rel.root, &path, &["self-update", "--latest"]);
    assert!(!ok && out.contains("does not match SHA256SUMS"), "{out}");
    untouched(&out);
    std::fs::write(&asset, good).unwrap();

    // By default, the version the project pins — even one this binary refuses.
    let r = Repo::new("selfupdate-pin");
    set_requires(&r, FAKE);
    let (ok, out) = rel.run(&r.main, &path, &["ready"]);
    assert!(!ok && out.contains(&format!("requires 5w {FAKE}")), "{out}");
    let (ok, out) = rel.run(&r.main, &path, &["self-update"]);
    assert!(
        ok && out.contains(&format!("5w {} → {FAKE}", env!("CARGO_PKG_VERSION"))),
        "{out}"
    );
    let o = Command::new(&rel.exe).arg("--version").output().unwrap();
    assert_eq!(
        String::from_utf8_lossy(&o.stdout).trim(),
        format!("5w {FAKE}")
    );
    assert_eq!(std::fs::read_dir(rel.root.join("bin")).unwrap().count(), 1);
}

#[test]
fn queue_commits_are_signed_when_the_repository_signs() {
    if Command::new("gpg").arg("--version").output().is_err() {
        eprintln!("skipped: no gpg");
        return;
    }
    let r = Repo::new("signed");
    let home = r.root.join("gnupg");
    std::fs::create_dir_all(&home).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
    let gpg = |args: &[&str]| {
        Command::new("gpg")
            .env("GNUPGHOME", &home)
            .args(["--batch", "--pinentry-mode", "loopback", "--passphrase", ""])
            .args(args)
            .output()
            .unwrap()
    };
    assert!(
        gpg(&[
            "--quick-generate-key",
            "t <t@example.com>",
            "ed25519",
            "sign",
            "never"
        ])
        .status
        .success()
    );
    let fpr = String::from_utf8_lossy(&gpg(&["--with-colons", "--list-secret-keys"]).stdout)
        .lines()
        .find_map(|l| {
            l.strip_prefix("fpr:")
                .map(|f| f.trim_matches(':').to_string())
        })
        .unwrap();
    r.git(&r.main, &["config", "commit.gpgsign", "true"]);
    r.git(&r.main, &["config", "user.signingkey", &fpr]);

    let mut c = Command::new(bin5w());
    c.args(["add", "signed row"]).current_dir(&r.main);
    env(&mut c, &r.root);
    c.env("GNUPGHOME", &home);
    let o = c.output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));

    let mut v = Command::new("git");
    v.args(["log", "-1", "--format=%G? %GF"])
        .current_dir(&r.main);
    env(&mut v, &r.root);
    v.env("GNUPGHOME", &home);
    let shown = String::from_utf8_lossy(&v.output().unwrap().stdout)
        .trim()
        .to_string();
    assert!(shown.ends_with(&fpr) && !shown.starts_with('N'), "{shown}");
}

#[test]
fn worktree_paths_are_normalized() {
    let r = Repo::new("wt-paths");
    r.ok(&r.main, &["add", "test task"]);

    // Configure the worktree root to use a relative path to trigger the normalization issue
    let config_path = r.main.join(".5w.toml");
    let mut config = std::fs::read_to_string(&config_path).unwrap();
    config = config.replace(
        "[worktrees]",
        "[worktrees]\nroot = \"../wt-rel\"\ninstall = \"true\"",
    );
    std::fs::write(&config_path, config).unwrap();

    // Create a new worktree using the relative path configuration; --install prints its directory
    let output = r.ok_with_relative_wt(&r.main, &["wt", "new", "feature/test", "--install"]);
    assert!(output.contains("(in "), "{output}");

    // Verify the output path is normalized (no .. components)
    assert!(
        !output.contains(".."),
        "wt new output should not contain .. components, got:\n{}",
        output
    );

    // Also verify wt path returns normalized paths
    let path_output = r.ok_with_relative_wt(&r.main, &["wt", "path", "feature/test"]);
    assert!(
        !path_output.trim().contains(".."),
        "wt path output should not contain .. components, got: {}",
        path_output
    );

    // Refusals naming a directory under the root print it clean too
    std::fs::create_dir_all(r.root.join("wt-rel/feature-fresh")).unwrap();
    std::fs::create_dir_all(r.root.join("wt-rel/feature-taken")).unwrap();
    r.git(&r.main, &["branch", "feature/taken"]);
    for args in [
        &["wt", "new", "feature/fresh"][..],
        &["wt", "add", "feature/taken"][..],
    ] {
        let o = r.cli_with_relative_wt(&r.main, args);
        let out = String::from_utf8_lossy(&o.stderr).to_string();
        assert!(!o.status.success() && out.contains("exists"), "{out}");
        assert!(!out.contains(".."), "{args:?}: {out}");
    }
}

#[test]
fn relative_wt_root_env_is_read_from_the_primary_checkout() {
    let r = Repo::new("wt-env-rel");
    r.ok(&r.main, &["wt", "new", "a/first"]);
    let sub = r.wt("a/first").join("deep/er");
    std::fs::create_dir_all(&sub).unwrap();
    let run = |cwd: &Path, args: &[&str]| {
        let mut c = Command::new(bin5w());
        c.args(args).current_dir(cwd);
        env(&mut c, &r.root);
        c.env("FIVEW_WT_ROOT", "../wt-rel");
        let o = c.output().unwrap();
        let out = String::from_utf8_lossy(&o.stdout).to_string();
        assert!(
            o.status.success(),
            "{args:?}: {out}{}",
            String::from_utf8_lossy(&o.stderr)
        );
        out
    };
    // From a subdirectory of another worktree: lands under <primary>/../wt-rel, where `wt path` says.
    run(&sub, &["wt", "new", "a/second", "--from", "main"]);
    let want = r.root.join("wt-rel/a-second");
    assert!(want.join("README").exists(), "not at {}", want.display());
    assert_eq!(
        run(&sub, &["wt", "path", "a/second"]).trim(),
        want.to_str().unwrap()
    );
    assert_eq!(
        run(&r.main, &["wt", "path", "a/second"]).trim(),
        want.to_str().unwrap()
    );
    // The refusal check looks at the same directory git would use.
    std::fs::create_dir_all(r.root.join("wt-rel/a-third")).unwrap();
    let mut c = Command::new(bin5w());
    c.args(["wt", "new", "a/third"]).current_dir(&sub);
    env(&mut c, &r.root);
    c.env("FIVEW_WT_ROOT", "../wt-rel");
    let o = c.output().unwrap();
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(!o.status.success() && err.contains("exists"), "{err}");
}

#[test]
fn empty_wt_root_env_is_treated_as_unset() {
    let r = Repo::new("wt-env-empty");
    for (val, branch) in [("", "a/empty"), ("  ", "a/blank")] {
        let mut c = Command::new(bin5w());
        c.args(["wt", "new", branch]).current_dir(&r.main);
        env(&mut c, &r.root);
        c.env("FIVEW_WT_ROOT", val);
        let o = c.output().unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        let slug = branch.replace('/', "-");
        // The default root beside the primary, never inside it.
        assert!(r.root.join("repo-wt").join(&slug).join("README").exists());
        assert!(!r.main.join(&slug).exists());
        assert!(!r.main.join(val).join(&slug).exists());
    }
}

#[test]
fn wt_rm_finds_force_anywhere_in_the_arguments() {
    let r = Repo::new("wt-rm-force");
    r.ok(&r.main, &["wt", "new", "a/x"]);
    let wt = r.wt("a/x");
    std::fs::write(wt.join("scratch"), "dirty").unwrap();
    assert!(r.refuses(&r.main, &["wt", "rm", "a/x"]).contains("--force"));
    r.ok(&r.main, &["wt", "rm", "--force", "a/x"]);
    assert!(!wt.exists());
    assert!(
        r.fails(&r.main, &["wt", "rm", "--forse", "a/x"])
            .contains("unknown flag")
    );
}

#[test]
fn wt_prune_lists_then_removes_only_empty_clean_unnamed_branches() {
    let r = Repo::new("wt-prune");
    std::fs::write(r.main.join(".gitignore"), ".env\ntarget\n").unwrap();
    r.git(&r.main, &["add", ".gitignore"]);
    r.git(&r.main, &["commit", "-qm", "ignore"]);
    // Empty, clean, and no open task suggests the name: removable, worktree or not, stacked or not.
    r.ok(&r.main, &["wt", "new", "t/empty"]);
    r.ok(&r.main, &["wt", "new", "t/stacked", "--from", "t/empty"]);
    r.git(&r.main, &["branch", "t/bare"]);
    // Kept: a commit past its parent; a task naming it; uncommitted or ignored files.
    r.ok(&r.main, &["wt", "new", "t/work"]);
    r.commit_in(&r.wt("t/work"), "x", "x\n");
    r.ok(&r.main, &["add", "named", "branch:t/named"]);
    r.ok(&r.main, &["wt", "new", "t/named"]);
    r.ok(&r.main, &["wt", "new", "t/dirty"]);
    std::fs::write(r.wt("t/dirty").join("scratch"), "s\n").unwrap();
    r.ok(&r.main, &["wt", "new", "t/ignored"]);
    std::fs::write(r.wt("t/ignored").join(".env"), "SECRET=1\n").unwrap();
    r.ok(&r.main, &["wt", "new", "t/disposable"]);
    std::fs::create_dir_all(r.wt("t/disposable").join("target")).unwrap();
    std::fs::write(r.wt("t/disposable").join("target/o"), "o\n").unwrap();
    r.ok(&r.main, &["wt", "new", "t/perennial"]);
    r.git(
        &r.main,
        &[
            "config",
            "git-town-branch.t/perennial.branchtype",
            "perennial",
        ],
    );

    let empty_wt = r.wt("t/empty");
    let dry = r.ok(&r.main, &["wt", "prune"]);
    for b in ["t/empty", "t/stacked", "t/bare", "t/disposable"] {
        assert!(dry.contains(&format!("remove {b}")), "{b}: {dry}");
    }
    assert!(
        dry.contains("kept t/dirty") && dry.contains("kept t/ignored"),
        "{dry}"
    );
    assert!(dry.contains(".env"), "{dry}");
    for b in ["t/work", "t/named", "t/perennial", "main"] {
        assert!(!dry.contains(&format!(" {b}")), "{b}: {dry}");
    }
    assert!(dry.contains("--yes"), "{dry}");
    // A dry run touches nothing.
    assert!(empty_wt.exists());
    r.git(&r.main, &["rev-parse", "--verify", "t/bare"]);

    assert!(
        r.fails(&r.main, &["wt", "prune", "--yse"])
            .contains("unknown flag")
    );

    let out = r.ok(&r.main, &["wt", "prune", "--yes"]);
    assert!(out.contains("removed"), "{out}");
    assert!(!empty_wt.exists());
    let branches = r.git(&r.main, &["branch", "--format=%(refname:short)"]);
    for b in ["t/empty", "t/stacked", "t/bare", "t/disposable"] {
        assert!(!branches.lines().any(|l| l == b), "{b}: {branches}");
        assert!(
            r.git(&r.main, &["config", "--get-regexp", "git-town-branch"])
                .lines()
                .all(|l| !l.starts_with(&format!("git-town-branch.{b}."))),
            "{b}"
        );
    }
    for b in [
        "main",
        "t/work",
        "t/named",
        "t/dirty",
        "t/ignored",
        "t/perennial",
    ] {
        assert!(branches.lines().any(|l| l == b), "{b}: {branches}");
    }
    assert!(r.wt("t/ignored").join(".env").exists());
    assert!(r.main.join(".gitignore").exists());
}

#[test]
fn wt_prune_keeps_the_suggested_branch_of_a_task_not_closed() {
    let r = Repo::new("wt-prune-open");
    r.ok(&r.main, &["add", "open work", "area:t"]);
    // Before the first commit and before submit: nothing names the branch yet.
    r.ok(&r.main, &["wt", "new", "t/task-1"]);
    let wt = r.wt("t/task-1");
    let out = r.ok(&r.main, &["wt", "prune", "--yes"]);
    assert!(!out.contains("t/task-1"), "{out}");
    assert!(wt.exists());
    r.git(&r.main, &["rev-parse", "--verify", "t/task-1"]);
}

#[test]
fn wt_prune_keeps_a_task_branch_after_the_area_changes() {
    let r = Repo::new("wt-prune-area");
    r.ok(&r.main, &["add", "open work", "area:a"]);
    r.ok(&r.main, &["wt", "new", "a/task-1"]);
    // The suggestion is now b/task-1; the worker's a/task-1 is still the task's.
    r.ok(&r.main, &["set", "1", "area", "b"]);
    r.ok(&r.main, &["wt", "new", "x/task-99"]);
    let stray = r.wt("x/task-99");
    let out = r.ok(&r.main, &["wt", "prune", "--yes"]);
    assert!(!out.contains("a/task-1"), "{out}");
    assert!(r.wt("a/task-1").exists());
    r.git(&r.main, &["rev-parse", "--verify", "a/task-1"]);
    // No task 99: its branch goes.
    assert!(!stray.exists(), "{out}");
    let branches = r.git(&r.main, &["branch", "--format=%(refname:short)"]);
    assert!(!branches.lines().any(|l| l == "x/task-99"), "{branches}");
}

#[test]
fn wt_prune_keeps_a_worktree_outside_the_worktree_root() {
    let r = Repo::new("wt-prune-outside");
    let manual = r.root.join("manual");
    r.git(
        &r.main,
        &[
            "worktree",
            "add",
            "-q",
            &manual.to_string_lossy(),
            "-b",
            "manual/keep",
        ],
    );
    let out = r.ok(&r.main, &["wt", "prune", "--yes"]);
    assert!(
        out.contains("kept manual/keep — worktree outside worktrees.root"),
        "{out}"
    );
    assert!(manual.exists());
    r.git(&r.main, &["rev-parse", "--verify", "manual/keep"]);
}

#[test]
fn wt_prune_keeps_a_branch_under_a_rebase_or_bisect() {
    let r = Repo::new("wt-prune-rebase");
    r.ok(&r.main, &["wt", "new", "t/rebasing"]);
    r.ok(&r.main, &["wt", "new", "t/bisecting"]);
    r.commit_in(&r.main, "later", "later\n");
    let wt = r.wt("t/rebasing");
    let mut c = Command::new("git");
    c.args(["rebase", "-i", "main"]).current_dir(&wt);
    env(&mut c, &r.root);
    c.env("GIT_SEQUENCE_EDITOR", "sed -i 1ibreak");
    assert!(c.output().unwrap().status.success());
    r.git(&r.wt("t/bisecting"), &["bisect", "start"]);
    let out = r.ok(&r.main, &["wt", "prune", "--yes"]);
    for b in ["t/rebasing", "t/bisecting"] {
        assert!(
            out.contains(&format!("kept {b} — a rebase or bisect")),
            "{out}"
        );
        r.git(&r.main, &["rev-parse", "--verify", b]);
    }
    assert!(wt.exists());
}

#[test]
fn doctor_names_a_trunk_copy_of_a_branch_and_discard_copy_takes_only_an_exact_one() {
    let r = Repo::new("copy");
    r.ok(&r.main, &["wt", "new", "a/x"]);
    let wt = r.wt("a/x");
    std::fs::create_dir_all(wt.join("new/deep")).unwrap();
    std::fs::write(wt.join("new/deep/file"), "added\n").unwrap();
    std::fs::write(wt.join("README"), "hi\nthere\n").unwrap();
    r.git(&wt, &["add", "-A"]);
    r.git(&wt, &["commit", "-qm", "change"]);
    r.ok(&r.main, &["wt", "new", "b/y"]);
    r.commit_in(&r.wt("b/y"), "README", "other\n");

    // Clean trunk: nothing to name, nothing to discard.
    assert!(!r.ok(&r.main, &["doctor"]).contains("discard-copy"));
    assert!(
        r.fails(&r.main, &["wt", "discard-copy", "a/x"])
            .contains("no uncommitted changes")
    );

    // The same change copied into the trunk checkout, part of it staged.
    std::fs::create_dir_all(r.main.join("new/deep")).unwrap();
    std::fs::write(r.main.join("new/deep/file"), "added\n").unwrap();
    std::fs::write(r.main.join("README"), "hi\nthere\n").unwrap();
    r.git(&r.main, &["add", "README"]);
    let doc = r.ok(&r.main, &["doctor"]);
    assert!(doc.contains("`5w wt discard-copy a/x`"), "{doc}");
    assert!(!doc.contains("b/y"), "{doc}");

    // Not exact — a byte differs, or something else is changed too: refused, untouched.
    std::fs::write(r.main.join("new/deep/file"), "added \n").unwrap();
    assert!(!r.ok(&r.main, &["doctor"]).contains("discard-copy"));
    assert!(
        r.fails(&r.main, &["wt", "discard-copy", "a/x"])
            .contains("not exactly a/x's diff")
    );
    assert_eq!(
        std::fs::read_to_string(r.main.join("new/deep/file")).unwrap(),
        "added \n"
    );
    std::fs::write(r.main.join("new/deep/file"), "added\n").unwrap();
    std::fs::write(r.main.join("mine"), "keep\n").unwrap();
    assert!(
        r.fails(&r.main, &["wt", "discard-copy", "a/x"])
            .contains("not exactly")
    );
    std::fs::remove_file(r.main.join("mine")).unwrap();
    assert!(
        r.fails(&r.main, &["wt", "discard-copy", "b/y"])
            .contains("not exactly")
    );

    // Partly staged: the staged text is in neither HEAD nor the branch.
    std::fs::write(r.main.join("README"), "staged\n").unwrap();
    r.git(&r.main, &["add", "README"]);
    std::fs::write(r.main.join("README"), "hi\nthere\n").unwrap();
    assert!(
        r.fails(&r.main, &["wt", "discard-copy", "a/x"])
            .contains("partly staged")
    );
    r.git(&r.main, &["add", "README"]);

    let out = r.ok(&r.main, &["wt", "discard-copy", "a/x"]);
    assert!(out.contains("discarded 2 file(s)"), "{out}");
    assert_eq!(r.git(&r.main, &["status", "--porcelain"]), "");
    assert!(!r.main.join("new").exists());
    assert_eq!(
        r.git(&r.main, &["diff", "main...a/x", "--name-only"]),
        "README\nnew/deep/file"
    );
    assert!(!r.ok(&r.main, &["doctor"]).contains("discard-copy"));
}

#[test]
fn doctor_survives_what_the_copy_check_cannot_hash() {
    use std::os::unix::ffi::OsStrExt;
    let r = Repo::new("copy-unhashable");
    r.ok(&r.main, &["wt", "new", "a/x"]);
    r.commit_in(&r.wt("a/x"), "README", "changed\n");
    std::fs::write(r.main.join("README"), "changed\n").unwrap();
    // An untracked nested repository, and a file whose name is not UTF-8.
    let nested = r.main.join("nested");
    std::fs::create_dir_all(&nested).unwrap();
    r.git(&nested, &["init", "-q"]);
    assert!(r.ok(&r.main, &["doctor"]).contains("ok"));
    std::fs::write(
        r.main.join(std::ffi::OsStr::from_bytes(b"bad-\xff-name")),
        "x\n",
    )
    .unwrap();
    let doc = r.ok(&r.main, &["doctor"]);
    assert!(doc.contains("ok"), "{doc}");
    assert!(!doc.contains("discard-copy"), "{doc}");
    assert!(r.ok(&r.main, &["audit"]).contains("doctor"));
    let out = r.fails(&r.main, &["wt", "discard-copy", "a/x"]);
    assert!(out.contains("nothing discarded"), "{out}");
    assert_eq!(
        std::fs::read_to_string(r.main.join("README")).unwrap(),
        "changed\n"
    );
    assert!(nested.join(".git").exists());
}

#[test]
fn discard_copy_refuses_a_file_that_becomes_a_directory_or_back() {
    let r = Repo::new("copy-filedir");
    std::fs::write(r.main.join(".gitignore"), "*.log\n").unwrap();
    std::fs::write(r.main.join("gone"), "file\n").unwrap();
    std::fs::create_dir_all(r.main.join("d")).unwrap();
    std::fs::write(r.main.join("d/x"), "x\n").unwrap();
    r.git(&r.main, &["add", "-A"]);
    r.git(&r.main, &["commit", "-qm", "base"]);

    // gone: a file on the trunk, a directory on the branch.
    r.ok(&r.main, &["wt", "new", "a/x"]);
    let wt = r.wt("a/x");
    std::fs::remove_file(wt.join("gone")).unwrap();
    std::fs::create_dir_all(wt.join("gone")).unwrap();
    std::fs::write(wt.join("gone/q"), "q\n").unwrap();
    r.git(&wt, &["add", "-A"]);
    r.git(&wt, &["commit", "-qm", "file to dir"]);
    std::fs::remove_file(r.main.join("gone")).unwrap();
    std::fs::create_dir_all(r.main.join("gone")).unwrap();
    std::fs::write(r.main.join("gone/q"), "q\n").unwrap();
    std::fs::write(r.main.join("gone/p.log"), "mine\n").unwrap();
    assert!(!r.ok(&r.main, &["doctor"]).contains("discard-copy"));
    let out = r.fails(&r.main, &["wt", "discard-copy", "a/x"]);
    assert!(out.contains("nothing discarded"), "{out}");
    assert_eq!(
        std::fs::read_to_string(r.main.join("gone/p.log")).unwrap(),
        "mine\n"
    );
    assert!(r.main.join("gone/q").exists());
    std::fs::remove_dir_all(r.main.join("gone")).unwrap();
    r.git(&r.main, &["checkout", "--", "gone"]);

    // d: a directory on the trunk, a symlink on the branch.
    r.ok(&r.main, &["wt", "new", "b/y"]);
    let wt = r.wt("b/y");
    std::fs::remove_dir_all(wt.join("d")).unwrap();
    std::os::unix::fs::symlink("README", wt.join("d")).unwrap();
    r.git(&wt, &["add", "-A"]);
    r.git(&wt, &["commit", "-qm", "dir to link"]);
    std::fs::remove_dir_all(r.main.join("d")).unwrap();
    std::os::unix::fs::symlink("README", r.main.join("d")).unwrap();
    assert!(!r.ok(&r.main, &["doctor"]).contains("discard-copy"));
    let out = r.fails(&r.main, &["wt", "discard-copy", "b/y"]);
    assert!(out.contains("nothing discarded"), "{out}");
    let status = r.git(&r.main, &["status", "--porcelain"]);
    assert!(
        status.contains(" D d/x") && status.contains("?? d"),
        "{status}"
    );
}

#[test]
fn discard_copy_refuses_an_ignored_directory_where_the_branch_deletes_a_file() {
    let r = Repo::new("copy-ignored-dir");
    std::fs::write(r.main.join(".gitignore"), "*.log\n").unwrap();
    std::fs::write(r.main.join("gone"), "file\n").unwrap();
    r.git(&r.main, &["add", "-A"]);
    r.git(&r.main, &["commit", "-qm", "base"]);
    r.ok(&r.main, &["wt", "new", "a/x"]);
    let wt = r.wt("a/x");
    r.git(&wt, &["rm", "-q", "gone"]);
    r.git(&wt, &["commit", "-qm", "drop gone"]);
    // The trunk lost `gone` too, and has only an ignored file where it was.
    std::fs::remove_file(r.main.join("gone")).unwrap();
    std::fs::create_dir_all(r.main.join("gone")).unwrap();
    std::fs::write(r.main.join("gone/p.log"), "mine\n").unwrap();
    assert!(!r.ok(&r.main, &["doctor"]).contains("discard-copy"));
    let out = r.fails(&r.main, &["wt", "discard-copy", "a/x"]);
    assert!(out.contains("nothing discarded"), "{out}");
    assert_eq!(
        std::fs::read_to_string(r.main.join("gone/p.log")).unwrap(),
        "mine\n"
    );
}

/// The branch adds `name` holding what the tracked `twin` holds; the trunk has an
/// untracked `name` with other content. A name git reads as quoted, or with its
/// trailing CR stripped, must not be hashed as the twin.
fn untracked_name_is_not_its_twin(label: &str, name: &str, twin: &str) {
    let r = Repo::new(label);
    std::fs::write(r.main.join(twin), "same\n").unwrap();
    r.git(&r.main, &["add", "-A"]);
    r.git(&r.main, &["commit", "-qm", "base"]);
    r.ok(&r.main, &["wt", "new", "a/x"]);
    let wt = r.wt("a/x");
    std::fs::write(wt.join(name), "same\n").unwrap();
    r.git(&wt, &["add", "-A"]);
    r.git(&wt, &["commit", "-qm", "add"]);
    std::fs::write(r.main.join(name), "mine\n").unwrap();
    assert!(!r.ok(&r.main, &["doctor"]).contains("discard-copy"));
    r.fails(&r.main, &["wt", "discard-copy", "a/x"]);
    assert_eq!(
        std::fs::read_to_string(r.main.join(name)).unwrap(),
        "mine\n"
    );
}

#[test]
fn discard_copy_hashes_a_name_starting_with_a_quote_as_itself() {
    untracked_name_is_not_its_twin("copy-quote", "\"x\"", "x");
}

#[test]
fn discard_copy_hashes_a_name_ending_in_cr_as_itself() {
    untracked_name_is_not_its_twin("copy-cr", "y\r", "y");
}

// --- audit: how a repository has used 5W ------------------------------------------------

impl Repo {
    /// Run 5w with the commit dates set, so durations in the history are known.
    fn ok_at(&self, when: &str, args: &[&str]) -> String {
        let mut c = Command::new(bin5w());
        c.args(args).current_dir(&self.main);
        env(&mut c, &self.root);
        c.env("GIT_COMMITTER_DATE", when)
            .env("GIT_AUTHOR_DATE", when);
        let o = c.output().unwrap();
        let out = format!(
            "{}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        );
        assert!(o.status.success(), "5w {args:?} failed:\n{out}");
        out
    }
}

#[test]
fn audit_reads_the_queue_history_and_writes_nothing() {
    let r = Repo::new("audit");
    let at = |h: u32| format!("2026-01-01T{h:02}:00:00Z");
    r.ok_at(&at(0), &["add", "parse the header", "area:core", "level:2"]);
    r.ok_at(
        &at(0),
        &["add", "write the docs", "area:docs", "level:1", "needs:#1"],
    );
    r.ok(&r.main, &["wt", "new", "core/header"]);
    let wt = r.wt("core/header");
    r.commit_in(&wt, "header.rs", "one\n");
    r.ok_at(&at(1), &["submit", "1", "core/header"]);
    r.ok_at(&at(2), &["reject", "1", "the parser drops the | case"]);
    r.commit_in(&wt, "header.rs", "two\n");
    r.ok_at(&at(3), &["submit", "1"]);
    r.ok_at(&at(5), &["accept", "1"]);
    r.ok_at(&at(6), &["done", "2", "--self"]);
    r.ok_at(&at(7), &["open", "2"]);

    // By hand: a fenced example row, a NUL in a body, #2 closed with no via:,
    // and a README change in the same commit.
    let t = r.tasks().replace(
        "## Open",
        "```\n- [ ] #1 an example, not a task\n```\n\n## Open",
    );
    let line = r.line(2);
    let t = t.replace(
        &line,
        &format!(
            "{}\n  a body with a \u{0} in it",
            line.replace("[ ]", "[x]")
        ),
    );
    std::fs::write(r.main.join("TASKS.md"), t).unwrap();
    std::fs::write(r.main.join("README"), "changed\n").unwrap();
    r.git(&r.main, &["add", "TASKS.md", "README"]);
    let mut c = Command::new("git");
    c.args(["commit", "-qm", "docs are done"])
        .current_dir(&r.main);
    env(&mut c, &r.root);
    c.env("GIT_COMMITTER_DATE", at(8))
        .env("GIT_AUTHOR_DATE", at(8));
    assert!(c.output().unwrap().status.success());

    r.ok_at(&at(9), &["add", "a third task", "--body", "with a body"]);
    r.fails(&r.main, &["accept", "3"]);
    r.ok(&r.main, &["report", "accept refused"]);

    let tip = r.git(&r.main, &["rev-parse", "main"]);
    let status = r.git(&r.main, &["status", "--porcelain"]);
    // A marker left over from a missed commit, which a write would clear.
    std::fs::write(r.main.join(".git/5w-missed-main"), format!("{tip} {tip}\n")).unwrap();
    // The git dir's entries, and what 5w keeps there.
    let git_dir = || {
        let mut v: Vec<(String, String)> = std::fs::read_dir(r.main.join(".git"))
            .unwrap()
            .flatten()
            .map(|e| {
                let name = e.file_name().to_string_lossy().to_string();
                let body = match name.starts_with("5w-") {
                    true => std::fs::read_to_string(e.path()).unwrap_or_default(),
                    false => String::new(),
                };
                (name, body)
            })
            .collect();
        v.sort();
        v
    };
    let before = git_dir();
    let out = r.ok(&r.main, &["audit"]);
    let has = |s: &str| assert!(out.contains(s), "want {s:?} in:\n{out}");
    has("tasks 3 · 1 open · 0 in review · 2 closed (1 via:review, 1 without via:)");
    has("areas @core 1 · @docs 1 · no area 1");
    has("review 1 accepted · submit→accept median 2h00m · p90 2h00m · max 2h00m #1");
    has("rework 1 rejections on 1 tasks · once 1");
    has("  #1 2026-01-01 the parser drops the | case");
    has("reopened 1 · #2\n");
    has("blocked 1 tasks waited on needs · median 5h00m");
    has("  #2 5h00m on #1\n");
    has("outside 1 queue commits not made by 5w · 1 lint findings");
    has("docs are done — a message 5w never writes; also touches README");
    has("#2: closed without via:");
    has("`5w accept 3` — #3 was never submitted");
    has("1 reports, 1 unsent");
    has("briefs 1 open delegable");
    assert!(!out.contains("note: replaying"), "{out}");
    assert!(
        out.lines()
            .any(|l| l.starts_with("briefs") && l.contains(" #3 ")),
        "{out}"
    );

    // Nothing written: not the trunk, not the checkout, not the git dir.
    assert_eq!(r.git(&r.main, &["rev-parse", "main"]), tip);
    assert_eq!(r.git(&r.main, &["status", "--porcelain"]), status);
    assert_eq!(git_dir(), before);

    let json = r.ok(&r.main, &["audit", "--json"]);
    for want in [
        "\"replay_diverged\":false",
        "\"review\":{\"accepted\":1,\"median_s\":7200",
        "\"rework\":{\"rejections\":1,\"tasks\":1,\"once\":1",
        "\"reason\":\"the parser drops the | case\"",
        "\"reopened\":[{\"id\":2,\"times\":1}]",
        "\"longest\":[{\"id\":2,\"seconds\":18000,\"needs\":[1],\"still\":false}]",
        "\"why\":[\"a message 5w never writes\",\"also touches README\"]",
    ] {
        assert!(json.contains(want), "want {want:?} in:\n{json}");
    }

    // A window: after the accept, by commit and by date.
    let accept = r.git(&r.main, &["log", "--format=%H", "--grep=accept #1", "main"]);
    for since in [accept.as_str(), "2026-01-01T05:30:00Z"] {
        let out = r.ok(&r.main, &["audit", "--since", since]);
        let has = |s: &str| assert!(out.contains(s), "want {s:?} in:\n{out}");
        has("review 0 accepted");
        has("rework 0 rejections");
        has("reopened 1 · #2");
        has("outside 1 queue commits");
        has("tasks 2 ·");
    }
    assert!(
        r.fails(&r.main, &["audit", "--since", "yesterday-ish"])
            .contains("not a commit or a date")
    );
    assert!(
        r.fails(&r.main, &["audit", "--since", "^HEAD"])
            .contains("not a commit or a date")
    );
}

#[test]
fn audit_on_a_repository_with_no_history_is_an_empty_report() {
    let r = Repo::new("audit-empty");
    let bare = r.root.join("bare");
    std::fs::create_dir_all(&bare).unwrap();
    r.git(&bare, &["init", "-q", "-b", "main"]);
    for dir in [&bare, &r.main] {
        let out = r.ok(dir, &["audit"]);
        assert!(out.contains("tasks 0 · 0 open"), "{out}");
        assert!(out.contains("review 0 accepted"), "{out}");
        assert!(out.contains("briefs 0 open delegable"), "{out}");
        assert!(r.ok(dir, &["audit", "--json"]).contains("\"commits\":"));
    }
    assert!(r.ok(&bare, &["audit"]).contains("no queue commits"));
}

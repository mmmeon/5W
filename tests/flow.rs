use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};

static N: AtomicU32 = AtomicU32::new(0);

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
        .env("FIVEW_WT_ROOT", root.join("wt"));
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
        let mut c = Command::new(env!("CARGO_BIN_EXE_5w"));
        c.args(args).current_dir(cwd);
        env(&mut c, &self.root);
        c.output().unwrap()
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
        out
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

    fn line(&self, id: u64) -> String {
        self.tasks()
            .lines()
            .find(|l| l.contains(&format!("] #{id} ")))
            .unwrap_or("")
            .to_string()
    }
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
}

#[test]
fn reject_reason_survives_every_special_character() {
    let r = Repo::new("reject");
    r.ok(&r.main, &["add", "x"]);
    let reason = r#"a | b / c "quoted" \ back"#;
    r.ok(&r.main, &["reject", "1", reason]);
    let show = r.ok(&r.main, &["show", "1"]);
    assert!(show.contains(&format!("rework:    {reason}")), "{show}");
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
    assert!(
        out.contains("gained commits after it was submitted"),
        "{out}"
    );
    r.ok(&r.main, &["accept", "1", "--at", "a/x"]);
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
    assert!(r.line(1).starts_with("- [ ] #1") && !r.line(1).contains("via:"));
    // Open moves it back out of Done.
    let t = r.tasks();
    assert!(t.find("#1 decide").unwrap() < t.find("## Done").unwrap());
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
    r.ok(&r.main, &["done", "1", "--self"]);
    let after = r.tasks();
    assert!(after.contains("```\n- [ ] #1 <what"), "example untouched");
    assert!(after.contains("- [x] #1 real via:self"));
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
            let mut c = Command::new(env!("CARGO_BIN_EXE_5w"));
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
            .contains("Ship the bottom of the stack first")
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
fn duplicate_ids_stop_every_command() {
    let r = Repo::new("dup");
    r.ok(&r.main, &["add", "one"]);
    let t = r
        .tasks()
        .replace("- [ ] #1 one", "- [ ] #1 one\n- [ ] #1 again");
    std::fs::write(r.main.join("TASKS.md"), t).unwrap();
    assert!(r.fails(&r.main, &["ready"]).contains("carries an id twice"));
    assert!(r.fails(&r.main, &["doctor"]).contains("appears twice"));
}

#[test]
fn symlinked_as_tasks_wt_ship() {
    let r = Repo::new("argv0");
    let bin = r.root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_5w"), bin.join("wt")).unwrap();
    let mut c = Command::new(bin.join("wt"));
    c.args(["new", "x/y"]).current_dir(&r.main);
    env(&mut c, &r.root);
    assert!(c.output().unwrap().status.success());
    assert!(r.git(&r.main, &["branch", "--list", "x/y"]).contains("x/y"));
}

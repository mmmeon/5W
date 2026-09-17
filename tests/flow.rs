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
fn reject_refuses_work_that_was_never_submitted() {
    let r = Repo::new("reject-open");
    r.ok(&r.main, &["add", "x"]);
    let out = r.fails(&r.main, &["reject", "1", "not good"]);
    assert!(out.contains("never submitted"), "{out}");
    assert!(r.line(1).starts_with("- [ ] #1") && !r.line(1).contains("rework:"));
    // Closed was never pending review either; `open` alone would not make it rejectable.
    r.ok(&r.main, &["done", "1", "--self"]);
    let out = r.fails(&r.main, &["reject", "1", "not good"]);
    assert!(out.contains("nothing to reject"), "{out}");
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
    r.ok(&r.main, &["accept", "1", "--at", "a/x"]);
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
            && j.contains("\"behind\":1}"),
        "{j}"
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
fn duplicate_ids_stop_every_command() {
    let r = Repo::new("dup");
    r.ok(&r.main, &["add", "one"]);
    let t = r
        .tasks()
        .replace("- [ ] #1 one", "- [ ] #1 one\n- [ ] #1 again");
    std::fs::write(r.main.join("TASKS.md"), t).unwrap();
    assert!(r.fails(&r.main, &["ready"]).contains("duplicate ids"));
    assert!(r.fails(&r.main, &["doctor"]).contains("appears twice"));
}

#[test]
fn a_subcommand_help_prints_only_that_command() {
    let r = Repo::new("subhelp");
    let full = r.ok(&r.main, &["--help"]);
    for (cmd, want) in [
        ("add", "usage: 5w add <text> [fields]"),
        ("submit", "usage: 5w submit <id> [branch]"),
        ("accept", "usage: 5w accept <id> [--at <rev>] [--force]"),
        ("reject", "usage: 5w reject <id> <reason>"),
        ("show", "usage: 5w show <id> [--json]"),
        ("ready", "usage: 5w ready [filters] [out]"),
        ("levels", "usage: 5w blocked | levels | all"),
        ("reopen", "usage: 5w open <id>"),
        ("hook", "usage: 5w hook install | uninstall"),
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
    assert_eq!(r.ok(&r.main, &["branch", "--help"]), full);
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
    let out = r.fails(&r.main, &["ship", "f/a", "--sync"]);
    assert!(out.contains("rebasing") && out.contains("back at"), "{out}");
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
    let json = r.ok(&r.main, &["ready", "--json"]);
    assert!(
        json.starts_with("{\"id\":2,\"state\":\"open\",\"level\":1,"),
        "{json}"
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
    let sha = r.git(&r.main, &["rev-parse", "--short=12", "a/x"]);

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
    ];
    for (from, to, want) in cases {
        let before = r.tasks();
        hand_edit(&r, from, to);
        let out = r.fails(&r.main, &["lint"]);
        assert!(out.contains(want), "want {want:?} in:\n{out}");
        std::fs::write(r.main.join("TASKS.md"), before).unwrap();
        r.git(&r.main, &["add", "TASKS.md"]);
    }

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
    let r = Repo::new("prereceive");
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
    let out = r.fails(&r.main, &["ready"]);
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
fn wt_rm_finds_force_anywhere_in_the_arguments() {
    let r = Repo::new("wt-rm-force");
    r.ok(&r.main, &["wt", "new", "a/x"]);
    let wt = r.wt("a/x");
    std::fs::write(wt.join("scratch"), "dirty").unwrap();
    assert!(r.fails(&r.main, &["wt", "rm", "a/x"]).contains("--force"));
    r.ok(&r.main, &["wt", "rm", "--force", "a/x"]);
    assert!(!wt.exists());
    assert!(
        r.fails(&r.main, &["wt", "rm", "--forse", "a/x"])
            .contains("unknown flag")
    );
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

    // Nothing written: not the trunk, not the checkout.
    assert_eq!(r.git(&r.main, &["rev-parse", "main"]), tip);
    assert_eq!(r.git(&r.main, &["status", "--porcelain"]), status);

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

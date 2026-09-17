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
fn a_flag_wt_report_init_or_lint_does_not_take_is_refused() {
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
    ] {
        let out = r.fails(&r.main, args);
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

    // Signed by a key that is not the release key.
    sign(&other);
    let (ok, out) = rel.run(&rel.root, &path, &["self-update", "--latest"]);
    assert!(
        !ok && out.contains("not signed by the 5W release key"),
        "{out}"
    );
    untouched(&out);

    // Signed, but the binary is not the one the sums name.
    sign(&release);
    let asset = rel.dir().join(Releases::asset());
    let good = std::fs::read(&asset).unwrap();
    std::fs::write(&asset, b"#!/bin/sh\necho evil\n").unwrap();
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
    assert!(r.fails(&r.main, &["wt", "rm", "a/x"]).contains("--force"));
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

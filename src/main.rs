mod audit;
mod ci;
mod config;
mod git;
mod lint;
mod queue;
mod report;
mod ship;
mod store;
mod tasks;
mod tokens;
mod upkeep;
mod util;
mod wt;

use store::Repo;
use util::Res;

const TEMPLATE_TASKS: &str = include_str!("../templates/TASKS.md");
const TEMPLATE_CONFIG: &str = include_str!("../templates/5w.toml");

#[cfg(unix)]
unsafe extern "C" {
    fn signal(sig: i32, handler: usize) -> usize;
}

fn main() {
    // Rust ignores SIGPIPE, so `5w ready | head` panics on the closed pipe. A
    // command-line filter should just stop, as every Unix tool does.
    #[cfg(unix)]
    unsafe {
        signal(13, 0); // SIGPIPE, SIG_DFL
    }
    report::install_panic_hook();
    // Test-only: the panic hook has no other way to be exercised, since no
    // ordinary command should ever crash. Not documented, and setting it does
    // nothing but crash the process on purpose.
    if std::env::var_os("FIVEW_TEST_PANIC").is_some() {
        panic!("FIVEW_TEST_PANIC");
    }
    let argv: Vec<String> = std::env::args().collect();
    // Invoked through a symlink named `tasks`, `wt` or `ship`, behave as that
    // tool — so a repo can keep `bin/tasks` and friends as the spelling.
    let name = argv
        .first()
        .and_then(|a| std::path::Path::new(a).file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut args: Vec<String> = argv.into_iter().skip(1).collect();
    match name.as_str() {
        "wt" => args.insert(0, "wt".into()),
        "ship" => args.insert(0, "ship".into()),
        _ => {}
    }
    let (args, no_color) = take_no_color(args);
    if no_color {
        util::NO_COLOR_FLAG.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    let reporting = args.first().is_some_and(|a| a == "report");
    if let Err(e) = dispatch(args.clone()) {
        if !reporting {
            report::record_failure(&args, &e);
        }
        debug_assert!(!e.contains('\n'), "a refusal is one line: {e:?}");
        eprintln!("{}", util::Sty::stderr().refusal(&e));
        std::process::exit(1);
    }
}

/// `--no-color` is global: taken out wherever it stands as its own argument,
/// so every command accepts it, except where it is text — the value of a flag
/// that takes one (`--body`, `--expected`, `-m`/`--message`), `reject`'s reason
/// and `report`'s text once they begin, and anything after `--`.
fn take_no_color(args: Vec<String>) -> (Vec<String>, bool) {
    let mut out: Vec<String> = Vec::with_capacity(args.len());
    let (mut found, mut text, mut value) = (false, false, false);
    for a in args {
        if text || value {
            value = false;
            out.push(a);
            continue;
        }
        if a == "--no-color" {
            found = true;
            continue;
        }
        text = a == "--"
            || match out.first().map(String::as_str) {
                Some("reject") => out.len() == 2,
                Some("report") => {
                    out.len() == 1
                        && !matches!(
                            a.as_str(),
                            "list" | "ls" | "show" | "rm" | "send" | "help" | "-h" | "--help"
                        )
                }
                _ => false,
            };
        value = matches!(a.as_str(), "--body" | "--expected" | "-m" | "--message");
        out.push(a);
    }
    (out, found)
}

fn dispatch(args: Vec<String>) -> Res<()> {
    let cmd = args.first().cloned().unwrap_or_else(|| "ready".into());
    let rest = if args.is_empty() { &[][..] } else { &args[1..] };
    if matches!(cmd.as_str(), "help" | "-h" | "--help") {
        println!("{}", tasks::USAGE);
        return Ok(());
    }
    if cmd == "--version" || cmd == "-V" {
        println!("{}", upkeep::own_version_line());
        return Ok(());
    }
    // A help flag where an argument belongs is a request for help. Without this
    // `add --help` would have made a task called "--help".
    if rest.first().is_some_and(|a| a == "-h" || a == "--help")
        && cmd != "wt"
        && cmd != "lint"
        && cmd != "ci"
        && cmd != "report"
        && cmd != "audit"
    {
        println!(
            "{}",
            if cmd == "ship" {
                ship::USAGE.to_string()
            } else {
                tasks::command_usage(&cmd).unwrap_or_else(|| tasks::USAGE.to_string())
            }
        );
        return Ok(());
    }
    if cmd == "report" {
        return report::run(rest);
    }
    // Neither needs the project: self-update is for when its pin refuses this binary.
    if cmd == "version" {
        return upkeep::version(rest);
    }
    if cmd == "self-update" {
        return upkeep::self_update(rest);
    }
    git::check_env()?;
    let repo = Repo::open()?;
    match cmd.as_str() {
        "wt" => wt::run(&repo, rest),
        "ship" => ship::run(&repo, rest),
        "init" => match rest.iter().find(|a| a.starts_with("--")) {
            Some(f) => Err(tasks::unknown_flag(&repo, "init", f)),
            None => init(&repo),
        },
        "lint" => lint::run(&repo, rest),
        "audit" => audit::run(&repo, rest),
        "ci" => ci::run(&repo, rest),
        "hook" => lint::hook(&repo, rest),
        "update-files" => upkeep::update_files(&repo, rest),
        _ => tasks::run(&repo, &cmd, rest),
    }
}

/// Write the config and the queue if missing, and commit them on the trunk.
/// This one commit goes through `git commit`, deliberately: it is the only time
/// the tool creates files rather than editing a line.
fn init(repo: &Repo) -> Res<()> {
    let checkout = repo.trunk_checkout()?;
    let p = checkout.as_ref().unwrap_or(&repo.primary);
    let mut created = Vec::new();
    let cfg = p.join(store::CONFIG_FILE);
    if !cfg.exists() {
        std::fs::write(
            &cfg,
            TEMPLATE_CONFIG
                .replace("{trunk}", &repo.trunk)
                .replace("{version}", upkeep::VERSION),
        )
        .map_err(|e| e.to_string())?;
        created.push(store::CONFIG_FILE.to_string());
    }
    let tf = p.join(&repo.cfg.file);
    if !tf.exists() {
        std::fs::write(&tf, TEMPLATE_TASKS).map_err(|e| e.to_string())?;
        created.push(repo.cfg.file.clone());
    }
    let proto = p.join("PROTOCOL.md");
    if !proto.exists() {
        std::fs::write(&proto, upkeep::protocol_text()).map_err(|e| e.to_string())?;
        created.push("PROTOCOL.md".to_string());
    }
    if created.is_empty() {
        println!(
            "init: {} and {} already exist",
            store::CONFIG_FILE,
            repo.cfg.file
        );
        return Ok(());
    }
    for c in &created {
        println!("init: wrote {c}");
    }
    if checkout.is_none() {
        println!(
            "init: the primary worktree is not on {}; commit these there yourself",
            repo.trunk
        );
        return Ok(());
    }
    let mut add = vec!["add", "--"];
    add.extend(created.iter().map(|s| s.as_str()));
    git::git(p, &add)?;
    let mut commit = vec!["commit", "-q", "-m", "chore(tasks): adopt 5W", "--"];
    commit.extend(created.iter().map(|s| s.as_str()));
    let unborn = git::rev(p, "HEAD").is_none();
    if unborn {
        // No commit yet: a pathspec commit is refused on an unborn branch.
        commit.truncate(4);
    }
    git::git(p, &commit)?;
    println!("init: committed on {}", repo.trunk);
    Ok(())
}

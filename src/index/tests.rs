use super::*;

/// A run in `cwd` that went well, in session "s".
fn run<'a>(command: &'a str, cwd: &'a str) -> Run<'a> {
    Run {
        command,
        cwd,
        ok: true,
        session: "s",
    }
}

/// An index of `history`, oldest first, committed, with the shell in `cwd`.
fn index(history: &[(&str, &str)], cwd: &str) -> Index {
    let mut index = Index::new();
    for &(command, dir) in history {
        index.push(&run(command, dir)).unwrap();
    }
    index.commit();
    index.set_context(cwd, None);
    index
}

/// `count` unrelated commands in `/elsewhere`, to let time pass.
fn time_passes(index: &mut Index, count: usize) {
    for n in 0..count {
        index
            .push(&run(&format!("echo {n}"), "/elsewhere"))
            .unwrap();
    }
    index.commit();
}

#[test]
fn nothing_matches_nothing() {
    let index = index(&[("git status", "/a")], "/a");
    assert_eq!(index.suggest("ls"), None);
    assert_eq!(index.suggest("git status -sb"), None);
    assert_eq!(index.suggest(""), None, "never for an empty line");
}

#[test]
fn a_command_exactly_as_typed_has_nothing_to_add() {
    let index = index(&[("make", "/a")], "/a");
    assert_eq!(index.suggest("make"), None);
    let index = self::index(&[("make test", "/a"), ("make", "/a")], "/a");
    assert_eq!(index.suggest("make"), Some("make test"));
}

#[test]
fn of_two_run_once_the_more_recent_wins() {
    let index = index(
        &[
            ("git status", "/a"),
            ("git switch main", "/a"),
            ("git stash", "/a"),
        ],
        "/z",
    );
    assert_eq!(index.suggest("git s"), Some("git stash"));
    assert_eq!(index.suggest("git sw"), Some("git switch main"));
}

#[test]
fn a_command_run_often_beats_one_run_once_after_it() {
    let mut index = Index::new();
    for _ in 0..5 {
        index.push(&run("cargo test", "/a")).unwrap();
    }
    index.push(&run("cargo tree", "/a")).unwrap();
    index.commit();
    index.set_context("/z", None);
    assert_eq!(index.suggest("cargo t"), Some("cargo test"));
}

#[test]
fn but_runs_long_ago_fade_and_the_recent_one_wins() {
    let mut index = Index::new();
    for _ in 0..5 {
        index.push(&run("cargo test", "/a")).unwrap();
    }
    index.commit();
    time_passes(&mut index, 3000);
    index.push(&run("cargo tree", "/a")).unwrap();
    index.commit();
    index.set_context("/z", None);
    assert_eq!(index.suggest("cargo t"), Some("cargo tree"));
}

#[test]
fn a_command_run_in_this_directory_comes_before_a_more_recent_one_elsewhere() {
    let mut index = index(
        &[
            ("make here", "/work/here"),
            ("make elsewhere", "/work/other"),
        ],
        "/work/here",
    );
    assert_eq!(index.suggest("make "), Some("make here"));
    index.set_context("/work/other", None);
    assert_eq!(index.suggest("make "), Some("make elsewhere"));
    index.set_context("/nowhere", None);
    assert_eq!(
        index.suggest("make "),
        Some("make elsewhere"),
        "the recent one"
    );
}

#[test]
fn a_command_run_elsewhere_in_the_project_comes_before_one_outside_it() {
    let mut index = index(
        &[
            ("cargo run --bin tool", "/p/crate/src"),
            ("cargo run --example demo", "/q"),
        ],
        "/p",
    );
    assert_eq!(index.suggest("cargo r"), Some("cargo run --example demo"));
    index.set_context("/p/crate", Some("/p"));
    assert_eq!(index.suggest("cargo r"), Some("cargo run --bin tool"));
    // A directory whose name starts like the project's is not in it.
    index.set_context("/p2", Some("/p2"));
    assert_eq!(index.suggest("cargo r"), Some("cargo run --example demo"));
}

#[test]
fn the_project_keeps_up_with_commands_run_while_the_shell_is_in_it() {
    let mut index = Index::new();
    index.set_context("/p/sub", Some("/p"));
    index.push(&run("make install", "/p/other")).unwrap();
    index.commit();
    // More recent, and outside the project.
    index.push(&run("make all", "/q")).unwrap();
    index.commit();
    assert_eq!(index.suggest("make "), Some("make install"));
}

#[test]
fn what_usually_comes_next_comes_first() {
    let mut index = Index::new();
    for _ in 0..4 {
        index.push(&run("git add -A", "/a")).unwrap();
        index.push(&run("git commit", "/a")).unwrap();
        index.push(&run("git checkout main", "/a")).unwrap();
    }
    index.commit();
    index.set_context("/a", None);
    index.set_prev(Some("git add -A"));
    assert_eq!(index.suggest("git c"), Some("git commit"));
    index.set_prev(Some("git commit"));
    assert_eq!(index.suggest("git c"), Some("git checkout main"));
}

#[test]
fn what_followed_in_another_session_does_not_follow() {
    let mut index = Index::new();
    // "vim notes" comes right after an ls in time, but in another session;
    // "vim todo" is the most recent.
    for (command, session) in [
        ("ls", "one"),
        ("vim notes", "two"),
        ("pwd", "one"),
        ("vim todo", "three"),
    ] {
        let run = Run {
            command,
            cwd: "/a",
            ok: true,
            session,
        };
        index.push(&run).unwrap();
    }
    index.commit();
    index.set_prev(Some("ls"));
    assert_eq!(index.suggest("vim "), Some("vim todo"));
}

#[test]
fn a_command_that_failed_counts_for_less() {
    let mut index = Index::new();
    index.push(&run("make test", "/a")).unwrap();
    let failed = Run {
        command: "make tset",
        cwd: "/a",
        ok: false,
        session: "s",
    };
    index.push(&failed).unwrap();
    index.commit();
    index.set_context("/a", None);
    assert_eq!(index.suggest("make t"), Some("make test"));
}

#[test]
fn the_older_orders_are_still_there() {
    let mut index = index(
        &[
            ("make here", "/w/here"),
            ("make again", "/w/other"),
            ("make again", "/w/other"),
            ("make once", "/w/other"),
        ],
        "/w/here",
    );
    let top = |index: &Index, order| {
        index
            .ranked("make ", order)
            .first()
            .map(|id| index.command(id).to_owned())
    };
    assert_eq!(top(&index, Order::Recent).as_deref(), Some("make once"));
    assert_eq!(
        top(&index, Order::RecentHereFirst).as_deref(),
        Some("make here")
    );
    index.set_context("/nowhere", None);
    assert_eq!(
        top(&index, Order::RecentHereFirst).as_deref(),
        Some("make once")
    );
}

#[test]
fn later_commits_are_merged_and_seen() {
    let mut index = index(&[("echo a", "/a"), ("echo c", "/a")], "/a");
    index.push(&run("echo b", "/a")).unwrap();
    index.push(&run("ls", "/b")).unwrap();
    assert_eq!(
        index.suggest("echo "),
        Some("echo c"),
        "not before the commit"
    );
    index.commit();
    assert_eq!(index.suggest("echo "), Some("echo b"));
    assert_eq!(index.suggest("l"), Some("ls"));
    assert_eq!(index.len(), 4);
    assert_eq!(index.rows(), 4);
}

#[test]
fn commands_that_cannot_be_typed_are_refused() {
    let mut index = Index::new();
    let refused = |index: &mut Index, command: &str| index.push(&run(command, "/")).err();
    assert_eq!(refused(&mut index, ""), Some(Refused::NotText));
    assert_eq!(refused(&mut index, "a\nb"), Some(Refused::NotText));
    assert_eq!(refused(&mut index, "a\tb"), Some(Refused::NotText));
    assert_eq!(refused(&mut index, " secret"), Some(Refused::Hidden));
    assert_eq!(
        refused(&mut index, &"x".repeat(5000)),
        Some(Refused::TooLong)
    );
    assert_eq!(refused(&mut index, "fine"), None);
}

#[test]
fn prefixes_with_multibyte_text_work() {
    let index = index(&[("echo \u{65e5}\u{672c}", "/a")], "/a");
    assert_eq!(
        index.suggest("echo \u{65e5}"),
        Some("echo \u{65e5}\u{672c}")
    );
}

#[test]
fn a_prefix_of_nothing_is_none_even_between_commands() {
    let index = index(&[("aa", "/"), ("cc", "/")], "/");
    assert_eq!(index.suggest("b"), None);
}

#[test]
fn clear_forgets_everything() {
    let mut index = index(&[("ls -la", "/")], "/");
    index.clear();
    assert!(index.is_empty());
    assert_eq!(index.suggest("ls"), None);
}

#[test]
fn a_programs_name_is_worth_what_its_commands_are() {
    let index = index(
        &[
            ("cargo build", "/a"),
            ("cargo test", "/a"),
            ("cargo", "/a"),
            ("cargo-deny check", "/a"),
            ("ls", "/a"),
        ],
        "/a",
    );
    assert!(index.word_worth("cargo") > index.word_worth("ls"));
    assert!(index.word_worth("cargo") > index.word_worth("cargo-deny"));
    assert!(index.word_worth("nothing").abs() < f64::EPSILON);
}

#[test]
fn a_lookup_in_a_large_history_is_fast() {
    let mut index = Index::new();
    for n in 0..100_000_u32 {
        let command = format!("cmd{} --flag {}", n % 30_000, n);
        let dir = format!("/d{}", n % 50);
        index.push(&run(&command, &dir)).unwrap();
    }
    index.commit();
    index.set_context("/d7", Some("/d7"));
    index.set_prev(Some("cmd7 --flag 7"));
    let started = std::time::Instant::now();
    for n in 0..1000 {
        let _ = index.suggest(&format!("cmd{}", n % 100));
    }
    let each = started.elapsed() / 1000;
    assert!(
        each < std::time::Duration::from_millis(2),
        "{each:?} a lookup"
    );
}

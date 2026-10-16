use super::*;

fn never(_: &str) -> Option<&'static str> {
    None
}

#[test]
fn with_nothing_suggested_every_character_is_a_key() {
    let typed = type_one("git status", &never);
    assert_eq!((typed.chars, typed.keys), (10, 10));
    assert_eq!(typed.right, [false; AFTER]);
    assert_eq!(typed.long_enough, [true; AFTER]);
}

#[test]
fn a_right_suggestion_is_taken_whole() {
    let typed = type_one("git status", &|line: &str| {
        "git status".starts_with(line).then_some("git status")
    });
    // "g", then the key that takes the rest.
    assert_eq!(typed.keys, 2);
    assert_eq!(typed.right, [true; AFTER]);
}

#[test]
fn a_suggestion_right_in_its_first_words_is_taken_a_word_at_a_time() {
    // Offered "git stash pop" while typing "git status -sb": "git" is taken
    // with one key, then typing goes on.
    let typed = type_one("git status -sb", &|line: &str| {
        "git stash pop".starts_with(line).then_some("git stash pop")
    });
    let plain = type_one("git status -sb", &never);
    assert!(
        typed.keys < plain.keys,
        "{} keys against {}",
        typed.keys,
        plain.keys
    );
    assert_eq!(typed.right, [false; AFTER]);
}

#[test]
fn a_gain_of_one_character_is_not_worth_a_key() {
    assert_eq!(take("ab", "ab", 1), None);
    assert_eq!(take("abc", "abc", 1), Some(3));
    assert_eq!(take("git status", "git stash", 1), Some(3));
    assert_eq!(take("git status", "gxt", 1), None);
}

#[test]
fn short_commands_count_only_where_they_are_long_enough() {
    let typed = type_one("ls", &never);
    assert_eq!(typed.long_enough, [true, false, false]);
    let mut score = Score::default();
    score.add(&typed);
    score.add(&type_one("git status", &|_: &str| Some("git status")));
    assert_eq!(score.commands, 2);
    assert_eq!(score.long_enough, [2, 1, 1]);
    assert_eq!(score.right, [1, 1, 1]);
    assert!((score.right_after(1) - 0.5).abs() < 1e-9);
    assert!(score.saved() > 0.0);
}

fn past(commands: &[(&str, &str)]) -> Vec<Past> {
    commands
        .iter()
        .map(|&(command, cwd)| Past {
            command: command.to_owned(),
            cwd: cwd.to_owned(),
            session: "s".to_owned(),
            ok: true,
        })
        .collect()
}

#[test]
fn a_replay_judges_each_order_on_the_same_history() {
    // In /a the same command is run again and again; elsewhere another
    // starting the same way is run in between.
    let mut commands = Vec::new();
    for _ in 0..20 {
        commands.push(("make test", "/a"));
        commands.push(("make tags", "/b"));
    }
    let past = past(&commands);
    let mut look = Look::default();
    let orders = [Order::Recent, Order::RecentHereFirst, Order::Likely];
    let replayed = replay(
        &past,
        Weights::default(),
        (10, past.len()),
        &orders,
        &mut look,
    );
    let [recent, here, likely] = replayed.orders.as_slice() else {
        panic!("three scores");
    };
    assert_eq!(recent.commands, 30);
    assert!(here.keys < recent.keys, "the directory helps");
    assert!(likely.keys <= here.keys);
    assert!(
        replayed.knowing.keys <= likely.keys,
        "knowing the command is the most"
    );
}

#[test]
fn tuning_never_does_worse_than_where_it_started() {
    let mut commands = Vec::new();
    for n in 0..30 {
        commands.push(("cargo build", "/p"));
        commands.push(if n % 3 == 0 {
            ("cargo test", "/q")
        } else {
            ("cargo bench", "/q")
        });
    }
    let past = past(&commands);
    let mut look = Look::default();
    let start = Weights::default();
    let before = replay(&past, start, (20, past.len()), &[Order::Likely], &mut look);
    let (_, after) = tune(&past, start, (20, past.len()), &mut look);
    assert!(after.keys <= before.orders.first().map_or(u64::MAX, |s| s.keys));
}

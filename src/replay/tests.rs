use super::*;

fn never(_: &str) -> Option<&'static str> {
    None
}

#[test]
fn with_nothing_suggested_every_character_is_a_key() {
    let typed = type_one("git status", Typist::Exact, &never);
    assert_eq!((typed.chars, typed.keys), (10, 10));
    assert_eq!(typed.right, [false; AFTER]);
    assert_eq!(typed.long_enough, [true; AFTER]);
}

#[test]
fn a_right_suggestion_is_taken_whole() {
    let typed = type_one("git status", Typist::Exact, &|line: &str| {
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
    let typed = type_one("git status -sb", Typist::Exact, &|line: &str| {
        "git stash pop".starts_with(line).then_some("git stash pop")
    });
    let plain = type_one("git status -sb", Typist::Exact, &never);
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
    assert_eq!(take("ab", "ab", "a"), None);
    assert_eq!(take("abc", "abc", "a"), Some(3));
    assert_eq!(take("git status", "git stash", "g"), Some(3));
    assert_eq!(take("git status", "gxt", "g"), None);
}

#[test]
fn short_commands_count_only_where_they_are_long_enough() {
    let typed = type_one("ls", Typist::Exact, &never);
    assert_eq!(typed.long_enough, [true, false, false]);
    let mut score = Score::default();
    score.add(&typed);
    score.add(&type_one("git status", Typist::Exact, &|_: &str| {
        Some("git status")
    }));
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
    let ways = [Order::Recent, Order::RecentHereFirst, Order::Likely].map(|order| Way {
        order,
        typist: Typist::Exact,
    });
    let replayed = replay(
        &past,
        Weights::default(),
        (10, past.len()),
        &ways,
        &mut look,
    );
    let [recent, here, likely] = replayed.ways.as_slice() else {
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
    let likely = Way {
        order: Order::Likely,
        typist: Typist::Exact,
    };
    let before = replay(&past, start, (20, past.len()), &[likely], &mut look);
    let (_, after) = tune(&past, start, (20, past.len()), &mut look);
    assert!(after.keys <= before.ways.first().map_or(u64::MAX, |s| s.keys));
}

#[test]
fn a_person_who_types_in_lower_case_takes_the_suggestion_in_its_case() {
    let ask = |line: &str| {
        (text::start("cd ~/Dev/GitHub", line) != Start::No).then_some("cd ~/Dev/GitHub")
    };
    let typed = type_one("cd ~/Dev/GitHub", Typist::Lower, &ask);
    // "c", then the key that takes the rest and puts "D" right.
    assert_eq!(typed.keys, 2);
    assert_eq!(typed.right, [true; AFTER]);
    let exact_only = |line: &str| {
        "cd ~/Dev/GitHub"
            .starts_with(line)
            .then_some("cd ~/Dev/GitHub")
    };
    let typed = type_one("cd ~/Dev/GitHub", Typist::Lower, &exact_only);
    // Found as it is after "c": the line has no letter yet that needs a
    // capital.
    assert_eq!(typed.keys, 2);
    let typed = type_one("cd ~/Dev/GitHub", Typist::Lower, &|line: &str| {
        (line.len() > 5 && "cd ~/Dev/GitHub".starts_with(line)).then_some("cd ~/Dev/GitHub")
    });
    assert_eq!(typed.keys, 15, "never offered for the line as typed");
}

#[test]
fn a_word_taken_in_another_case_counts_from_where_the_line_ends() {
    assert_eq!(
        take("cd ~/Dev/GitHub", "cd ~/Dev/GitHub/x", "cd ~/dev/g"),
        Some(15)
    );
    assert_eq!(
        take("cd ~/Dev/GitHub", "cd ~/Dev/Gitea", "cd ~/dev/g"),
        None
    );
    assert_eq!(
        take("cd ~/Dev/GitHub", "cd ~/Dev/GitHub", "cd ~/dev/g"),
        Some(15)
    );
    assert_eq!(take("cd ~/Dev/GitHub", "cd ~/dev/GitHub", "cd ~/dev"), None);
}

#[test]
fn inkling_finds_what_a_person_typing_in_lower_case_means_and_fish_less_often() {
    // Each is the other's next, so the most recent is never the one meant.
    let mut commands = Vec::new();
    for _ in 0..20 {
        commands.push(("cd ~/Dev/GitHub", "/a"));
        commands.push(("cd ~/dotfiles", "/a"));
    }
    let past = past(&commands);
    let mut look = Look::default();
    let ways = [Order::Recent, Order::RecentHereFirst, Order::Likely].map(|order| Way {
        order,
        typist: Typist::Lower,
    });
    let replayed = replay(
        &past,
        Weights::default(),
        (10, past.len()),
        &ways,
        &mut look,
    );
    let [fish, before, likely] = replayed.ways.as_slice() else {
        panic!("three scores");
    };
    assert!(fish.keys < before.keys, "inkling before minded case");
    assert!(likely.keys < fish.keys, "what comes next, in any case");
}

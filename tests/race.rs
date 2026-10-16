//! The race: how long each line editor takes from a key to the right
//! suggestion on the screen, typing the same thing into the same history.
//!
//! A measurement, not a check: `scripts/race` runs it, in a release build,
//! one contender at a time. Plain bash (which suggests nothing, so only its
//! echo is timed) and inkling are always in it. fish and ble.sh join when the
//! environment names them: `RACE_FISH` (a fish program) and `RACE_BLESH`
//! (ble.sh's built `ble.sh`). `RACE_ROWS` is how many commands the history
//! holds besides the one that is typed (`1000,100000` if not given).
//!
//! Each contender runs on a pseudo-terminal of its own, in a scratch home,
//! and what it writes goes through a whole terminal emulator (the `vt100`
//! crate), which also answers the questions fish and ble.sh ask a terminal.
//! A key counts as done when the cursor's row shows what has been typed, the
//! cursor right after it, and the rest of the command after the cursor.

#[cfg(test)]
mod race {
    use std::fmt::Write as _;
    use std::os::fd::OwnedFd;
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};

    use rustix::event::{PollFd, PollFlags, Timespec, poll};
    use rustix::fs::{Mode, OFlags};
    use rustix::io::Errno;
    use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};
    use rustix::termios::{Winsize, tcsetwinsize};

    const ROWS: u16 = 30;
    const COLS: u16 = 160;
    /// The command that is suggested: the oldest in the history, so that a
    /// search from the newest end has to go through all the rest.
    const TARGET: &str = "git status --short --branch";
    /// What is typed of it, one key at a time.
    const TYPED: &str = "git status";
    /// The pause between keys, so that each contender is idle when a key comes.
    const GAP: Duration = Duration::from_millis(120);
    /// The longest a key is waited on before it counts as missed.
    const LIMIT: Duration = Duration::from_secs(2);
    /// How many times the whole of `TYPED` is typed.
    const ROUNDS: usize = 20;

    struct Term {
        master: OwnedFd,
        child: Child,
        parser: vt100::Parser,
        /// The last few bytes read, to find a question split across reads.
        tail: Vec<u8>,
    }

    fn winsize() -> Winsize {
        Winsize {
            ws_row: ROWS,
            ws_col: COLS,
            ws_xpixel: 0,
            ws_ypixel: 0,
        }
    }

    /// The questions a terminal is asked, and what this one answers. Fish asks
    /// for the primary attributes and waits for them before its first prompt.
    const ANSWERS: &[(&[u8], &[u8])] = &[
        (b"\x1b[c", b"\x1b[?62;22c"),
        (b"\x1b[0c", b"\x1b[?62;22c"),
        (b"\x1b[>c", b"\x1b[>1;4000;0c"),
        (b"\x1b[>0c", b"\x1b[>1;4000;0c"),
    ];

    impl Term {
        fn start(argv: &[String], env: &[(String, String)], dir: &Path) -> Self {
            let master =
                openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY | OpenptFlags::CLOEXEC).unwrap();
            grantpt(&master).unwrap();
            unlockpt(&master).unwrap();
            let name = ptsname(&master, Vec::new()).unwrap();
            let slave = rustix::fs::open(
                name.as_c_str(),
                OFlags::RDWR | OFlags::NOCTTY | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .unwrap();
            tcsetwinsize(&slave, winsize()).unwrap();
            let copy = || Stdio::from(slave.try_clone().unwrap());
            // inkling's own session command makes the pseudo-terminal the
            // controlling one and runs the program; nothing else of inkling's.
            let child = Command::new(env!("CARGO_BIN_EXE_inkling"))
                .arg("__session")
                .arg("--")
                .args(argv)
                .env_clear()
                .envs(env.iter().map(|(k, v)| (k, v)))
                .current_dir(dir)
                .stdin(copy())
                .stdout(copy())
                .stderr(copy())
                .spawn()
                .unwrap();
            drop(slave);
            let flags = rustix::fs::fcntl_getfl(&master).unwrap();
            rustix::fs::fcntl_setfl(&master, flags | OFlags::NONBLOCK).unwrap();
            Self {
                master,
                child,
                parser: vt100::Parser::new(ROWS, COLS, 0),
                tail: Vec::new(),
            }
        }

        fn send(&self, bytes: &[u8]) {
            let mut at = 0;
            for _ in 0..1000 {
                if at >= bytes.len() {
                    break;
                }
                match rustix::io::write(&self.master, &bytes[at..]) {
                    Ok(n) => at = at.saturating_add(n),
                    Err(Errno::AGAIN | Errno::INTR) => {
                        std::thread::sleep(Duration::from_micros(50));
                    }
                    Err(error) => panic!("write: {error}"),
                }
            }
        }

        /// Wait up to `wait` for output, and take in all there is. Returns
        /// whether anything came.
        fn read(&mut self, wait: Duration) -> bool {
            let timeout = Timespec::try_from(wait).unwrap();
            let mut fds = [PollFd::new(&self.master, PollFlags::IN)];
            match poll(&mut fds, Some(&timeout)) {
                Ok(0) | Err(Errno::INTR) => return false,
                Ok(_) => {}
                Err(error) => panic!("poll: {error}"),
            }
            let mut any = false;
            let mut buf = vec![0_u8; 65536];
            for _ in 0..64 {
                match rustix::io::read(&self.master, &mut buf) {
                    Ok(0) | Err(Errno::AGAIN | Errno::IO) => break,
                    Ok(n) => {
                        any = true;
                        self.parser.process(&buf[..n]);
                        self.answer(&buf[..n]);
                    }
                    Err(Errno::INTR) => {}
                    Err(error) => panic!("read: {error}"),
                }
            }
            any
        }

        /// Answer the questions in what was just read.
        fn answer(&mut self, chunk: &[u8]) {
            let mut window = std::mem::take(&mut self.tail);
            let old = window.len();
            window.extend_from_slice(chunk);
            for (question, reply) in ANSWERS {
                let mut from = 0;
                while let Some(at) = window[from..]
                    .windows(question.len())
                    .position(|w| w == *question)
                {
                    let end = from.saturating_add(at).saturating_add(question.len());
                    if end > old {
                        self.send(reply);
                    }
                    from = end;
                }
            }
            // A request for the cursor's position.
            if window
                .windows(4)
                .skip(old.saturating_sub(3))
                .any(|w| w == b"\x1b[6n")
            {
                let (row, col) = self.parser.screen().cursor_position();
                self.send(
                    format!("\x1b[{};{}R", row.saturating_add(1), col.saturating_add(1)).as_bytes(),
                );
            }
            let keep = window.len().saturating_sub(8);
            self.tail = window.split_off(keep);
        }

        /// Read until nothing has come for `quiet`, or `limit` has passed.
        fn settle(&mut self, quiet: Duration, limit: Duration) {
            let started = Instant::now();
            while started.elapsed() < limit {
                if !self.read(quiet) {
                    return;
                }
            }
        }

        /// The text of the cursor's row before the cursor, and from it on.
        fn around_cursor(&self) -> (String, String) {
            let screen = self.parser.screen();
            let (row, col) = screen.cursor_position();
            let line = screen
                .rows(0, COLS)
                .nth(usize::from(row))
                .unwrap_or_default();
            let chars: Vec<char> = line.chars().collect();
            let col = usize::from(col).min(chars.len());
            (chars[..col].iter().collect(), chars[col..].iter().collect())
        }
    }

    impl Drop for Term {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    /// A contender: how to start it in a home that holds the history.
    struct Contender {
        name: &'static str,
        /// Where it is listed.
        order: usize,
        suggests: bool,
        argv: Vec<String>,
        env: Vec<(String, String)>,
    }

    /// A scratch home with the history in both bash's and fish's form.
    struct Home {
        dir: tempfile::TempDir,
    }

    fn filler(n: usize) -> String {
        // Nothing here begins with a "g", so the typed command is the only one
        // that matches from the first key.
        const VERBS: &[&str] = &[
            "ls -la",
            "echo",
            "cd",
            "make",
            "cat",
            "vim",
            "rm -f",
            "cp -r",
            "mv",
            "tar xf",
            "ssh",
            "docker run --rm",
            "cargo build -p",
            "npm run",
            "python3",
            "curl -sS",
        ];
        let verb = VERBS[n.checked_rem(VERBS.len()).unwrap_or(0)];
        format!("{verb} item{n}/part{}", n.checked_rem(97).unwrap_or(0))
    }

    impl Home {
        fn new(rows: usize) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let home = dir.path();
            let mut bash = String::with_capacity(rows.saturating_mul(32));
            let mut fish = String::with_capacity(rows.saturating_mul(48));
            let start: u64 = 1_700_000_000;
            for (n, command) in std::iter::once(TARGET.to_owned())
                .chain((0..rows).map(filler))
                .enumerate()
            {
                bash.push_str(&command);
                bash.push('\n');
                let when = start.saturating_add(u64::try_from(n).unwrap_or(0));
                let _ = writeln!(fish, "- cmd: {command}\n  when: {when}");
            }
            std::fs::write(home.join(".bash_history"), bash).unwrap();
            let fish_dir = home.join(".local/share/fish");
            std::fs::create_dir_all(&fish_dir).unwrap();
            std::fs::write(fish_dir.join("fish_history"), fish).unwrap();
            Self { dir }
        }

        fn path(&self) -> &Path {
            self.dir.path()
        }

        fn env(&self) -> Vec<(String, String)> {
            let home = self.path().display().to_string();
            let inkling_dir = Path::new(env!("CARGO_BIN_EXE_inkling"))
                .parent()
                .unwrap()
                .display()
                .to_string();
            vec![
                ("HOME".into(), home.clone()),
                (
                    "PATH".into(),
                    format!("{inkling_dir}:/usr/local/bin:/usr/bin:/bin"),
                ),
                ("TERM".into(), "xterm-256color".into()),
                ("LANG".into(), "C.UTF-8".into()),
                ("XDG_DATA_HOME".into(), format!("{home}/.local/share")),
                ("XDG_CONFIG_HOME".into(), format!("{home}/.config")),
                ("XDG_CACHE_HOME".into(), format!("{home}/.cache")),
                ("XDG_STATE_HOME".into(), format!("{home}/.local/state")),
                ("HISTFILE".into(), format!("{home}/.bash_history")),
            ]
        }

        fn rc(&self, name: &str, body: &str) -> String {
            let path = self.path().join(name);
            let text = format!(
                "PS1='$ '\nunset PROMPT_COMMAND\nHISTFILE={}/.bash_history\nHISTSIZE=-1\nHISTFILESIZE=-1\n{body}\n",
                self.path().display()
            );
            std::fs::write(&path, text).unwrap();
            path.display().to_string()
        }

        fn bash(&self, name: &'static str, body: &str, suggests: bool) -> Contender {
            let rc = self.rc(&format!("{name}.rc"), body);
            Contender {
                name,
                order: 0,
                suggests,
                argv: vec![
                    "bash".into(),
                    "--noprofile".into(),
                    "--rcfile".into(),
                    rc,
                    "-i".into(),
                ],
                env: self.env(),
            }
        }
    }

    fn contenders(home: &Home) -> Vec<Contender> {
        let mut all = vec![
            home.bash("bash alone (echo only)", "", false),
            home.bash("inkling", "eval \"$(inkling init bash)\"", true),
        ];
        if let Some(fish) = std::env::var_os("RACE_FISH") {
            all.push(Contender {
                name: "fish",
                order: 0,
                suggests: true,
                argv: vec![
                    PathBuf::from(fish).display().to_string(),
                    // Not --no-config: that leaves the history unread. The
                    // scratch home has no configuration of fish's anyway.
                    "-i".into(),
                ],
                env: home.env(),
            });
        }
        if let Some(blesh) = std::env::var_os("RACE_BLESH") {
            let body = format!("source {}", PathBuf::from(blesh).display());
            all.push(home.bash("ble.sh", &body, true));
        }
        for (order, contender) in all.iter_mut().enumerate() {
            contender.order = order;
        }
        all
    }

    #[derive(Default)]
    struct Times {
        echo: Vec<Duration>,
        full: Vec<Duration>,
        missed: usize,
    }

    /// A contender started and waiting at a prompt, with its times so far.
    struct Runner {
        contender: Contender,
        term: Term,
        times: Times,
    }

    /// Send one key and time it: until the cursor's row shows `typed` before
    /// the cursor, and `rest` after it where a suggestion is expected.
    fn one_key(runner: &mut Runner, key: char, typed: &str, rest: &str, timed: bool) {
        let Runner {
            contender,
            term,
            times,
        } = runner;
        let started = Instant::now();
        term.send(key.to_string().as_bytes());
        let (mut echo, mut full) = (None, None);
        while started.elapsed() < LIMIT {
            if !term.read(Duration::from_millis(5)) {
                continue;
            }
            let (before, after) = term.around_cursor();
            if echo.is_none() && before.ends_with(typed) {
                echo = Some(started.elapsed());
            }
            if before.ends_with(typed) && after.starts_with(rest) {
                full = Some(started.elapsed());
            }
            if echo.is_some() && (full.is_some() || !contender.suggests) {
                break;
            }
        }
        if !timed {
            return;
        }
        match (echo, full) {
            (Some(e), Some(f)) => {
                times.echo.push(e);
                times.full.push(f);
            }
            (Some(e), None) if !contender.suggests => times.echo.push(e),
            _ => {
                times.missed = times.missed.saturating_add(1);
                if std::env::var_os("RACE_DUMP").is_some() {
                    let (before, after) = term.around_cursor();
                    eprintln!("{} missed {typed:?}: [{before}|{after}]", contender.name);
                }
            }
        }
    }

    /// Type `TYPED` at a fresh prompt, one key at a time, `rounds` times.
    /// The contenders take turns key by key, so that whatever the machine is
    /// doing at the time weighs on them all alike.
    fn race(runners: &mut [Runner], rounds: usize, timed: bool) {
        for _ in 0..rounds {
            for runner in runners.iter_mut() {
                // Ctrl-C: the line is thrown away and a new prompt drawn.
                runner.term.send(b"\x03");
            }
            for runner in runners.iter_mut() {
                runner
                    .term
                    .settle(Duration::from_millis(250), Duration::from_secs(5));
            }
            for (at, key) in TYPED.char_indices() {
                let typed = &TYPED[..at.saturating_add(key.len_utf8())];
                let rest = &TARGET[typed.len()..];
                // Each key starts the turns with the next contender, so that
                // none always follows the same one (ble.sh keeps the
                // processor busy for a while after each key).
                runners.rotate_left(1);
                for runner in runners.iter_mut() {
                    one_key(runner, key, typed, rest, timed);
                    runner.term.settle(GAP, GAP.saturating_mul(4));
                }
            }
        }
    }

    fn stats(times: &mut [Duration]) -> String {
        if times.is_empty() {
            return format!("{:>27}", "none");
        }
        times.sort_unstable();
        let last = times.len().saturating_sub(1);
        let at = |per_cent: usize| {
            times[(times
                .len()
                .saturating_mul(per_cent)
                .checked_div(100)
                .unwrap_or(0))
            .min(last)]
        };
        let us = |d: Duration| d.as_secs_f64() * 1e6;
        format!(
            "{:>8.0} {:>8.0} {:>8.0}",
            us(at(50)),
            us(at(95)),
            us(*times.last().unwrap())
        )
    }

    #[test]
    #[ignore = "a measurement: scripts/race"]
    fn race_to_the_suggestion() {
        let rows: Vec<usize> = std::env::var("RACE_ROWS")
            .unwrap_or_else(|_| "1000,100000".into())
            .split(',')
            .filter_map(|n| n.trim().parse().ok())
            .collect();
        for rows in rows {
            let home = Home::new(rows);
            eprintln!(
                "\n{} commands in the history; typing {TYPED:?} towards the oldest, {TARGET:?}",
                rows + 1
            );
            eprintln!(
                "{:28} {:>27} {:>27} {:>6}",
                "", "echo  median   p95  worst", "suggestion  median   p95  worst", "missed"
            );
            let only = std::env::var("RACE_ONLY").ok();
            let mut runners: Vec<Runner> = contenders(&home)
                .into_iter()
                .filter(|c| only.as_deref().is_none_or(|name| c.name.starts_with(name)))
                .map(|contender| {
                    let term = Term::start(&contender.argv, &contender.env, home.path());
                    Runner {
                        contender,
                        term,
                        times: Times::default(),
                    }
                })
                .collect();
            for runner in &mut runners {
                // Up and at a prompt: whatever it prints at the start has
                // stopped.
                runner
                    .term
                    .settle(Duration::from_millis(500), Duration::from_secs(60));
            }
            // One untimed round, so that nothing is measured cold.
            race(&mut runners, 1, false);
            race(&mut runners, ROUNDS, true);
            runners.sort_by_key(|runner| runner.contender.order);
            for runner in &mut runners {
                eprintln!(
                    "{:28} {} {} {:>6}",
                    runner.contender.name,
                    stats(&mut runner.times.echo),
                    stats(&mut runner.times.full),
                    runner.times.missed
                );
            }
        }
    }
}

//! The real thing, end to end: inkling and bash on a pseudo-terminal, typed
//! to as a person would, and judged by what a person would see on a screen.

#[cfg(test)]
mod support;

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::support::sandbox::Sandbox;
    use super::support::screen::Screen;
    use super::support::terminal::{Launch, Terminal};

    const HISTORY: &[&str] = &["echo hello world", "git status", "ls -la /tmp"];
    const RIGHT: &[u8] = b"\x1b[C";
    const END: &[u8] = b"\x1b[F";
    const LEFT: &[u8] = b"\x1b[D";
    const ALT_F: &[u8] = b"\x1bf";
    const CTRL_RIGHT: &[u8] = b"\x1b[1;5C";

    fn shell_with_history() -> (Sandbox, Terminal) {
        let sandbox = Sandbox::new();
        sandbox.write_history(HISTORY);
        let shell = sandbox.shell("");
        (sandbox, shell)
    }

    #[test]
    fn with_no_terminal_to_wrap_the_command_still_runs_and_is_told_inkling_is_off() {
        // A shell must come up whatever becomes of the suggestions. Here the
        // proxy cannot be set up at all (there is no terminal), which stands
        // for every reason it might not be.
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_inkling"))
            .args(["run", "--", "sh", "-c", "echo ran-with-$INKLING"])
            .env_remove("INKLING")
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        assert_eq!(String::from_utf8_lossy(&output.stdout), "ran-with-0\n");
        let said = String::from_utf8_lossy(&output.stderr);
        assert!(said.contains("running without suggestions"), "{said}");
    }

    #[test]
    fn a_suggestion_appears_dim_after_a_prefix() {
        let (_sandbox, mut shell) = shell_with_history();
        shell.type_and_settle(b"git");
        assert_eq!(shell.cursor_row(), "$ git status");
        assert_eq!(shell.dim_on_cursor_row(), " status");
        // The cursor is where the person's typing is, not after the ghost.
        assert_eq!(shell.screen.cursor().0, "$ git".len());
    }

    #[test]
    fn the_most_recent_command_with_the_prefix_is_the_one_suggested() {
        let sandbox = Sandbox::new();
        sandbox.write_history(&["echo one", "echo two", "ls"]);
        let mut shell = sandbox.shell("");
        shell.type_and_settle(b"echo");
        assert_eq!(shell.dim_on_cursor_row(), " two");
    }

    #[test]
    fn right_at_the_end_takes_all_of_it() {
        let (_sandbox, mut shell) = shell_with_history();
        shell.type_and_settle(b"echo");
        assert_eq!(shell.dim_on_cursor_row(), " hello world");
        shell.type_and_settle(RIGHT);
        assert_eq!(shell.cursor_row(), "$ echo hello world");
        assert_eq!(shell.dim_on_cursor_row(), "");
        assert_eq!(shell.screen.cursor().0, "$ echo hello world".len());
        shell.command("");
        shell.type_and_settle(b"echo");
        shell.type_and_settle(RIGHT);
        shell.send(b"\r");
        assert!(shell.wait_text("hello world\n$"), "{}", shell.screen.text());
    }

    #[test]
    fn end_and_ctrl_e_take_it_too() {
        for key in [END, b"\x05", b"\x1bOF", b"\x1b[4~"] {
            let (_sandbox, mut shell) = shell_with_history();
            shell.type_and_settle(b"git");
            shell.type_and_settle(key);
            assert_eq!(shell.cursor_row(), "$ git status", "{key:?}");
            assert_eq!(shell.dim_on_cursor_row(), "", "{key:?}");
        }
    }

    #[test]
    fn alt_f_and_ctrl_right_take_one_word_at_a_time() {
        for key in [ALT_F, CTRL_RIGHT] {
            let (_sandbox, mut shell) = shell_with_history();
            shell.type_and_settle(b"ec");
            assert_eq!(shell.dim_on_cursor_row(), "ho hello world");
            shell.type_and_settle(key);
            assert_eq!(shell.cursor_row(), "$ echo hello world", "{key:?}");
            assert_eq!(shell.dim_on_cursor_row(), " hello world", "{key:?}");
            shell.type_and_settle(key);
            assert_eq!(shell.dim_on_cursor_row(), " world", "{key:?}");
            shell.type_and_settle(key);
            assert_eq!(shell.dim_on_cursor_row(), "", "{key:?}");
            assert_eq!(shell.cursor_row(), "$ echo hello world");
        }
    }

    #[test]
    fn typing_something_else_withdraws_it() {
        let (_sandbox, mut shell) = shell_with_history();
        shell.type_and_settle(b"git");
        shell.type_and_settle(b"x");
        assert_eq!(shell.cursor_row(), "$ gitx");
        assert_eq!(shell.dim_on_cursor_row(), "");
        assert!(!shell.screen.any_dim(), "{}", shell.screen.text());
    }

    #[test]
    fn typing_the_next_characters_of_it_keeps_the_rest() {
        let (_sandbox, mut shell) = shell_with_history();
        shell.type_and_settle(b"gi");
        assert_eq!(shell.dim_on_cursor_row(), "t status");
        shell.type_and_settle(b"t");
        assert_eq!(shell.dim_on_cursor_row(), " status");
        shell.type_and_settle(b" st");
        assert_eq!(shell.dim_on_cursor_row(), "atus");
        assert_eq!(shell.cursor_row(), "$ git status");
    }

    #[test]
    fn backspace_brings_it_back_and_an_empty_line_has_none() {
        let (_sandbox, mut shell) = shell_with_history();
        shell.type_and_settle(b"gx");
        assert_eq!(shell.dim_on_cursor_row(), "");
        shell.type_and_settle(b"\x7f");
        assert_eq!(shell.dim_on_cursor_row(), "it status");
        shell.type_and_settle(b"\x7f");
        assert_eq!(shell.cursor_row(), "$");
        assert!(!shell.screen.any_dim());
    }

    #[test]
    fn with_the_cursor_inside_the_line_there_is_nothing_and_end_brings_it_back() {
        let (_sandbox, mut shell) = shell_with_history();
        shell.type_and_settle(b"git");
        shell.type_and_settle(LEFT);
        assert_eq!(shell.cursor_row(), "$ git");
        assert_eq!(shell.dim_on_cursor_row(), "");
        // Moving right with nothing to take is just moving.
        shell.type_and_settle(RIGHT);
        assert_eq!(shell.dim_on_cursor_row(), " status");
        shell.type_and_settle(LEFT);
        shell.type_and_settle(END);
        assert_eq!(shell.dim_on_cursor_row(), " status");
        assert_eq!(shell.screen.cursor().0, "$ git".len());
    }

    #[test]
    fn what_was_run_in_this_session_is_suggested_next() {
        let (_sandbox, mut shell) = shell_with_history();
        // Typed and then run, as a person does: the line is known by then.
        shell.type_and_settle(b"echo brand new");
        shell.send(b"\r");
        shell.wait_prompt();
        shell.type_and_settle(b"echo b");
        assert_eq!(shell.dim_on_cursor_row(), "rand new");
    }

    #[test]
    fn a_command_that_failed_to_parse_as_text_is_never_suggested() {
        let sandbox = Sandbox::new();
        std::fs::write(sandbox.histfile(), b"echo good\necho \xff\xfe bad\n").unwrap();
        let mut shell = sandbox.shell("");
        shell.type_and_settle(b"echo ");
        assert_eq!(shell.dim_on_cursor_row(), "good");
    }

    #[test]
    fn the_shell_still_sees_its_own_status_and_last_argument() {
        let (_sandbox, mut shell) = shell_with_history();
        shell.command("false");
        shell.send(b"echo st=$?");
        shell.settle(Duration::from_millis(80));
        shell.send(b"\r");
        assert!(shell.wait_text("st=1"), "{}", shell.screen.text());
        shell.wait_prompt();
        shell.command("true lastarg");
        shell.send(b"echo us=$_");
        shell.settle(Duration::from_millis(80));
        shell.send(b"\r");
        assert!(shell.wait_text("us=lastarg"), "{}", shell.screen.text());
    }

    #[test]
    fn tab_completion_cycles_as_it_does_without_inkling() {
        let run = |env: &[(&str, &str)]| {
            let sandbox = Sandbox::new();
            sandbox.write_history(HISTORY);
            std::fs::create_dir_all(sandbox.home().join("files")).unwrap();
            for name in ["aa1", "aa2", "aa3"] {
                std::fs::write(sandbox.home().join("files").join(name), b"").unwrap();
            }
            let mut shell = sandbox.shell_with("bind 'TAB: menu-complete'\ncd files", env);
            shell.type_and_settle(b"ls a");
            shell.type_and_settle(b"\t");
            shell.type_and_settle(b"\t");
            shell.cursor_row()
        };
        let plain = run(&[("INKLING", "0")]);
        let with = run(&[]);
        assert_eq!(plain, "$ ls aa2");
        assert_eq!(with, plain);
    }

    #[test]
    fn two_kills_in_a_row_are_still_one_piece_to_yank() {
        let run = |env: &[(&str, &str)]| {
            let (_sandbox, mut shell) = {
                let sandbox = Sandbox::new();
                sandbox.write_history(&["unrelated"]);
                let shell = sandbox.shell_with("", env);
                (sandbox, shell)
            };
            shell.type_and_settle(b"one two three");
            shell.type_and_settle(b"\x17");
            shell.type_and_settle(b"\x17");
            shell.type_and_settle(b"\x19");
            shell.cursor_row()
        };
        let plain = run(&[("INKLING", "0")]);
        assert_eq!(plain, "$ one two three");
        assert_eq!(run(&[]), plain);
    }

    #[test]
    fn running_programs_get_exactly_the_keys_typed() {
        let (_sandbox, mut shell) = shell_with_history();
        shell.send(b"cat\r");
        shell.settle(Duration::from_millis(100));
        let before = shell.raw.len();
        shell.send(b"git\r");
        shell.settle(Duration::from_millis(100));
        shell.send(b"\x04");
        shell.wait_prompt();
        let seen = String::from_utf8_lossy(shell.raw.get(before..).unwrap()).into_owned();
        assert!(!seen.contains("\x1b7"), "something was drawn: {seen:?}");
        assert!(!seen.contains("\x1b[2m"), "something was drawn: {seen:?}");
        assert!(!seen.contains('^'), "a key leaked to cat: {seen:?}");
        assert_eq!(seen.matches("git").count(), 2, "{seen:?}");
        // And the prompt works again.
        shell.type_and_settle(b"git");
        assert_eq!(shell.dim_on_cursor_row(), " status");
    }

    /// Type ahead while `sleep` runs, and see what the screen is after.
    fn typed_ahead(env: &[(&str, &str)]) -> (String, String, Terminal, Sandbox) {
        let sandbox = Sandbox::new();
        sandbox.write_history(HISTORY);
        let mut shell = sandbox.shell_with("", env);
        shell.send(b"sleep 1\r");
        shell.settle(Duration::from_millis(150));
        let before = shell.raw.len();
        shell.send(b"git");
        shell.settle(Duration::from_millis(300));
        let during = String::from_utf8_lossy(shell.raw.get(before..).unwrap()).into_owned();
        assert!(!shell.screen.any_dim());
        shell.wait_for(Duration::from_secs(5), |s| s.text().ends_with("$ git"));
        shell.settle(Duration::from_millis(100));
        let after = shell.screen.text();
        (during, after, shell, sandbox)
    }

    #[test]
    fn nothing_is_drawn_and_nothing_leaks_while_sleep_runs() {
        let (_, plain, _plain_shell, _a) = typed_ahead(&[("INKLING", "0")]);
        let (during, after, mut shell, _b) = typed_ahead(&[]);
        assert!(!during.contains("\x1b7"), "something was drawn: {during:?}");
        assert!(!during.contains('^'), "a key leaked: {during:?}");
        // What was typed ahead lands in the line exactly as without inkling.
        assert_eq!(after, plain);
        shell.type_and_settle(b" ");
        shell.type_and_settle(b"s");
        assert_eq!(shell.dim_on_cursor_row(), "tatus");
    }

    fn editor() -> Option<&'static str> {
        ["vim", "nvim"].into_iter().find(|name| {
            std::env::var_os("PATH").is_some_and(|paths| {
                std::env::split_paths(&paths).any(|dir| dir.join(name).is_file())
            })
        })
    }

    #[test]
    fn a_full_screen_editor_gets_its_keys_and_nothing_is_drawn() {
        let Some(editor) = editor() else {
            eprintln!("no vim or nvim here: skipped");
            return;
        };
        let (_sandbox, mut shell) = shell_with_history();
        shell.send(format!("{editor} -u NONE -n\r").as_bytes());
        assert!(
            shell.wait_for(Duration::from_secs(10), Screen::on_alternate_screen),
            "{}",
            shell.screen.text()
        );
        shell.settle(Duration::from_millis(300));
        let before = shell.raw.len();
        shell.send(b"ihello git");
        shell.settle(Duration::from_millis(300));
        assert_eq!(shell.screen.row(0), "hello git", "{}", shell.screen.text());
        shell.send(b"\x1b");
        shell.settle(Duration::from_millis(200));
        shell.send(b":q!\r");
        shell.wait_prompt();
        let seen = String::from_utf8_lossy(shell.raw.get(before..).unwrap()).into_owned();
        assert!(
            !seen.contains("\x1b[2mit status"),
            "a suggestion was drawn: {seen:?}"
        );
        assert!(!seen.contains("^X"), "a key leaked: {seen:?}");
        shell.type_and_settle(b"git");
        assert_eq!(shell.dim_on_cursor_row(), " status");
    }

    const BLOCK: &str = "0123456789abcdef";

    fn check_paste(shell: &mut Terminal, bracketed: bool) -> Duration {
        let block = BLOCK.repeat(125);
        assert_eq!(block.len(), 2000);
        let mut paste = Vec::new();
        if bracketed {
            paste.extend_from_slice(b"\x1b[200~");
        }
        paste.extend_from_slice(format!("x={block}").as_bytes());
        if bracketed {
            paste.extend_from_slice(b"\x1b[201~");
        }
        let started = Instant::now();
        shell.send(&paste);
        shell.settle(Duration::from_millis(60));
        let took = started.elapsed();
        shell.send(b"\r");
        shell.wait_prompt();
        shell.command("y=; for ((i=0;i<125;i++)); do y+=0123456789abcdef; done");
        shell.command("[[ $x == \"$y\" ]] && echo SAME-${#x} || echo DIFFERENT-${#x}");
        assert!(shell.wait_text("SAME-2000"), "{}", shell.screen.text());
        took
    }

    #[test]
    fn a_pasted_block_arrives_intact_and_quickly() {
        for bracketed in [true, false] {
            let (_sandbox, mut shell) = shell_with_history();
            let took = check_paste(&mut shell, bracketed);
            assert!(
                took < Duration::from_secs(2),
                "bracketed={bracketed}: {took:?}"
            );
        }
    }

    #[test]
    fn the_window_size_reaches_the_shell_after_a_resize() {
        let (_sandbox, mut shell) = shell_with_history();
        shell.command("stty size");
        assert!(shell.wait_text("24 80"), "{}", shell.screen.text());
        shell.resize(31, 97);
        shell.settle(Duration::from_millis(100));
        shell.command("stty size");
        assert!(shell.wait_text("31 97"), "{}", shell.screen.text());
        shell.command("echo $LINES-$COLUMNS");
        assert!(shell.wait_text("31-97"), "{}", shell.screen.text());
    }

    #[test]
    fn the_exit_status_comes_back() {
        for code in [0, 7, 130] {
            let (_sandbox, mut shell) = shell_with_history();
            shell.send(format!("exit {code}\r").as_bytes());
            let status = shell
                .exit_status(Duration::from_secs(10))
                .expect("it exits");
            assert_eq!(status.code(), Some(code));
        }
    }

    #[test]
    fn a_program_killed_by_a_signal_is_reported_as_a_shell_would() {
        let sandbox = Sandbox::new();
        let mut shell = sandbox.shell("");
        shell.send(b"exec sh -c 'kill -TERM $$'\r");
        let status = shell
            .exit_status(Duration::from_secs(10))
            .expect("it exits");
        assert_eq!(status.code(), Some(143));
    }

    #[test]
    fn with_inkling_off_the_shell_is_not_wrapped_at_all() {
        let sandbox = Sandbox::new();
        sandbox.write_history(HISTORY);
        let mut off = sandbox.shell_with("", &[("INKLING", "0")]);
        off.command("cat /proc/$PPID/comm");
        assert!(
            !off.screen.text().contains("inkling"),
            "{}",
            off.screen.text()
        );
        off.type_and_settle(b"git");
        assert_eq!(off.dim_on_cursor_row(), "");
        off.command("");
        off.command("echo nonce=${INKLING_NONCE-unset}");
        assert!(off.wait_text("nonce=unset"));
        drop(off);

        let mut on = sandbox.shell("");
        on.command("cat /proc/$PPID/comm");
        assert!(on.screen.text().contains("inkling"), "{}", on.screen.text());
    }

    #[test]
    fn a_shell_started_from_the_shell_gets_a_proxy_of_its_own() {
        let (_sandbox, mut shell) = shell_with_history();
        shell.send(b"bash --rcfile ~/rc.sh -i\r");
        shell.settle(Duration::from_millis(500));
        shell.type_and_settle(b"git");
        assert_eq!(shell.dim_on_cursor_row(), " status");
        shell.type_and_settle(b"\x03");
        shell.send(b"exit\r");
        shell.settle(Duration::from_millis(300));
        shell.type_and_settle(b"echo");
        assert_eq!(
            shell.dim_on_cursor_row(),
            " hello world",
            "{}",
            shell.screen.text()
        );
    }

    #[test]
    fn a_search_is_left_alone_and_suggestions_return_after_it() {
        let (_sandbox, mut shell) = shell_with_history();
        shell.type_and_settle(b"\x12");
        shell.type_and_settle(b"git");
        assert!(
            shell.screen.text().contains("reverse-i-search"),
            "{}",
            shell.screen.text()
        );
        assert!(!shell.screen.any_dim());
        shell.type_and_settle(b"\x07");
        shell.send(b"\x15");
        shell.wait_for(Duration::from_secs(2), |s| s.row(s.cursor().1) == "$");
        shell.settle(Duration::from_millis(50));
        shell.type_and_settle(b"git");
        assert_eq!(shell.dim_on_cursor_row(), " status");
    }

    #[test]
    fn commands_run_in_this_directory_come_first() {
        let sandbox = Sandbox::new();
        let here = sandbox.home().display().to_string();
        sandbox.atuin_db(&[("make here", &here), ("make elsewhere", "/elsewhere")]);
        let mut shell = sandbox.shell("");
        shell.type_and_settle(b"make ");
        assert_eq!(shell.dim_on_cursor_row(), "here");
        shell.send(b"\x15");
        shell.settle(Duration::from_millis(60));
        shell.command("cd /tmp");
        shell.type_and_settle(b"make ");
        assert_eq!(shell.dim_on_cursor_row(), "elsewhere");
    }

    struct Tmux(String);

    impl Tmux {
        fn run(&self, args: &[&str]) -> String {
            let output = std::process::Command::new("tmux")
                .args(["-L", &self.0])
                .args(args)
                .output()
                .unwrap();
            String::from_utf8_lossy(&output.stdout).into_owned()
        }

        fn pane(&self) -> String {
            self.run(&["capture-pane", "-p", "-e", "-t", "t"])
        }

        fn wait_for(&self, what: &str, condition: impl Fn(&str) -> bool) -> String {
            let started = Instant::now();
            loop {
                let pane = self.pane();
                if condition(&pane) {
                    return pane;
                }
                assert!(
                    started.elapsed() < Duration::from_secs(8),
                    "{what}: {pane:?}"
                );
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }

    impl Drop for Tmux {
        fn drop(&mut self) {
            self.run(&["kill-server"]);
        }
    }

    /// An independent terminal emulator's view of what is drawn.
    #[test]
    fn tmux_sees_a_faint_suggestion_with_the_cursor_where_the_typing_is() {
        if std::process::Command::new("tmux")
            .arg("-V")
            .output()
            .is_err()
        {
            eprintln!("no tmux here: skipped");
            return;
        }
        let sandbox = Sandbox::new();
        sandbox.write_history(HISTORY);
        let rc = sandbox.rc_around("", "");
        let tmux = Tmux(format!("inkling-test-{}", std::process::id()));
        let env: Vec<String> = sandbox
            .env()
            .into_iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect();
        let command = format!(
            "env -i {} bash --noprofile --rcfile {} -i",
            env.join(" "),
            rc.display()
        );
        tmux.run(&[
            "new-session",
            "-d",
            "-s",
            "t",
            "-x",
            "80",
            "-y",
            "24",
            &command,
        ]);
        tmux.wait_for("a prompt", |pane| pane.starts_with('$'));
        std::thread::sleep(Duration::from_millis(100));
        tmux.run(&["send-keys", "-t", "t", "git"]);
        let pane = tmux.wait_for("the suggestion", |pane| pane.contains("\x1b[2m status"));
        assert!(pane.starts_with("$ git\x1b[2m status"), "{pane:?}");
        assert_eq!(
            tmux.run(&["display", "-p", "-t", "t", "#{cursor_x},#{cursor_y}"])
                .trim(),
            "5,0"
        );
        tmux.run(&["send-keys", "-t", "t", "Right"]);
        let pane = tmux.wait_for("it taken", |pane| pane.starts_with("$ git status"));
        assert!(!pane.contains("\x1b[2m"), "{pane:?}");
        tmux.run(&["send-keys", "-t", "t", "C-u", "echo"]);
        tmux.wait_for("another", |pane| pane.contains("$ echo\x1b[2m hello world"));
        tmux.run(&["send-keys", "-t", "t", "M-f"]);
        tmux.wait_for("one word", |pane| {
            pane.contains("$ echo hello\x1b[2m world")
        });
    }

    #[test]
    fn job_control_works_through_the_proxy() {
        let (_sandbox, mut shell) = shell_with_history();
        shell.send(b"sleep 30\r");
        shell.settle(Duration::from_millis(200));
        shell.send(b"\x1a");
        shell.wait_prompt();
        assert!(
            shell.screen.text().contains("Stopped"),
            "{}",
            shell.screen.text()
        );
        shell.command("jobs");
        assert!(shell.wait_text("sleep 30"), "{}", shell.screen.text());
        shell.send(b"fg\r");
        shell.settle(Duration::from_millis(200));
        shell.send(b"\x03");
        shell.wait_prompt();
        shell.type_and_settle(b"git");
        assert_eq!(shell.dim_on_cursor_row(), " status");
    }

    #[test]
    fn a_lot_of_output_arrives_whole() {
        let (_sandbox, mut shell) = shell_with_history();
        shell.send(b"seq 1 100000\r");
        shell.wait_prompt();
        let text = shell.screen.all_text();
        let numbers: Vec<u32> = text
            .lines()
            .filter_map(|line| line.trim().parse().ok())
            .collect();
        assert_eq!(numbers.len(), 100_000);
        assert!(numbers.windows(2).all(|w| w[1] == w[0] + 1), "out of order");
        assert_eq!(numbers.first(), Some(&1));
    }

    #[test]
    fn a_lot_of_input_arrives_whole() {
        let (_sandbox, mut shell) = shell_with_history();
        shell.command("stty -echo");
        shell.send(b"cat > received\r");
        shell.settle(Duration::from_millis(100));
        let line = "the quick brown fox jumps over the lazy dog 0123456789\n";
        let lines = 20_000;
        for _ in 0..lines {
            shell.send(line.as_bytes());
        }
        shell.send(b"\x04");
        shell.wait_prompt();
        shell.command("wc -c < received");
        assert!(
            shell.wait_text(&(line.len() * lines).to_string()),
            "{}",
            shell.screen.text()
        );
    }

    #[test]
    fn closing_the_terminal_ends_the_shell() {
        let (_sandbox, mut shell) = shell_with_history();
        shell.command("echo shell=$$");
        let text = shell.screen.text();
        let pid: u32 = text
            .lines()
            .find_map(|line| line.strip_prefix("shell="))
            .and_then(|n| n.trim().parse().ok())
            .expect("the shell says its id");
        shell.close_terminal();
        let started = Instant::now();
        let alive = |pid: u32| std::path::Path::new(&format!("/proc/{pid}")).exists();
        while (alive(pid) || !shell.has_exited()) && started.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!alive(pid), "the shell outlived its terminal");
        assert!(shell.has_exited(), "the proxy outlived its terminal");
    }

    #[test]
    fn ctrl_c_and_ctrl_l_leave_the_next_prompt_working() {
        let (_sandbox, mut shell) = shell_with_history();
        shell.type_and_settle(b"git");
        assert_eq!(shell.dim_on_cursor_row(), " status");
        shell.send(b"\x03");
        shell.wait_prompt();
        assert!(!shell.screen.any_dim(), "{}", shell.screen.text());
        shell.type_and_settle(b"git");
        assert_eq!(shell.dim_on_cursor_row(), " status");
        shell.type_and_settle(b"\x0c");
        assert_eq!(shell.cursor_row(), "$ git");
        shell.type_and_settle(b" ");
        assert_eq!(shell.dim_on_cursor_row(), "status");
    }

    #[test]
    fn a_suggestion_that_would_run_past_the_row_is_cut_and_taken_whole() {
        let sandbox = Sandbox::new();
        let long = format!("echo {}", "0123456789".repeat(12));
        sandbox.write_history(&[long.as_str()]);
        let mut shell = sandbox.shell("");
        shell.type_and_settle(b"echo 0123456789012345678901234567890123456789");
        let row = shell.cursor_row();
        assert!(row.starts_with("$ echo 0123"), "{row}");
        let dim = shell.dim_on_cursor_row();
        assert!(dim.ends_with('\u{2026}'), "{dim:?}");
        // Nothing wrapped: the suggestion stayed on its row, and the cursor
        // stayed at the end of what was typed.
        assert_eq!(
            shell.screen.cursor(),
            ("$ echo 0123456789012345678901234567890123456789".len(), 0)
        );
        assert_eq!(shell.screen.row(1), "");
        shell.type_and_settle(b"\x1b[C");
        assert_eq!(
            shell.screen.all_text().replace('\n', ""),
            format!("$ {long}"),
            "taken whole, over two rows"
        );
        assert_eq!(shell.dim_on_cursor_row(), "");
    }

    #[test]
    fn a_resize_at_the_prompt_leaves_it_working() {
        let (_sandbox, mut shell) = shell_with_history();
        shell.type_and_settle(b"git");
        shell.resize(30, 60);
        shell.settle(Duration::from_millis(150));
        assert!(!shell.screen.any_dim(), "{}", shell.screen.text());
        shell.type_and_settle(b" ");
        assert_eq!(shell.dim_on_cursor_row(), "status");
    }

    /// What kitty and ghostty do: start bash in POSIX mode with a script of
    /// their own to run, which takes itself and its marker out of the
    /// environment, turns POSIX mode off, and runs the person's startup.
    #[test]
    fn a_terminals_own_shell_integration_is_replayed_for_the_shell_in_the_proxy() {
        let sandbox = Sandbox::new();
        sandbox.write_history(HISTORY);
        let rc = sandbox.rc_around("", "");
        let integration = sandbox.home().join("terminal-integration.bash");
        std::fs::write(
            &integration,
            format!(
                "[[ -n $TERMINAL_INJECT ]] || return\nprintf 'ran %s\\n' \"$$\" >> \"$HOME/injection.log\"\n\
                 unset TERMINAL_INJECT ENV\nset +o posix\nexport PATH_ADDED_BY_RC=${{PATH_ADDED_BY_RC-}}x\n. {}\n",
                rc.display()
            ),
        )
        .unwrap();
        let mut env = sandbox.env();
        env.push(("ENV".into(), integration.display().to_string()));
        env.push(("TERMINAL_INJECT".into(), "1".into()));
        let argv = ["bash".to_owned(), "--posix".to_owned()];
        let mut shell = Terminal::start(&Launch {
            argv: &argv,
            env,
            dir: &sandbox.home(),
            size: (24, 80),
        });
        assert!(shell.wait_for(Duration::from_secs(10), |s| s.row(s.cursor().1) == "$"));
        shell.settle(Duration::from_millis(100));
        shell.type_and_settle(b"git");
        assert_eq!(shell.dim_on_cursor_row(), " status");
        shell.send(b"\x15");
        shell.settle(Duration::from_millis(50));
        // The shell in the proxy is the one the terminal's script made: out
        // of POSIX mode, and with the environment set once, not twice.
        shell.command("[[ -o posix ]] && echo posix-on || echo posix-off");
        assert!(shell.wait_text("posix-off"), "{}", shell.screen.text());
        shell.command("echo added=$PATH_ADDED_BY_RC");
        assert!(shell.wait_text("added=x\n"), "{}", shell.screen.text());
        let log = std::fs::read_to_string(sandbox.home().join("injection.log")).unwrap();
        assert_eq!(
            log.lines().count(),
            2,
            "the script ran once for each shell: {log}"
        );
    }

    /// What a random session of editing is made of: letters, and the keys
    /// that move, delete, recall and complete. Some are there twice, to come
    /// up more often.
    const EDITING: [&[u8]; 39] = [
        b"a",
        b"b",
        b"c",
        b"e",
        b"g",
        b"i",
        b"s",
        b"t",
        b" ",
        b" ",
        b"-",
        b"x",
        b"\x7f",
        b"\x7f",
        b"\x1b[D",
        b"\x1b[D",
        b"\x1b[3~",
        b"\x01",
        b"\x1bb",
        b"\x17",
        b"\x0b",
        b"\x19",
        b"\x15",
        b"\x14",
        b"\x0c",
        b"\x1b[H",
        b"\x02",
        b"\x04",
        b"\t",
        b"\x1bd",
        b"\x1b.",
        b"\x1bc",
        b"\x1bu",
        b"\x1b\x7f",
        b"\x12",
        b"\x07",
        b"\x18\x15",
        b"\x1b[A",
        b"\x1b[B",
    ];

    /// Ctrl-C, for the sessions that are interrupted now and then.
    const INTERRUPTS: [&[u8]; 2] = [b"\x03", b"\x03"];

    /// The keys that take a suggestion, or part of one.
    const TAKING: [&[u8]; 6] = [
        b"\x1b[C",
        b"\x1b[F",
        b"\x05",
        b"\x1bf",
        b"\x06",
        b"\x1b[1;5C",
    ];

    /// The keys of a random session of editing: no accepting keys unless
    /// `accepting`, since those are meant to differ.
    fn random_editing(seed: u64, keys: usize, accepting: bool, signals: bool) -> Vec<Vec<u8>> {
        let mut state = seed;
        let mut next = move |bound: usize| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            usize::try_from(state.checked_shr(33).unwrap_or(0))
                .unwrap()
                .checked_rem(bound)
                .unwrap_or(0)
        };
        let mut alphabet: Vec<&[u8]> = EDITING.to_vec();
        if signals {
            alphabet.extend(INTERRUPTS);
        }
        if accepting {
            alphabet.extend(TAKING);
        }
        (0..keys)
            .map(|_| {
                alphabet
                    .get(next(alphabet.len()))
                    .copied()
                    .unwrap_or(b"a")
                    .to_vec()
            })
            .collect()
    }

    /// The state of what was typed: the row without any suggestion on it, and
    /// where the cursor is.
    fn typed_state(terminal: &Terminal) -> String {
        let (x, y) = terminal.screen.cursor();
        format!("{}|{x}", terminal.screen.solid_row(y))
    }

    /// How the keys are cut into writes.
    #[derive(Clone, Copy)]
    enum Cut {
        /// Whole keys, a few at a time, as a person's terminal writes them.
        AtKeys,
        /// Anywhere at all, in the middle of escape sequences too.
        Anywhere,
    }

    /// Cut `keys` into writes, by a seeded random choice.
    fn writes(keys: &[Vec<u8>], seed: u64, cut: Cut) -> Vec<Vec<u8>> {
        let mut state = seed;
        let mut next = move |bound: usize| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            usize::try_from(state.checked_shr(40).unwrap_or(0))
                .unwrap()
                .checked_rem(bound)
                .unwrap_or(0)
        };
        let mut out = Vec::new();
        match cut {
            Cut::AtKeys => {
                let mut rest = keys;
                while !rest.is_empty() {
                    let mut take = next(4).saturating_add(1).min(rest.len());
                    // What the shell does with keys that come in the very
                    // write that has Ctrl-C in it is a race even in plain
                    // bash, and a person cannot type that fast: the write
                    // ends with the signal key.
                    if let Some(at) = rest
                        .iter()
                        .take(take)
                        .position(|key| key.as_slice() == b"\x03")
                    {
                        take = at.saturating_add(1);
                    }
                    let (now, later) = rest.split_at(take);
                    out.push(now.iter().flatten().copied().collect());
                    rest = later;
                }
            }
            Cut::Anywhere => {
                let bytes: Vec<u8> = keys.iter().flatten().copied().collect();
                let mut at = 0;
                while at < bytes.len() {
                    let end = at
                        .saturating_add(next(9).saturating_add(1))
                        .min(bytes.len());
                    out.push(bytes.get(at..end).unwrap().to_vec());
                    at = end;
                }
            }
        }
        out
    }

    /// Both shells get the same keys in the same writes and after each write
    /// they must show the same. `with_history` gives both something to
    /// suggest from, and then the keys that take a suggestion are left out,
    /// since those are meant to differ; without history they are tried.
    fn stays_in_step(with_history: bool, signals: bool, cut: Cut) {
        let history = [
            "eab c",
            "git status",
            "abc abc",
            "test it",
            "ls -la",
            "xxit",
        ];
        // More seeds, for a longer search, with FUZZ_SEEDS=100.
        let seeds: u64 = std::env::var("FUZZ_SEEDS")
            .ok()
            .and_then(|n| n.parse().ok())
            .unwrap_or(5);
        for seed in 1..=seeds {
            let keys = random_editing(seed, 60, !with_history, signals);
            let plain_home = Sandbox::new();
            let inkling_home = Sandbox::new();
            if with_history {
                plain_home.write_history(&history);
                inkling_home.write_history(&history);
            }
            let mut plain = plain_home.shell_with("", &[("INKLING", "0")]);
            let mut wrapped = inkling_home.shell("");
            let mut so_far = Vec::new();
            for chunk in writes(&keys, seed, cut) {
                plain.send(&chunk);
                wrapped.send(&chunk);
                so_far.extend_from_slice(&chunk);
                plain.settle(Duration::from_millis(40));
                wrapped.settle(Duration::from_millis(40));
                assert_eq!(
                    typed_state(&wrapped),
                    typed_state(&plain),
                    "seed {seed}, after {so_far:?}"
                );
            }
        }
    }

    #[test]
    fn random_editing_with_suggestions_showing_stays_in_step_with_plain_bash() {
        stays_in_step(true, true, Cut::AtKeys);
    }

    #[test]
    fn random_editing_with_the_taking_keys_and_no_history_stays_in_step() {
        stays_in_step(false, true, Cut::AtKeys);
    }

    #[test]
    fn keys_cut_in_the_middle_of_sequences_stay_in_step_too() {
        stays_in_step(true, false, Cut::Anywhere);
        stays_in_step(false, false, Cut::Anywhere);
    }

    #[test]
    fn the_integration_text_the_program_prints_is_the_one_in_the_library() {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_inkling"))
            .args(["init", "bash"])
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            inkling::init::bash_for(env!("CARGO_BIN_EXE_inkling"))
        );
    }

    fn doctor(sandbox: &Sandbox) -> (bool, String) {
        let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_inkling"));
        command
            .arg("doctor")
            .env_clear()
            .current_dir(sandbox.home());
        for (name, value) in sandbox.env() {
            command.env(name, value);
        }
        let output = command.output().unwrap();
        (
            output.status.success(),
            String::from_utf8(output.stdout).unwrap(),
        )
    }

    #[test]
    fn the_doctor_says_what_it_found() {
        let sandbox = Sandbox::new();
        sandbox.write_history(HISTORY);
        std::fs::write(
            sandbox.home().join(".bashrc"),
            "# >>> inkling >>>\neval \"$(inkling init bash)\"\n# <<< inkling <<<\n",
        )
        .unwrap();
        let (well, report) = doctor(&sandbox);
        assert!(well, "{report}");
        assert!(report.contains("history file at"), "{report}");
        assert!(
            report.contains("3 distinct commands from 3 runs"),
            "{report}"
        );
        assert!(
            report.contains("~/.bashrc:               starts inkling"),
            "{report}"
        );
        assert!(
            report.contains("shell integration:       works with bash 5."),
            "{report}"
        );
        assert!(report.contains("everything needed is in place"), "{report}");

        let sandbox = Sandbox::new();
        sandbox.atuin_db(&[("ls", "/")]);
        std::fs::write(sandbox.home().join(".bashrc"), "alias l=ls\n").unwrap();
        let (_, report) = doctor(&sandbox);
        assert!(report.contains("atuin at"), "{report}");
        assert!(
            report.contains("1 distinct commands from 1 runs"),
            "{report}"
        );
        assert!(report.contains("does not start inkling"), "{report}");
    }

    #[test]
    fn the_doctor_knows_when_it_is_run_inside_the_proxy() {
        let (_sandbox, mut shell) = shell_with_history();
        shell.command("inkling doctor | grep 'this shell is wrapped'");
        assert!(
            shell.wait_text("yes, under the proxy"),
            "{}",
            shell.screen.text()
        );
    }

    #[test]
    fn with_no_history_at_all_the_session_still_learns() {
        let sandbox = Sandbox::new();
        let mut shell = sandbox.shell("");
        shell.type_and_settle(b"echo");
        assert_eq!(shell.dim_on_cursor_row(), "");
        shell.type_and_settle(b" hi");
        shell.send(b"\r");
        shell.wait_prompt();
        shell.type_and_settle(b"echo");
        assert_eq!(shell.dim_on_cursor_row(), " hi");
    }

    #[test]
    fn the_doctor_fails_when_there_is_no_history() {
        let sandbox = Sandbox::new();
        let (well, report) = doctor(&sandbox);
        assert!(!well, "{report}");
        assert!(
            report.contains("there is no history to suggest from"),
            "{report}"
        );
    }

    #[test]
    fn where_there_is_no_terminal_it_says_so_and_runs_the_command_plainly() {
        let sandbox = Sandbox::new();
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_inkling"))
            .args(["run", "--", "sh", "-c", "exit 7"])
            .env("HOME", sandbox.home())
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(7), "the command's own status");
        assert!(String::from_utf8_lossy(&output.stderr).contains("not a terminal"));
    }

    #[test]
    fn a_terminal_that_is_a_terminal_but_launched_without_the_helper_still_works_for_the_exit_status()
     {
        let sandbox = Sandbox::new();
        let argv: Vec<String> = ["inkling", "run", "--", "sh", "-c", "exit 42"]
            .map(String::from)
            .to_vec();
        let mut terminal = Terminal::start(&Launch {
            argv: &argv,
            env: sandbox.env(),
            dir: &sandbox.home(),
            size: (24, 80),
        });
        let status = terminal
            .exit_status(Duration::from_secs(10))
            .expect("it exits");
        assert_eq!(status.code(), Some(42));
    }
}

#[cfg(test)]
mod atuin {
    use std::path::Path;
    use std::time::{Duration, Instant};

    use super::support::sandbox::Sandbox;
    use super::support::terminal::{Launch, Terminal};

    const PREEXEC: &str = "/usr/share/bash-preexec/bash-preexec.sh";

    fn available() -> bool {
        let has_atuin = std::env::var_os("PATH").is_some_and(|paths| {
            std::env::split_paths(&paths).any(|dir| dir.join("atuin").is_file())
        });
        has_atuin && Path::new(PREEXEC).is_file()
    }

    fn setup() -> String {
        format!("source {PREEXEC}\neval \"$(atuin init bash)\"")
    }

    fn recorded(sandbox: &Sandbox) -> Vec<String> {
        let path = sandbox.home().join(".local/share/atuin/history.db");
        let Ok(conn) = rusqlite::Connection::open(path) else {
            return Vec::new();
        };
        let Ok(mut statement) = conn.prepare("SELECT command FROM history ORDER BY timestamp")
        else {
            return Vec::new();
        };
        statement
            .query_map([], |row| row.get::<_, String>(0))
            .map(|rows| rows.filter_map(Result::ok).collect())
            .unwrap_or_default()
    }

    /// A session that runs some commands, so that Atuin has a database with
    /// them in it before the session under test begins.
    fn warm_up(sandbox: &Sandbox, before: &str, after: &str) {
        let mut shell = sandbox.shell_around(before, after, &[]);
        for command in ["true", "echo first", "echo second"] {
            shell.command(command);
        }
        shell.send(b"exit\r");
        shell.exit_status(Duration::from_secs(10)).expect("exits");
        let started = Instant::now();
        while !recorded(sandbox).contains(&"echo second".to_owned()) {
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "{:?}",
                recorded(sandbox)
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        std::thread::sleep(Duration::from_millis(300));
    }

    fn leave(mut shell: Terminal) {
        shell.send(b"\x15exit\r");
        shell.exit_status(Duration::from_secs(10)).expect("exits");
    }

    #[test]
    fn bash_preexec_sees_every_command_with_inkling_before_or_after_it() {
        if !Path::new(PREEXEC).is_file() {
            eprintln!("no bash-preexec here: skipped");
            return;
        }
        for inkling_first in [true, false] {
            let sandbox = Sandbox::new();
            let log = sandbox.home().join("preexec.log");
            let hook = format!(
                "source {PREEXEC}\npreexec() {{ printf '%s\\n' \"$1\" >> {}; }}",
                log.display()
            );
            let (before, after) = if inkling_first {
                (String::new(), hook)
            } else {
                (hook, String::new())
            };
            let mut shell = sandbox.shell_around(&before, &after, &[]);
            shell.command("echo one");
            shell.type_and_settle(b"echo tw");
            shell.type_and_settle(b"o");
            shell.send(b"\r");
            shell.wait_prompt();
            shell.command("echo three");
            leave(shell);
            let seen = std::fs::read_to_string(&log).unwrap();
            let seen: Vec<&str> = seen.lines().collect();
            assert_eq!(
                seen,
                ["echo one", "echo two", "echo three", "exit"],
                "inkling first: {inkling_first}"
            );
        }
    }

    #[test]
    fn atuin_and_bash_preexec_work_alongside_in_either_order() {
        if !available() {
            eprintln!("no atuin or bash-preexec here: skipped");
            return;
        }
        for inkling_first in [true, false] {
            let sandbox = Sandbox::new();
            sandbox.atuin_config();
            let (before, after) = if inkling_first {
                (String::new(), setup())
            } else {
                (setup(), String::new())
            };
            warm_up(&sandbox, &before, &after);

            let mut shell = sandbox.shell_around(&before, &after, &[]);
            shell.type_and_settle(b"echo f");
            assert_eq!(
                shell.dim_on_cursor_row(),
                "irst",
                "inkling first: {inkling_first}"
            );
            shell.send(b"\x15");
            shell.settle(Duration::from_millis(100));
            shell.send(b"echo st=$?");
            shell.settle(Duration::from_millis(100));
            shell.send(b"\r");
            assert!(shell.wait_text("st=0"), "{}", shell.screen.text());
            shell.wait_prompt();
            leave(shell);

            let commands = recorded(&sandbox);
            assert!(
                commands
                    .iter()
                    .all(|c| !c.contains("inkling") && !c.contains("READLINE")),
                "{commands:?}"
            );
            assert!(commands.contains(&"echo st=$?".to_owned()), "{commands:?}");
        }
    }

    const OMARCHY: &str = "/usr/share/omarchy/default/bash";

    /// The startup of this machine's own ~/.bashrc, after the inkling line:
    /// Omarchy's shell setup (starship, zoxide, mise, fzf, completion), then
    /// Atuin on bash-preexec.
    fn the_whole_stack() -> String {
        format!(
            "[[ -r {OMARCHY}/env-bootstrap ]] && source {OMARCHY}/env-bootstrap\n\
             source \"$OMARCHY_PATH/default/bash/rc\"\n\
             source {PREEXEC}\neval \"$(atuin init bash)\"\n"
        )
    }

    fn whole_stack_available() -> bool {
        available()
            && Path::new(OMARCHY).is_dir()
            && std::env::var_os("PATH").is_some_and(|paths| {
                std::env::split_paths(&paths).any(|dir| dir.join("starship").is_file())
            })
    }

    fn start_whole_stack(sandbox: &Sandbox, env: &[(&str, &str)]) -> Terminal {
        let rc = sandbox.home().join("stack.sh");
        let body = format!(
            "HISTFILE={}\nHISTCONTROL=ignoreboth\neval \"$(inkling init bash)\"\n{}",
            sandbox.histfile().display(),
            the_whole_stack()
        );
        std::fs::write(&rc, body).unwrap();
        let argv = Sandbox::bash_argv(&rc);
        let mut vars = sandbox.env();
        vars.retain(|(name, _)| name != "TERM");
        vars.push(("TERM".into(), "xterm-kitty".into()));
        vars.push(("SHELL".into(), "/usr/bin/bash".into()));
        vars.extend(env.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())));
        let mut shell = Terminal::start(&Launch {
            argv: &argv,
            env: vars,
            dir: &sandbox.home(),
            size: (24, 100),
        });
        let prompt = shell.wait_for(Duration::from_secs(20), |screen| {
            screen.row(screen.cursor().1).contains('\u{276f}')
        });
        assert!(prompt, "no prompt:\n{}", shell.screen.all_text());
        shell.settle(Duration::from_millis(300));
        shell
    }

    /// A first session in the whole stack, for Atuin to have a database to
    /// be read.
    fn warm_up_the_whole_stack(sandbox: &Sandbox) {
        let mut shell = start_whole_stack(sandbox, &[("INKLING", "0")]);
        for command in ["true", "echo first", "echo second"] {
            shell.send(format!("{command}\r").as_bytes());
            shell.settle(Duration::from_millis(600));
        }
        shell.send(b"exit\r");
        shell.exit_status(Duration::from_secs(10)).expect("exits");
        std::thread::sleep(Duration::from_millis(500));
    }

    fn at_the_stacks_prompt(shell: &mut Terminal) {
        shell.wait_for(Duration::from_secs(5), |s| {
            s.row(s.cursor().1).contains('\u{276f}')
        });
        shell.settle(Duration::from_millis(300));
    }

    #[test]
    fn this_machines_whole_shell_stack_looks_the_same_and_gains_suggestions() {
        if !whole_stack_available() {
            eprintln!("no Omarchy shell setup, starship or atuin here: skipped");
            return;
        }
        let sandbox = Sandbox::new();
        sandbox.atuin_config();
        warm_up_the_whole_stack(&sandbox);

        // The first screen, without inkling and with it.
        let plain = start_whole_stack(&sandbox, &[("INKLING", "0")]);
        let plain_screen = plain.screen.text();
        drop(plain);
        let mut shell = start_whole_stack(&sandbox, &[]);
        assert_eq!(
            shell.screen.text(),
            plain_screen,
            "the first prompt is the same"
        );

        shell.type_and_settle(b"echo f");
        assert!(
            shell.cursor_row().ends_with("echo first"),
            "{}",
            shell.screen.text()
        );
        assert_eq!(shell.dim_on_cursor_row(), "irst", "{}", shell.screen.text());
        shell.type_and_settle(b"\x1b[C");
        assert!(
            shell.cursor_row().ends_with("echo first"),
            "{}",
            shell.screen.text()
        );
        shell.send(b"\r");
        assert!(shell.wait_text("first\n"), "{}", shell.screen.text());
        at_the_stacks_prompt(&mut shell);
        // Tab completion, as it is without inkling.
        shell.type_and_settle(b"ls /usr/shar");
        shell.type_and_settle(b"\t");
        assert!(
            shell.cursor_row().contains("/usr/share"),
            "{}",
            shell.screen.text()
        );
        shell.send(b"\x15");
        shell.settle(Duration::from_millis(200));
        // Atuin's search, and back.
        shell.send(b"\x1b[A");
        shell.settle(Duration::from_millis(900));
        shell.send(b"\x1b");
        shell.settle(Duration::from_millis(900));
        shell.send(b"\x15");
        shell.settle(Duration::from_millis(200));
        shell.type_and_settle(b"echo s");
        assert_eq!(
            shell.dim_on_cursor_row(),
            "econd",
            "{}",
            shell.screen.text()
        );
        shell.send(b"\x15exit\r");
        shell.exit_status(Duration::from_secs(10)).expect("exits");
    }

    #[test]
    fn the_atuin_search_is_not_disturbed_and_suggestions_return_after_it() {
        if !available() {
            eprintln!("no atuin or bash-preexec here: skipped");
            return;
        }
        let sandbox = Sandbox::new();
        sandbox.atuin_config();
        warm_up(&sandbox, "", &setup());
        let mut shell = sandbox.shell_around("", &setup(), &[]);
        shell.type_and_settle(b"echo s");
        assert_eq!(shell.dim_on_cursor_row(), "econd");
        // Up is Atuin's search, started with what has been typed.
        let before = shell.raw.len();
        shell.send(b"\x1b[A");
        shell.settle(Duration::from_millis(800));
        shell.send(b"ec");
        shell.settle(Duration::from_millis(500));
        let seen = String::from_utf8_lossy(shell.raw.get(before..).unwrap()).into_owned();
        assert!(!seen.contains("^X"), "a probe reached the search: {seen:?}");
        assert!(
            !seen.contains("\x1b[2mecond"),
            "a suggestion was drawn: {seen:?}"
        );
        assert!(
            shell.screen.text().contains("echo second"),
            "{}",
            shell.screen.text()
        );
        // Back out of it, to the line as it was.
        shell.send(b"\x1b");
        shell.settle(Duration::from_millis(800));
        shell.send(b"\x15");
        shell.settle(Duration::from_millis(200));
        shell.type_and_settle(b"echo f");
        assert_eq!(shell.dim_on_cursor_row(), "irst", "{}", shell.screen.text());
        leave(shell);
    }
}

/// Measurements, not checks: run by `scripts/measure`, in a release build,
/// one at a time.
#[cfg(test)]
mod measure {
    use std::time::{Duration, Instant};

    use super::support::sandbox::Sandbox;
    use super::support::terminal::Terminal;

    fn median(times: &mut [Duration]) -> Duration {
        times.sort_unstable();
        times.get(times.len() / 2).copied().unwrap_or_default()
    }

    fn percentile(times: &mut [Duration], per_cent: usize) -> Duration {
        times.sort_unstable();
        let at = times
            .len()
            .saturating_mul(per_cent)
            .checked_div(100)
            .unwrap_or(0)
            .min(times.len().saturating_sub(1));
        times.get(at).copied().unwrap_or_default()
    }

    fn micros(time: Duration) -> String {
        format!("{:7.0} us", time.as_secs_f64() * 1e6)
    }

    fn summary(label: &str, mut times: Vec<Duration>) -> Duration {
        let middle = median(&mut times);
        let worst = times.iter().max().copied().unwrap_or_default();
        eprintln!(
            "{label:36} median {}  p95 {}  worst {}  over {} keys",
            micros(middle),
            micros(percentile(&mut times, 95)),
            micros(worst),
            times.len()
        );
        middle
    }

    const PATTERN: &[u8] = b"git status -sb";

    /// Time from each key to the first byte back, typing one key at a time
    /// with the shell idle between.
    fn key_times(shell: &mut Terminal, keys: usize) -> Vec<Duration> {
        let mut times = Vec::new();
        for n in 0..keys {
            let at = n.checked_rem(PATTERN.len()).unwrap_or(0);
            if at == 0 {
                shell.send(b"\x15");
                shell.settle(Duration::from_millis(30));
            }
            let key = PATTERN.get(at).copied().unwrap();
            times.push(shell.type_timed(&[key]));
            shell.settle(Duration::from_millis(25));
        }
        times
    }

    fn history() -> Vec<&'static str> {
        vec![
            "git status -sb --short",
            "git stash",
            "ls -la /tmp",
            "echo hello world",
        ]
    }

    #[test]
    #[ignore = "a measurement: scripts/measure"]
    fn keystroke_latency() {
        let bare = Sandbox::new();
        bare.write_history(&history());
        let mut shell = bare.start(&bare.bare_rc(), &[]);
        let _ = shell.wait_prompt_start();
        let plain = summary("echo, bash alone", key_times(&mut shell, 300));
        drop(shell);

        let with = Sandbox::new();
        with.write_history(&history());
        let mut shell = with.shell("");
        let wrapped = summary(
            "echo and suggestion, with inkling",
            key_times(&mut shell, 300),
        );
        eprintln!(
            "added by inkling, medians: {}",
            micros(wrapped.saturating_sub(plain))
        );
    }

    #[test]
    #[ignore = "a measurement: scripts/measure"]
    fn startup_cost() {
        let time = |wrapped: bool| {
            let mut times = Vec::new();
            for _ in 0..15 {
                let sandbox = Sandbox::new();
                sandbox.write_history(&history());
                let rc = if wrapped {
                    sandbox.rc_around("", "")
                } else {
                    sandbox.bare_rc()
                };
                let started = Instant::now();
                let mut shell = sandbox.start(&rc, &[]);
                let came = shell.wait_prompt_start();
                times.push(came.duration_since(started));
            }
            times
        };
        let plain = median(&mut time(false));
        let mut wrapped_times = time(true);
        let wrapped = median(&mut wrapped_times);
        let millis = |t: Duration| format!("{:6.1} ms", t.as_secs_f64() * 1e3);
        eprintln!(
            "shell start to first prompt, bash alone:    median {}",
            millis(plain)
        );
        eprintln!(
            "shell start to first prompt, with inkling:  median {}",
            millis(wrapped)
        );
        eprintln!(
            "added by inkling:                           {}",
            millis(wrapped.saturating_sub(plain))
        );
        let mut inits = Vec::new();
        for _ in 0..50 {
            let started = Instant::now();
            let output = std::process::Command::new(env!("CARGO_BIN_EXE_inkling"))
                .args(["init", "bash"])
                .output()
                .unwrap();
            assert!(output.status.success());
            inits.push(started.elapsed());
        }
        eprintln!(
            "`inkling init bash`, run and read:         median {}",
            millis(median(&mut inits))
        );
    }

    #[test]
    #[ignore = "a measurement: scripts/measure"]
    fn output_throughput() {
        let measure = |wrapped: bool| {
            let sandbox = Sandbox::new();
            sandbox.write_history(&history());
            let mut shell = if wrapped {
                sandbox.shell("")
            } else {
                let mut shell = sandbox.start(&sandbox.bare_rc(), &[]);
                let _ = shell.wait_prompt_start();
                shell
            };
            let started = Instant::now();
            shell.send(b"seq 1 2000000\r");
            let bytes = shell.read_until_tail(b"2000000\r\n\x1b[?2004h$ ", Duration::from_secs(60));
            (bytes, started.elapsed())
        };
        for (label, wrapped) in [("bash alone", false), ("with inkling", true)] {
            let (bytes, took) = measure(wrapped);
            eprintln!(
                "{label:14} {} MiB of output in {:5.0} ms, {:6.0} MiB/s",
                bytes / (1024 * 1024),
                took.as_secs_f64() * 1e3,
                f64::from(u32::try_from(bytes).unwrap()) / 1_048_576.0 / took.as_secs_f64()
            );
        }
    }

    fn rss(pid: u32) -> (u64, u64) {
        let status = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap();
        let kib = |name: &str| {
            status
                .lines()
                .find_map(|line| line.strip_prefix(name))
                .and_then(|rest| rest.split_whitespace().next())
                .and_then(|n| n.parse::<u64>().ok())
                .unwrap_or(0)
        };
        (kib("VmRSS:"), kib("VmHWM:"))
    }

    #[test]
    #[ignore = "a measurement: scripts/measure"]
    fn memory_and_load_time() {
        for rows in [0_usize, 10_000, 100_000, 400_000] {
            let sandbox = Sandbox::new();
            if rows > 0 {
                sandbox.big_atuin_db(rows);
            } else {
                sandbox.write_history(&history());
            }
            let started = Instant::now();
            let mut shell = sandbox.start(&sandbox.rc_around("", ""), &[]);
            let up = shell.wait_prompt_start().duration_since(started);
            shell.type_and_settle(b"git --option1");
            let (now, peak) = rss(shell.pid());
            eprintln!(
                "{rows:7} runs in history: proxy resident {:6.1} MiB (peak {:6.1} MiB); prompt after {:5.0} ms",
                f64::from(u32::try_from(now).unwrap()) / 1024.0,
                f64::from(u32::try_from(peak).unwrap()) / 1024.0,
                up.as_secs_f64() * 1e3
            );
        }
    }
}

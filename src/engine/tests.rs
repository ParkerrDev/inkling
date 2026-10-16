//! The engine, driven by a conversation: what the person types, what a shell
//! like bash writes back, and the passing of time.

use std::cell::Cell;

use super::*;

const NONCE: &str = "0123456789abcdef";

struct Fake {
    front: Cell<bool>,
    mode: Cell<Mode>,
}

impl Tty for Fake {
    fn shell_in_front(&self) -> bool {
        self.front.get()
    }
    fn mode(&self) -> Mode {
        self.mode.get()
    }
}

/// History, oldest first; the last match is the most recent.
struct Words(Vec<&'static str>);

impl Suggest for Words {
    fn suggest(&self, line: &str) -> Option<Cow<'_, str>> {
        self.0
            .iter()
            .rev()
            .find(|c| c.starts_with(line) && c.len() > line.len())
            .map(|c| Cow::Borrowed(*c))
    }
}

fn mark(body: &str) -> Vec<u8> {
    format!("\x1b]7777;{NONCE};{body}\x07").into_bytes()
}

/// What bash writes when the probe runs: clear the line, the report, then the
/// prompt and the line again.
fn reply(line: &str, point: usize) -> Vec<u8> {
    let mut bytes = b"\r\x1b[K\r".to_vec();
    bytes.extend(mark(&format!(
        "L;{point};{};{}",
        line.chars().count(),
        quoted(line)
    )));
    bytes.extend_from_slice(format!("P> {line}").as_bytes());
    bytes
}

fn quoted(line: &str) -> String {
    if line.is_empty() {
        return "''".to_owned();
    }
    line.replace(' ', "\\ ")
}

/// The bytes that draw `rest` as a suggestion.
fn drawn(rest: &str) -> Vec<u8> {
    format!("\x1b7\x1b[?7l\x1b[2m{rest}\x1b[22m\x1b[?7h\x1b8").into_bytes()
}

/// The bytes that write `text` typed ahead of the shell.
fn typed(text: &str) -> Vec<u8> {
    format!("\x1b[?7l{text}\x1b[?7h").into_bytes()
}

/// `keys` followed by the probe.
fn probed(keys: &[u8]) -> Vec<u8> {
    let mut bytes = keys.to_vec();
    bytes.extend_from_slice(PROBE_KEYS);
    bytes
}

struct Sim {
    engine: Engine<Words>,
    tty: Fake,
    now: Instant,
    out: Out,
    terminal: Vec<u8>,
    shell: Vec<u8>,
    events: Vec<Event>,
}

impl Sim {
    fn new(history: &[&'static str]) -> Self {
        Self {
            engine: Engine::new(NONCE, 80, Style::dim(), Words(history.to_vec())),
            tty: Fake {
                front: Cell::new(true),
                mode: Cell::new(Mode::Readline),
            },
            now: Instant::now(),
            out: Out::default(),
            terminal: Vec::new(),
            shell: Vec::new(),
            events: Vec::new(),
        }
    }

    fn collect(&mut self) {
        self.terminal.append(&mut self.out.to_terminal);
        self.shell.append(&mut self.out.to_shell);
        self.events.append(&mut self.out.events);
    }

    fn input(&mut self, bytes: &[u8]) {
        self.engine
            .on_input(self.now, bytes, &self.tty, &mut self.out);
        self.collect();
    }

    fn output(&mut self, bytes: &[u8]) {
        self.engine
            .on_output(self.now, bytes, &self.tty, &mut self.out);
        self.collect();
    }

    fn wait(&mut self, millis: u64) {
        self.now = output::after(self.now, Duration::from_millis(millis));
        self.engine.on_tick(self.now, &self.tty, &mut self.out);
        self.collect();
    }

    fn resize(&mut self, columns: usize) {
        self.engine.resize(columns, &mut self.out);
        self.collect();
    }

    fn take_terminal(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.terminal)
    }

    fn take_shell(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.shell)
    }

    /// The shell draws a prompt.
    fn prompt(&mut self) {
        let mut bytes = mark("P;");
        bytes.extend_from_slice(b"\r\x1b[K\rP> ");
        self.output(&bytes);
        self.take_terminal();
    }

    /// The shell draws a prompt after keys were typed ahead for it, so its
    /// line is not known until a probe says what it is.
    fn prompt_unknown(&mut self) {
        self.engine.typeahead = true;
        self.prompt();
    }

    /// The person types `typed`; the shell answers with the probe's reply for
    /// `line`; the redraw settles. Returns what the terminal was sent.
    fn type_and_settle(&mut self, typed: &[u8], line: &str) -> Vec<u8> {
        self.input(typed);
        assert_eq!(
            self.take_shell(),
            probed(typed),
            "the keys go on with a probe"
        );
        self.output(&reply(line, line.chars().count()));
        self.wait(1);
        self.take_terminal()
    }
}

#[test]
fn outside_a_prompt_everything_passes_untouched() {
    let mut sim = Sim::new(&["git status"]);
    sim.input(b"hello\x1b[A\r");
    assert_eq!(sim.take_shell(), b"hello\x1b[A\r");
    sim.output(b"output \x1b[1mbold\x1b[0m\r\n");
    assert_eq!(sim.take_terminal(), b"output \x1b[1mbold\x1b[0m\r\n");
    assert!(!sim.engine.holds_input());
}

#[test]
fn a_key_at_a_prompt_goes_on_with_a_probe_and_input_is_held() {
    let mut sim = Sim::new(&["git status"]);
    sim.prompt();
    sim.input(b"g");
    assert_eq!(sim.take_shell(), probed(b"g"));
    assert!(sim.engine.holds_input());
    assert!(sim.engine.deadline().is_some());
}

#[test]
fn the_suggestion_is_drawn_after_the_redraw_in_one_go() {
    let mut sim = Sim::new(&["git status"]);
    sim.prompt_unknown();
    let sent = sim.type_and_settle(b"g", "g");
    let mut expect = b"\r\x1b[K\rP> g".to_vec();
    expect.extend(drawn("it status"));
    assert_eq!(sent, expect);
    assert!(!sim.engine.holds_input());
}

#[test]
fn the_suggestion_comes_as_soon_as_the_redraw_ends_with_the_line() {
    let mut sim = Sim::new(&["git status"]);
    sim.prompt_unknown();
    sim.input(b"g");
    sim.output(b"g");
    sim.output(&reply("g", 1));
    let mut expect = b"g\r\x1b[K\rP> g".to_vec();
    expect.extend(drawn("it status"));
    assert_eq!(sim.take_terminal(), expect, "no waiting");
    assert!(!sim.engine.holds_input());
}

#[test]
fn when_the_end_of_the_redraw_cannot_be_told_it_is_waited_for() {
    let mut sim = Sim::new(&["git status"]);
    sim.prompt();
    sim.input(b"\x7f");
    sim.output(b"\x08");
    sim.output(&reply("", 0));
    assert_eq!(sim.take_terminal(), b"", "held while more may come");
    sim.output(b"");
    sim.wait(1);
    assert_eq!(sim.take_terminal(), b"\x08\r\x1b[K\rP> ");
}

#[test]
fn more_output_after_the_report_extends_the_wait() {
    let mut sim = Sim::new(&["git status"]);
    sim.prompt();
    sim.input(b"\x7f");
    sim.output(&reply("", 0));
    sim.wait(0);
    assert_eq!(sim.take_terminal(), b"");
    sim.output(b"x");
    sim.wait(0);
    assert_eq!(sim.take_terminal(), b"");
    sim.wait(1);
    assert_ne!(sim.take_terminal().len(), 0);
}

#[test]
fn a_probe_that_is_not_answered_is_given_up_on_and_the_prompt_is_quarantined() {
    let mut sim = Sim::new(&["git status"]);
    sim.prompt_unknown();
    sim.input(b"g");
    sim.output(b"g");
    sim.wait(100);
    assert_eq!(sim.take_terminal(), b"g");
    assert!(!sim.engine.holds_input());
    sim.take_shell();
    sim.input(b"i");
    assert_eq!(sim.take_shell(), b"i", "no more probes at this prompt");
}

#[test]
fn the_suggestion_is_erased_before_the_next_output() {
    let mut sim = Sim::new(&["git status"]);
    sim.prompt();
    sim.type_and_settle(b"g", "g");
    sim.output(b"x");
    assert_eq!(sim.take_terminal(), b"\x1b[Kx");
    sim.output(b"y");
    assert_eq!(sim.take_terminal(), b"y");
}

#[test]
fn typing_on_replaces_the_suggestion_without_a_gap() {
    let mut sim = Sim::new(&["git status"]);
    sim.prompt();
    sim.type_and_settle(b"g", "g");
    sim.input(b"i");
    let mut ahead = b"\x1b[K".to_vec();
    ahead.extend(typed("i"));
    ahead.extend(drawn("t status"));
    assert_eq!(
        sim.take_terminal(),
        ahead,
        "the key and its suggestion at once"
    );
    sim.take_shell();
    sim.output(&reply("gi", 2));
    sim.wait(1);
    let mut expect = b"\x1b[1D\x1b[K\r\x1b[K\rP> gi".to_vec();
    expect.extend(drawn("t status"));
    assert_eq!(
        sim.take_terminal(),
        expect,
        "back to the shell's cursor, its redraw and the suggestion are one write"
    );
}

#[test]
fn no_suggestion_for_an_empty_line_or_when_nothing_matches() {
    let mut sim = Sim::new(&["git status"]);
    sim.prompt();
    let sent = sim.type_and_settle(b"x", "x");
    let mut expect = typed("x");
    expect.extend_from_slice(b"\x1b[1D\x1b[K\r\x1b[K\rP> x");
    assert_eq!(sent, expect);
    let sent = sim.type_and_settle(b"\x7f", "");
    assert_eq!(sent, b"\x1b[1D\x1b[K\x1b[1C\x1b[K\r\x1b[K\rP> ");
}

/// The shell's reply with the cursor one place before the end of `line`.
fn reply_with_cursor_back(line: &str) -> Vec<u8> {
    let mut bytes = b"\r\x1b[K\r".to_vec();
    bytes.extend(mark(&format!(
        "L;{};{};{line}",
        line.chars().count().saturating_sub(1),
        line.chars().count()
    )));
    bytes.extend_from_slice(format!("P> {line}\x08").as_bytes());
    bytes
}

#[test]
fn no_suggestion_when_the_cursor_is_not_at_the_end() {
    let mut sim = Sim::new(&["git status"]);
    sim.prompt();
    sim.input(b"gi\x1b[D");
    sim.take_shell();
    sim.output(&reply_with_cursor_back("gi"));
    sim.wait(1);
    assert_eq!(sim.take_terminal(), b"\r\x1b[K\rP> gi\x08");
}

#[test]
fn right_at_the_end_types_the_whole_suggestion_instead_of_itself() {
    let mut sim = Sim::new(&["git status"]);
    sim.prompt();
    sim.type_and_settle(b"g", "g");
    sim.input(b"\x1b[C");
    assert_eq!(sim.take_shell(), probed(b"it status"));
}

#[test]
fn end_and_ctrl_e_and_the_application_forms_take_it_too() {
    for key in [
        &b"\x1b[F"[..],
        b"\x05",
        b"\x1bOC",
        b"\x1bOF",
        b"\x1b[4~",
        b"\x1b[8~",
    ] {
        let mut sim = Sim::new(&["git status"]);
        sim.prompt();
        sim.type_and_settle(b"g", "g");
        sim.input(key);
        assert_eq!(sim.take_shell(), probed(b"it status"), "{key:?}");
    }
}

#[test]
fn alt_f_and_ctrl_right_take_one_word() {
    for key in [&b"\x1bf"[..], b"\x1b[1;5C"] {
        let mut sim = Sim::new(&["git commit -m fix"]);
        sim.prompt();
        sim.type_and_settle(b"g", "g");
        sim.input(key);
        assert_eq!(sim.take_shell(), probed(b"it"), "{key:?}");
    }
}

#[test]
fn alt_right_takes_a_word_and_its_blanks() {
    let mut sim = Sim::new(&["git commit -m fix"]);
    sim.prompt();
    sim.type_and_settle(b"git ", "git ");
    sim.input(b"\x1b[1;3C");
    assert_eq!(sim.take_shell(), probed(b"commit "));
}

#[test]
fn taking_a_word_again_takes_the_next_one() {
    let mut sim = Sim::new(&["git commit -m fix"]);
    sim.prompt();
    sim.type_and_settle(b"g", "g");
    sim.input(b"\x1bf");
    sim.take_shell();
    sim.output(&reply("git", 3));
    sim.wait(1);
    sim.take_terminal();
    sim.input(b"\x1bf");
    assert_eq!(sim.take_shell(), probed(b" commit"));
}

#[test]
fn a_taking_key_with_nothing_to_take_is_an_ordinary_key() {
    let mut sim = Sim::new(&["git status"]);
    sim.prompt();
    sim.type_and_settle(b"x", "x");
    sim.input(b"\x1b[C");
    assert_eq!(sim.take_shell(), probed(b"\x1b[C"));
}

#[test]
fn a_taking_key_straight_after_typing_waits_for_the_line_to_be_known() {
    let mut sim = Sim::new(&["git status"]);
    sim.prompt();
    sim.input(b"g\x1b[C");
    assert_eq!(sim.take_shell(), probed(b"g"), "the probe goes first");
    assert!(sim.engine.holds_input());
    sim.output(&reply("g", 1));
    sim.wait(1);
    assert_eq!(
        sim.take_shell(),
        probed(b"it status"),
        "then the key is read"
    );
}

#[test]
fn keys_before_a_taking_key_in_a_quarantined_prompt_all_go_on() {
    let mut sim = Sim::new(&["git status"]);
    sim.prompt();
    sim.input(b"\x12");
    sim.take_shell();
    // No probe will come, so nothing may wait for one.
    sim.input(b"ab\x1b[C");
    assert_eq!(sim.take_shell(), b"ab\x1b[C");
    assert!(!sim.engine.holds_input());
    sim.input(b"\x02\x05x");
    assert_eq!(sim.take_shell(), b"\x02\x05x");
}

#[test]
fn a_taking_key_that_has_no_suggestion_after_all_goes_on() {
    let mut sim = Sim::new(&["git status"]);
    sim.prompt();
    sim.input(b"x\x1b[C");
    assert_eq!(sim.take_shell(), probed(b"x"));
    sim.output(&reply("x", 1));
    sim.wait(1);
    assert_eq!(sim.take_shell(), probed(b"\x1b[C"));
}

#[test]
fn keys_that_start_a_run_get_no_probe() {
    for key in [
        &b"\t"[..],
        b"\x0b",
        b"\x15",
        b"\x17",
        b"\x19",
        b"\x1bd",
        b"\x1b\x7f",
    ] {
        let mut sim = Sim::new(&["git status"]);
        sim.prompt();
        sim.input(key);
        assert_eq!(sim.take_shell(), key, "{key:?}");
        assert!(!sim.engine.holds_input());
    }
}

#[test]
fn a_plain_key_after_one_that_starts_a_run_is_probed() {
    let mut sim = Sim::new(&["git status"]);
    sim.prompt();
    sim.input(b"\t");
    sim.take_shell();
    sim.input(b"g");
    assert_eq!(sim.take_shell(), probed(b"g"));
}

#[test]
fn a_modal_key_quarantines_the_prompt_until_a_new_one() {
    let mut sim = Sim::new(&["git status"]);
    sim.prompt();
    sim.input(b"\x12");
    assert_eq!(sim.take_shell(), b"\x12");
    sim.input(b"g");
    assert_eq!(sim.take_shell(), b"g", "no probe in a search");
    sim.prompt();
    sim.input(b"g");
    assert_eq!(
        sim.take_shell(),
        probed(b"g"),
        "a new prompt is a fresh start"
    );
}

#[test]
fn nothing_is_probed_after_a_signal_key_until_the_next_prompt() {
    // The shell handles the interrupt after reading the keys that came with
    // it, and a command that runs then is thrown out with the line.
    let mut sim = Sim::new(&["git status"]);
    sim.prompt();
    sim.input(b"ab\x03cd\x01");
    assert_eq!(sim.take_shell(), b"ab\x03cd\x01");
    assert!(!sim.engine.holds_input());
    sim.input(b"e");
    assert_eq!(sim.take_shell(), b"e");
    sim.prompt();
    sim.input(b"e");
    assert_eq!(sim.take_shell(), probed(b"e"));
}

#[test]
fn ctrl_g_does_not_undo_a_signal_key() {
    let mut sim = Sim::new(&["git status"]);
    sim.prompt();
    sim.input(b"\x03\x07tx");
    assert_eq!(sim.take_shell(), b"\x03\x07tx");
    sim.tty.front.set(false);
    sim.output(b"x");
    sim.tty.front.set(true);
    sim.output(b"y");
    sim.input(b"z");
    assert_eq!(
        sim.take_shell(),
        b"z",
        "neither Ctrl-G nor a program coming and going helps"
    );
}

#[test]
fn ctrl_g_ends_a_quarantine() {
    let mut sim = Sim::new(&["git status"]);
    sim.prompt();
    sim.input(b"\x12g");
    sim.take_shell();
    sim.input(b"\x07");
    sim.take_shell();
    sim.input(b"g");
    assert_eq!(sim.take_shell(), probed(b"g"));
}

#[test]
fn a_program_that_takes_the_terminal_is_waited_out() {
    let mut sim = Sim::new(&["git status"]);
    sim.prompt();
    // Atuin's search: the up arrow is a modal key.
    sim.input(b"\x1b[A");
    sim.take_shell();
    sim.tty.front.set(false);
    sim.output(b"search screen");
    sim.input(b"abc");
    assert_eq!(sim.take_shell(), b"abc", "typed into the program, no probe");
    // It is done: the shell has the terminal and readline's modes again.
    sim.tty.front.set(true);
    sim.output(b"\r\x1b[KP> found");
    sim.input(b"g");
    assert_eq!(
        sim.take_shell(),
        probed(b"g"),
        "the shell is back: probing resumes"
    );
}

#[test]
fn a_program_in_raw_mode_under_the_shells_own_group_is_waited_out_too() {
    // Atuin's search runs in a command substitution, which stays in the
    // shell's process group: only its terminal settings give it away.
    let mut sim = Sim::new(&["git status"]);
    sim.prompt();
    sim.input(b"\x1b[A");
    sim.take_shell();
    sim.tty.mode.set(Mode::Other);
    sim.output(b"search screen");
    sim.input(b"abc");
    assert_eq!(sim.take_shell(), b"abc");
    sim.tty.mode.set(Mode::Readline);
    sim.output(b"\r\x1b[KP> found");
    sim.input(b"g");
    assert_eq!(sim.take_shell(), probed(b"g"));
}

#[test]
fn a_foreground_that_is_not_the_shell_stops_probes_even_without_a_modal_key() {
    let mut sim = Sim::new(&["git status"]);
    sim.prompt();
    sim.tty.front.set(false);
    sim.input(b"g");
    assert_eq!(sim.take_shell(), b"g");
}

#[test]
fn a_terminal_not_yet_in_readline_mode_gets_no_probe_and_no_quarantine() {
    let mut sim = Sim::new(&["git status"]);
    sim.prompt();
    sim.tty.mode.set(Mode::Cooked);
    sim.input(b"g");
    assert_eq!(sim.take_shell(), b"g");
    sim.tty.mode.set(Mode::Readline);
    sim.input(b"i");
    assert_eq!(sim.take_shell(), probed(b"i"));
}

#[test]
fn the_key_that_runs_the_line_is_never_followed_by_a_probe() {
    for keys in [&b"ls\r"[..], b"ls\n", b"\r", b"ls\x0f"] {
        let mut sim = Sim::new(&["git status"]);
        sim.prompt();
        sim.input(keys);
        assert_eq!(sim.take_shell(), keys, "{keys:?}");
        assert!(!sim.engine.holds_input());
        // And what comes after, before the shell says anything, passes.
        sim.input(b"more");
        assert_eq!(sim.take_shell(), b"more");
    }
}

#[test]
fn keys_after_the_one_that_runs_the_line_in_the_same_write_go_on_untouched() {
    let mut sim = Sim::new(&["git status"]);
    sim.prompt();
    sim.input(b"a\rb\x1b[C");
    assert_eq!(sim.take_shell(), b"a\rb\x1b[C");
}

#[test]
fn the_line_that_ran_is_reported_when_the_command_starts_if_it_was_known() {
    let mut sim = Sim::new(&["git status"]);
    sim.prompt();
    sim.type_and_settle(b"ls", "ls");
    sim.input(b"\r");
    sim.output(&mark("R"));
    assert_eq!(
        sim.events,
        vec![
            Event::PromptStarted,
            Event::CommandStarted {
                line: Some("ls".to_owned())
            }
        ]
    );
}

#[test]
fn a_line_that_changed_after_the_report_is_not_reported() {
    let mut sim = Sim::new(&["git status"]);
    sim.prompt();
    sim.type_and_settle(b"l", "l");
    sim.input(b"s\r");
    sim.output(&mark("R"));
    assert_eq!(
        sim.events.last(),
        Some(&Event::CommandStarted { line: None })
    );
}

#[test]
fn a_bracketed_paste_is_not_probed_inside_and_is_after() {
    let mut sim = Sim::new(&["git status"]);
    sim.prompt();
    sim.input(b"\x1b[200~ab");
    assert_eq!(sim.take_shell(), b"\x1b[200~ab", "no probe inside");
    sim.input(b"cd\r");
    assert_eq!(
        sim.take_shell(),
        b"cd\r",
        "a newline inside a paste is not the end of the line"
    );
    sim.input(b"\x1b[201~");
    assert_eq!(
        sim.take_shell(),
        probed(b"\x1b[201~"),
        "probe after the paste"
    );
}

#[test]
fn a_paste_end_marker_split_across_reads_is_found() {
    let mut sim = Sim::new(&["git status"]);
    sim.prompt();
    sim.input(b"\x1b[200~xy\x1b[20");
    sim.take_shell();
    sim.input(b"1~");
    assert_eq!(sim.take_shell(), probed(b"1~"));
}

#[test]
fn a_whole_paste_in_one_read_is_probed_once() {
    let mut sim = Sim::new(&["git status"]);
    sim.prompt();
    let mut paste = b"\x1b[200~".to_vec();
    paste.extend(std::iter::repeat_n(b'a', 2000));
    paste.extend_from_slice(b"\x1b[201~");
    sim.input(&paste);
    assert_eq!(sim.take_shell(), probed(&paste));
}

#[test]
fn a_long_unbracketed_burst_is_probed_once_at_the_end() {
    let mut sim = Sim::new(&["git status"]);
    sim.prompt();
    let burst = vec![b'a'; 2000];
    sim.input(&burst);
    assert_eq!(sim.take_shell(), probed(&burst));
}

#[test]
fn a_key_cut_in_two_waits_for_its_other_half() {
    let mut sim = Sim::new(&["git status"]);
    sim.prompt();
    sim.input(b"\x1b");
    assert_eq!(sim.take_shell(), b"", "kept back");
    sim.input(b"[D");
    assert_eq!(sim.take_shell(), probed(b"\x1b[D"));
}

#[test]
fn a_character_cut_in_two_is_not_split_by_a_probe() {
    let mut sim = Sim::new(&["git status"]);
    sim.prompt();
    sim.input(&[b'a', 0xe6, 0x97]);
    assert_eq!(sim.take_shell(), probed(b"a"));
    sim.output(&reply("a", 1));
    sim.wait(1);
    sim.input(&[0xa5]);
    assert_eq!(sim.take_shell(), probed(&[0xe6, 0x97, 0xa5]));
}

#[test]
fn a_lone_escape_is_let_through_after_a_moment_and_quarantines() {
    let mut sim = Sim::new(&["git status"]);
    sim.prompt();
    sim.input(b"\x1b");
    sim.wait(5);
    assert_eq!(sim.take_shell(), b"");
    sim.wait(20);
    assert_eq!(sim.take_shell(), b"\x1b");
    sim.input(b"g");
    assert_eq!(sim.take_shell(), b"g", "an escape starts something");
}

#[test]
fn a_taking_key_does_not_take_when_the_line_is_not_at_the_end() {
    let mut sim = Sim::new(&["git status"]);
    sim.prompt();
    sim.input(b"gi\x1b[D");
    sim.take_shell();
    sim.output(&reply_with_cursor_back("gi"));
    sim.wait(1);
    sim.take_terminal();
    sim.input(b"\x1b[C");
    assert_eq!(sim.take_shell(), probed(b"\x1b[C"));
}

#[test]
fn a_narrow_row_cuts_the_suggestion_with_an_ellipsis() {
    let mut sim = Sim::new(&["echo 0123456789"]);
    sim.resize(12);
    sim.prompt_unknown();
    // "P> echo" is 7 columns; room is 12 - 1 - 7 = 4.
    let sent = sim.type_and_settle(b"echo", "echo");
    let mut expect = b"\r\x1b[K\rP> echo".to_vec();
    expect.extend(drawn(" 01\u{2026}"));
    assert_eq!(sent, expect);
}

#[test]
fn the_whole_suggestion_is_taken_even_when_it_was_cut() {
    let mut sim = Sim::new(&["echo 0123456789"]);
    sim.resize(12);
    sim.prompt();
    sim.type_and_settle(b"echo", "echo");
    sim.input(b"\x1b[C");
    assert_eq!(sim.take_shell(), probed(b" 0123456789"));
}

#[test]
fn no_suggestion_when_the_cursors_column_is_not_known() {
    let mut sim = Sim::new(&["git status"]);
    sim.prompt();
    sim.resize(80);
    sim.input(b"g");
    sim.take_shell();
    // The redraw does something this does not follow, and there is no
    // carriage return after it.
    let mut bytes = mark("L;1;1;g");
    bytes.extend_from_slice(b"\x1b[3bg");
    sim.output(&bytes);
    sim.wait(1);
    assert_eq!(sim.take_terminal(), b"\x1b[3bg");
}

#[test]
fn a_resize_erases_the_suggestion() {
    let mut sim = Sim::new(&["git status"]);
    sim.prompt();
    sim.type_and_settle(b"g", "g");
    sim.resize(100);
    assert_eq!(sim.take_terminal(), b"\x1b[K");
}

#[test]
fn vi_mode_is_left_alone() {
    let mut sim = Sim::new(&["git status"]);
    let mut bytes = mark("P;v");
    bytes.extend_from_slice(b"P> ");
    sim.output(&bytes);
    sim.take_terminal();
    sim.input(b"g\x1b[C\r");
    assert_eq!(sim.take_shell(), b"g\x1b[C\r");
}

#[test]
fn a_report_that_is_not_one_is_a_quarantine_and_the_stream_goes_on() {
    let mut sim = Sim::new(&["git status"]);
    sim.prompt_unknown();
    sim.input(b"g");
    sim.take_shell();
    let mut bytes = b"g".to_vec();
    bytes.extend(mark("L;nonsense"));
    sim.output(&bytes);
    sim.wait(1);
    assert_eq!(sim.take_terminal(), b"g");
    sim.input(b"i");
    assert_eq!(sim.take_shell(), b"i");
}

#[test]
fn a_line_that_is_not_text_gets_no_suggestion_but_probes_go_on() {
    let mut sim = Sim::new(&["git status"]);
    sim.prompt_unknown();
    sim.input(b"g");
    sim.take_shell();
    let mut bytes = b"\r\x1b[K\r".to_vec();
    bytes.extend(mark("L;1;1;$'\\377'"));
    bytes.extend_from_slice(b"P> ?");
    sim.output(&bytes);
    sim.wait(1);
    assert_eq!(sim.take_terminal(), b"\r\x1b[K\rP> ?");
    sim.input(b"x");
    assert_eq!(sim.take_shell(), probed(b"x"));
}

#[test]
fn marks_split_across_reads_are_still_marks() {
    let mut sim = Sim::new(&["git status"]);
    let whole = mark("P;");
    let (head, tail) = whole.split_at(9);
    sim.output(head);
    assert_eq!(sim.take_terminal(), b"");
    sim.output(tail);
    sim.input(b"g");
    assert_eq!(sim.take_shell(), probed(b"g"));
}

#[test]
fn the_start_of_a_mark_that_never_finishes_is_given_back() {
    let mut sim = Sim::new(&["git status"]);
    sim.output(b"ok\x1b]77");
    assert_eq!(sim.take_terminal(), b"ok");
    sim.wait(10);
    assert_eq!(sim.take_terminal(), b"\x1b]77");
}

#[test]
fn a_probe_that_is_answered_with_a_flood_is_given_up_on() {
    let mut sim = Sim::new(&["git status"]);
    sim.prompt_unknown();
    sim.input(b"g");
    sim.take_shell();
    let flood = vec![b'x'; HELD_LIMIT];
    sim.output(&flood);
    sim.output(b"more");
    assert_eq!(sim.take_terminal().len(), HELD_LIMIT.saturating_add(4));
    assert!(!sim.engine.holds_input());
    sim.input(b"i");
    assert_eq!(sim.take_shell(), b"i", "no more probes at this prompt");
}

#[test]
fn keys_held_back_when_a_new_prompt_begins_are_not_lost() {
    // Ctrl-C ends the line and the shell draws a new prompt; the start of a
    // key that was waiting for its end is still the shell's.
    let mut sim = Sim::new(&["git status"]);
    sim.prompt();
    sim.input(b"\x03\x1b");
    assert_eq!(sim.take_shell(), b"\x03");
    sim.prompt();
    assert_eq!(sim.take_shell(), b"\x1b");
    sim.input(b"[D");
    assert_eq!(sim.take_shell(), probed(b"[D"));
}

#[test]
fn the_end_of_the_shell_lets_go_of_what_is_held() {
    let mut sim = Sim::new(&["git status"]);
    sim.prompt_unknown();
    sim.input(b"g");
    sim.output(b"g");
    sim.engine.finish(&mut sim.out);
    sim.collect();
    assert_eq!(sim.take_terminal(), b"g");
}

#[test]
fn keys_held_back_when_the_command_starts_are_not_lost() {
    let mut sim = Sim::new(&["git status"]);
    sim.prompt();
    sim.input(b"g");
    sim.take_shell();
    sim.output(&reply("g", 1));
    sim.wait(1);
    sim.take_terminal();
    sim.input(b"\x1b");
    sim.output(&mark("R"));
    assert_eq!(sim.take_shell(), b"\x1b");
}

#[test]
fn at_a_fresh_prompt_a_key_and_its_suggestion_are_drawn_before_the_shell_answers() {
    let mut sim = Sim::new(&["git status"]);
    sim.prompt();
    sim.input(b"g");
    let mut ahead = typed("g");
    ahead.extend(drawn("it status"));
    assert_eq!(sim.take_terminal(), ahead);
    assert_eq!(
        sim.take_shell(),
        probed(b"g"),
        "the probe goes all the same"
    );
    // The shell's echo and redraw land where it has the cursor.
    sim.output(b"g");
    sim.output(&reply("g", 1));
    let mut expect = b"\x1b[1D\x1b[Kg\r\x1b[K\rP> g".to_vec();
    expect.extend(drawn("it status"));
    assert_eq!(sim.take_terminal(), expect);
}

#[test]
fn a_wrong_guess_is_put_right_by_the_shells_redraw() {
    // A key the shell does something else with: the redraw shows what it
    // did, and the suggestion is for the line as reported.
    let mut sim = Sim::new(&["git status", "ls -la"]);
    sim.prompt();
    sim.input(b"g");
    sim.take_terminal();
    sim.take_shell();
    sim.output(&reply("l", 1));
    sim.wait(1);
    let mut expect = b"\x1b[1D\x1b[K\r\x1b[K\rP> l".to_vec();
    expect.extend(drawn("s -la"));
    assert_eq!(sim.take_terminal(), expect);
}

#[test]
fn backspace_at_the_end_is_drawn_ahead_with_the_shorter_lines_suggestion() {
    let mut sim = Sim::new(&["git status", "gi"]);
    sim.prompt();
    sim.type_and_settle(b"gi", "gi");
    sim.input(b"\x7f");
    let mut ahead = b"\x1b[1D\x1b[K".to_vec();
    ahead.extend(drawn("i"));
    assert_eq!(sim.take_terminal(), ahead);
    sim.take_shell();
    sim.output(b"\x08\x1b[K");
    sim.output(&reply("g", 1));
    sim.wait(1);
    let mut expect = b"\x1b[1C\x1b[K\x08\x1b[K\r\x1b[K\rP> g".to_vec();
    expect.extend(drawn("i"));
    assert_eq!(sim.take_terminal(), expect);
}

#[test]
fn taking_the_suggestion_is_drawn_ahead_as_typed_text() {
    let mut sim = Sim::new(&["git status"]);
    sim.prompt();
    sim.type_and_settle(b"git", "git");
    sim.input(b"\x1b[C");
    assert_eq!(sim.take_shell(), probed(b" status"));
    let mut ahead = b"\x1b[K".to_vec();
    ahead.extend(typed(" status"));
    assert_eq!(sim.take_terminal(), ahead);
    sim.output(&reply("git status", 10));
    sim.wait(1);
    assert_eq!(sim.take_terminal(), b"\x1b[7D\x1b[K\r\x1b[K\rP> git status");
}

#[test]
fn a_given_up_probe_clears_what_was_drawn_ahead() {
    let mut sim = Sim::new(&["git status"]);
    sim.prompt();
    sim.input(b"g");
    sim.take_terminal();
    sim.output(b"g");
    sim.wait(100);
    assert_eq!(sim.take_terminal(), b"\x1b[1D\x1b[Kg");
}

#[test]
fn nothing_is_drawn_ahead_where_it_cannot_be_told_for_certain() {
    // Typed ahead for the prompt: the line is not known.
    let mut sim = Sim::new(&["git status"]);
    sim.prompt_unknown();
    sim.input(b"g");
    assert_eq!(sim.take_terminal(), b"");
    // A key that is not plain text.
    let mut sim = Sim::new(&["echo \u{e9}t\u{e9}"]);
    sim.prompt();
    sim.input("e\u{e9}".as_bytes());
    assert_eq!(sim.take_terminal(), b"", "only ASCII is drawn ahead");
    // A key after which no probe goes: Tab.
    let mut sim = Sim::new(&["git status"]);
    sim.prompt();
    sim.input(b"g\t");
    assert_eq!(sim.take_terminal(), b"");
    // The cursor inside the line.
    let mut sim = Sim::new(&["git status"]);
    sim.prompt();
    sim.input(b"gi\x1b[D");
    sim.take_shell();
    sim.output(&reply_with_cursor_back("gi"));
    sim.wait(1);
    sim.take_terminal();
    sim.input(b"x");
    assert_eq!(sim.take_terminal(), b"", "the cursor is inside the line");
}

#[test]
fn nothing_is_drawn_ahead_that_would_reach_the_end_of_the_row() {
    let mut sim = Sim::new(&["echo 0123456789"]);
    sim.resize(12);
    sim.prompt();
    // "P> " is 3 columns: room is 12 - 1 - 3 = 8, so 8 keys do not fit.
    sim.input(b"echo 012");
    assert_eq!(sim.take_terminal(), b"");
    let mut sim = Sim::new(&["echo 0123456789"]);
    sim.resize(12);
    sim.prompt();
    sim.input(b"echo 01");
    let mut ahead = typed("echo 01");
    ahead.extend(drawn("\u{2026}"));
    assert_eq!(
        sim.take_terminal(),
        ahead,
        "the one column left takes the ellipsis"
    );
}

#[test]
fn a_prompt_after_keys_typed_ahead_or_ctrl_o_is_not_known_empty() {
    let mut sim = Sim::new(&["git status"]);
    sim.prompt();
    sim.type_and_settle(b"ls", "ls");
    sim.input(b"\r");
    sim.output(&mark("R"));
    sim.input(b"g");
    sim.prompt();
    sim.take_shell();
    sim.input(b"i");
    assert_eq!(sim.take_terminal(), b"", "typed while the command ran");
    let mut sim = Sim::new(&["git status"]);
    sim.prompt();
    sim.type_and_settle(b"ls", "ls");
    sim.input(b"\x0f");
    sim.output(&mark("R"));
    sim.prompt();
    sim.take_shell();
    sim.input(b"g");
    assert_eq!(
        sim.take_terminal(),
        b"",
        "Ctrl-O puts a line in the next prompt"
    );
    let mut sim = Sim::new(&["git status"]);
    sim.prompt();
    sim.type_and_settle(b"ls", "ls");
    sim.input(b"\r");
    sim.output(&mark("R"));
    sim.prompt();
    sim.take_shell();
    sim.input(b"g");
    assert_ne!(
        sim.take_terminal(),
        b"",
        "nothing typed ahead: the line starts empty"
    );
}

#[test]
fn the_column_is_known_at_a_prompt_that_follows_a_command() {
    // The command's output is followed from its last carriage return, so
    // the first key at the next prompt is drawn ahead.
    let mut sim = Sim::new(&["git status"]);
    sim.prompt();
    sim.type_and_settle(b"ls", "ls");
    sim.input(b"\r");
    sim.output(&mark("R"));
    sim.output(b"\r\nfile-one  file-two\r\n");
    let mut bytes = mark("P;");
    bytes.extend_from_slice(b"P> ");
    sim.output(&bytes);
    sim.take_terminal();
    sim.take_shell();
    sim.input(b"g");
    let mut ahead = typed("g");
    ahead.extend(drawn("it status"));
    assert_eq!(sim.take_terminal(), ahead);
}

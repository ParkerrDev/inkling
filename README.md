# inkling

Fish-like inline autosuggestions for bash, written in Rust, and nothing else
changed. As you type at a bash prompt, the rest of the command you most likely
mean appears dim after the cursor. Right arrow takes it. Everything else you
type just types.

```
$ git sta                       you type "git sta"
$ git sta|tus -sb               the dim rest of your last "git status -sb"
```

It exists because the usual way to get this in bash, ble.sh, replaces readline
with a line editor written in bash, which is why it is slow. inkling leaves
readline alone. It runs your shell inside a pseudo-terminal it owns, asks
readline itself what is in the line, and draws the suggestion on the terminal
next to what the shell draws.

## Install

```sh
./install.sh
```

That builds it, puts the program in `~/.local/bin/inkling`, and adds one marked
block at the top of `~/.bashrc`:

```sh
# >>> inkling >>>
command -v inkling >/dev/null 2>&1 && eval "$(inkling init bash)"
# <<< inkling <<<
```

Open a new terminal. `inkling doctor` says what it found and tries the shell
side on a bash of its own.

To remove it, `./uninstall.sh`: it takes the block out of `~/.bashrc`, leaving
the rest as it was, and removes the program. It keeps no data, and never
changes the history it reads.

`INKLING=0 bash` starts a shell without it, for one session. Set `INKLING=0`
in the environment to turn it off for everything started from there.
`INKLING_STYLE` is the SGR parameters the suggestion is drawn with (the
default is `2`, dim; `38;5;244` is a grey).

## The keys

| Key | At the end of the line, with a suggestion showing |
| --- | --- |
| Right, End, Ctrl-E | take all of it |
| Alt-F, Ctrl-Right | take one word, as Alt-F moves over one |
| Alt-Right | take one whitespace-delimited word and the blanks around it |
| anything else | types, and the suggestion is withdrawn or follows what you typed |

With nothing showing, or the cursor inside the line, these keys do what they
always did. The missing characters are sent to the shell as if you had typed
them.

A suggestion is never made for an empty line, for a command that is exactly
what you typed, for a command with a control character in it (a newline or a
tab would be a key of its own when typed), or for a command that was not text.
Commands that start with a space (which you asked not to be remembered) and
commands that were not found (exit status 127: typos, which fish does not keep
either) are left out.

## How it works

**A transparent proxy.** `inkling run` starts your shell on a pseudo-terminal
and passes every byte through in both directions: raw mode on your terminal,
the real terminal's settings and size on the shell's, window size changes
forwarded, the shell's exit status returned. The shell is its session leader,
so job control, Ctrl-C and Ctrl-Z behave as they do in a plain terminal. Outside
a prompt it does nothing but copy. `eval "$(inkling init bash)"` makes the
shell start the proxy once, on a real terminal, and start itself again inside
it exactly as it was started: the same words on its command line, and the
environment it began with, so that a terminal's own shell integration (kitty's,
ghostty's) runs again for the inner shell.

**Marks.** The shell writes two private operating system commands, which the
proxy takes out of the stream before the terminal sees them: one from
`PROMPT_COMMAND` (a prompt is about to be drawn) and one from `PS0` (a command
is about to run). They carry a random token only that shell was given.

**The probe.** To know the line exactly, inkling does not read the screen. The
integration binds one otherwise unused key sequence (Ctrl-X, Ctrl-underscore,
`I`, BEL) with `bind -x` to a function that writes `READLINE_LINE` and
`READLINE_POINT` in a third private command. After a key goes to the shell,
the proxy sends that sequence in the same write and reads the report back. The
line is quoted by `printf %q`: one builtin call, no fork.

**Drawing.** With the line known and the cursor at its end, the most likely
rest is drawn: save the cursor, switch automatic wrapping off, write the text
dim, restore. It is cut to what fits on the row. Before any further output
from the shell reaches the terminal, one "erase to end of row" goes first.
Readline clears the line and draws it again around the report; the proxy holds
that back and sends it, the erase and the new suggestion as one write, so the
terminal shows no gap. The held output is released at once when it ends with
the line (which it does when the cursor is at the end), and otherwise after a
fifth of a millisecond.

**History.** From Atuin's database (`~/.local/share/atuin/history.db`, opened
read-only, `query_only` on) if it is there, else `$HISTFILE`. The commands are
read into memory when the session starts and what is new is read at each
prompt. Every distinct command is kept once in a list sorted by its text,
beside the time it last ran: the commands that start with what you typed are
one run of that list, found by two binary searches, and the most recent is the
largest time in the run. As fish does, the most recent wins, but a command run
in the current directory comes before all that were not (Atuin knows the
directory; a history file does not).

## When a probe is sent

A probe is a command to readline like any other, so it must never reach
anything but readline in its plain editing state. The rule, which the tests
exercise:

* Only at a prompt: after the prompt mark, and before the key that runs the
  line. The key that runs the line (Enter, Ctrl-J, Ctrl-O, keypad Enter), and
  everything in the same write after it, goes through without a probe.
* Only when the terminal is the shell's: the shell is the foreground process
  group of the pseudo-terminal, and the terminal's modes are readline's (no
  echo, no line editing, signals and output processing on). A program that has
  taken the terminal in raw mode, as Atuin's search or fzf do from a key
  binding, is told by its modes even though it stays in the shell's process
  group.
* Only after a key that leaves readline plain and does not begin or continue a
  run of commands that depend on each other: text, Backspace, Delete, the
  cursor keys, Ctrl-A, B, E, F, Alt-B, Alt-F. Not after Tab (pressed twice it
  cycles), the kills (two in a row make one piece to yank), history keys, undo
  and so on. A probe between two Tabs would stop the cycling; this was
  measured, and the tests check that the keys act as they do without inkling.
* Never after a key that may leave readline somewhere else: Ctrl-R and Ctrl-S
  (incremental search), Ctrl-V and Ctrl-Q (quoted insert), Ctrl-X (a prefix),
  Ctrl-T (taken by fzf), Escape alone or with anything that is not a known
  motion (Alt-C is fzf's), Up and Down (Atuin's), a lone ESC that never became a
  sequence, or any sequence it does not know. Probes resume at the next prompt,
  when the foreign program has given the terminal back, or at Ctrl-G.
* Never inside a bracketed paste, or in the middle of an escape sequence or a
  character. A probe goes after a paste ends, once, whatever its length.
* One at a time. While a probe is out, the proxy reads nothing more from you
  (your keys wait in the kernel), and gives up after 80 ms, after which that
  prompt gets no more probes.

## Measurements

Release build, bash 5.3.15, this laptop (Omarchy, Arch Linux), through
`scripts/measure`. Typing one key at a time into an idle prompt, 300 keys a
round, three rounds:

| | bash alone | with inkling |
| --- | --- | --- |
| time from key to first output, median | 145 to 211 us | 509 to 605 us |
| 95th percentile | 319 to 673 us | 768 to 1137 us |
| worst of 300 | 432 to 1812 us | 933 to 2130 us |

So it adds about **0.3 to 0.5 ms** per keystroke, and the suggestion arrives in
the same write as the echo of the key. Starting a shell with a minimal startup
file takes 8 ms alone and 20 ms with inkling, so about **12 ms** are added
(bash starts twice; with the block at the top of `~/.bashrc` the first one
stops there, and your real startup file runs once). `inkling init bash`
takes 0.8 ms to run and print. Output through the proxy: 19 MiB/s against
27 to 33 MiB/s for `seq 1 2000000` straight to the terminal's pseudo-terminal.

Memory of the proxy, resident: 3.3 MiB with a small history file; 6.5 MiB with
10,000 runs in Atuin; 18 MiB with 100,000 (peak 25 MiB while loading); 43 MiB
with 400,000 (peak 80 MiB). The program is 2.2 MB. A lookup takes about
0.3 microseconds.

One trial against the real Atuin database on this machine (read-only, counts
and timings only): 62,206 runs, 8,484 distinct commands, read in 21 ms.

## What it changes in your shell

One function for the prompt mark, one for the probe, one key binding
(`\C-x\C-_I\a`), an addition to `PS0`, and an entry at the front of
`PROMPT_COMMAND`. The probe leaves `$?`, `$_`, `PIPESTATUS` and your history
as they were (`$_` by being passed to the function as its last argument, which
is how bash restores it), and bash-preexec, and so Atuin, see every command,
the first included; both are tested against the real thing. The shell's parent
process is `inkling` instead of the terminal. The nonce and the shell's
process id are given to the shell in its environment and taken out of its
exports at once, so nothing it starts inherits them: a terminal started from
it gets a proxy of its own.

## What it does not do yet

* A suggestion that would run past the end of the row is cut there, with an
  ellipsis, and not wrapped. Taking it takes all of it, wrapping included.
* Only bash (5.1 or later, for the array `PROMPT_COMMAND`), and only emacs
  editing mode; in vi mode it does nothing.
* Only history is suggested. ble.sh also suggests file and command names.
* The suggestion returns after the next plain key, not straight after a Tab,
  a kill or a history key (see the rule above), and not after a key it does
  not know until the next prompt.
* A history file only holds a session's commands after it ends, so for that
  source a command is learned when the line was known for certain before the
  key that ran it (typed, a pause, Enter). Atuin's records are read at the
  next prompt. A command Atuin has marked deleted is still suggested until the
  next session.
* Multi-line commands in history are skipped.
* Wide characters are counted one code point at a time (a ZWJ emoji sequence
  counts wide), so such a prompt shortens the suggestion more than it needs
  to, never less.
* Linux only (it reads `/proc`), and a terminal that supports saving the
  cursor with its attributes (`ESC 7`, `ESC 8`) and the dim attribute.

## Checks

`scripts/check-rust` runs the formatting, clippy with every warning an error
(the Power of Ten lint tables in `Cargo.toml`), the tests, the documentation,
and `cargo deny`. The tests include the end-to-end ones: bash and the proxy on
a pseudo-terminal, typed to, and read back through a small screen model of
what a person would see (and tmux, when it is installed, as a second opinion on
the drawing). They cover the suggestion, the keys that take it, nothing being
drawn or leaked while `cat`, an editor or `sleep` run, a 2,000 character paste,
resizing, exit statuses, job control, closing the terminal, `INKLING=0`,
Tab cycling and the kill ring unchanged, bash-preexec and Atuin alongside,
and this machine's whole shell setup (Omarchy's rc, starship, zoxide, mise,
fzf, Atuin) in a scratch home. `scripts/check-repo` adds shellcheck and the
install and uninstall tests, against a scratch `HOME` only.

## Credits

The behaviour of ble.sh's auto-complete was read, and the choices here follow
it where they are alike; see `NOTICE`. No code was taken from it.

MIT licence, in `LICENSE`.

# inkling

Fish-like inline autosuggestions for bash, written in Rust, and nothing else
changed. As you type at a bash prompt, the rest of the command you most likely
mean appears dim after the cursor. Right arrow takes it. Everything else you
type just types.

```
$ git sta                       you type "git sta"
$ git sta|tus -sb               the dim rest of the "git status -sb" you most likely mean
```

It exists because the usual way to get this in bash, ble.sh, replaces readline
with a line editor written in bash, which is why it is slow. inkling leaves
readline alone. It runs your shell inside a pseudo-terminal it owns, asks
readline itself what is in the line, and draws the suggestion on the terminal
next to what the shell draws.

It is quick: a key and its suggestion are on the screen as soon as bash's own
echo of the key would be, and the suggestion is never late, however long the
history (with 100,000 commands fish's suggestion falls 20 to 30 ms behind at
the 95th percentile; ble.sh's is 30 to 40 ms behind with any history). And it
knows where you are: the suggestion is the command you most likely mean here,
from what you run in this directory, in this project and after the command you
just ran, among those whose paths are there; with nothing in the history, a
program's or a file's name is finished instead. On its owner's own history,
replayed, the very command is suggested after one character 64% of the time,
against 53% for fish's rule. See Measurements.

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

If inkling cannot set itself up (no pseudo-terminal to be had, say), it says
why in one line and starts your shell without suggestions: a terminal always
comes up with a shell in it.

`inkling replay` replays your history and says how much typing the
suggestions would have saved you, and how often they were the very command,
next to fish's order and inkling's earlier one. It prints numbers only, never
a command. `inkling replay --tune` also searches for the weights (see below)
that would have done best on the older part of your history, and judges them
on the newer part.

`INKLING=0 bash` starts a shell without it, for one session. Set `INKLING=0`
in the environment to turn it off for everything started from there.
`INKLING_STYLE` is the SGR parameters the suggestion is drawn with (the
default is `2`, dim; `38;5;244` is a grey). `INKLING_TRACE=/tmp/inkling.log`,
set before the shell starts, writes every chunk that passes through the proxy,
in both directions, to that file, for finding out why something went as it did.

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

**Drawing ahead.** Waiting for the report would put the probe's round trip
through bash in front of every key. So where the line is known for certain
and the cursor is at its end, and the keys only type plain characters (ASCII,
a column each), delete the last ones, or take part of the suggestion, inkling
draws what they will do the moment they arrive, with the suggestion for the
line they make, and sends them on. The probe goes out as always. When bash's
own echo and redraw come back, the cursor is first put back where bash has it
and what was drawn ahead is cleared, in the same write, so bash's drawing lands
where bash expects it and puts right anything that was guessed wrong (a key
bound to something else, say). A fresh prompt's line is known to be empty when
nothing was typed ahead for it (keys typed while a command ran, or Ctrl-O,
which puts a line of the history into the next prompt), and the cursor's
column is followed from the last carriage return of a command's output, so the
first key of a command is drawn ahead too. Nothing is drawn ahead that would
reach the end of the row, and automatic wrapping is off while it is.

**History.** From Atuin's database (`~/.local/share/atuin/history.db`, opened
read-only, `query_only` on) if it is there, else `$HISTFILE`. The commands are
read into memory on a thread of their own when the session starts, so the
shell's first prompt waits for nothing, and what is new is read at each
prompt. Every distinct command is kept once in a list sorted by its text: the
commands that start with what you typed are one run of that list, found by two
binary searches. Each directory has a list of the same kind for what was run
there, and the project the shell is in (the tree under the nearest directory
with a `.git`, `.hg` or `.jj`) gets one made from the directories under it.

**Which command.** Of the commands that start with the line, the one suggested
is the likeliest to be meant, as a mixture of four estimates, each a share
that sums to one over the commands it knows:

* its share of all the runs in the history;
* its share of the runs in this project;
* its share of the runs in this very directory;
* how often it came next, in the same session, after the command just run.

In each, an old run counts for less than a recent one: a run's weight halves
every few hundred commands run since, counted in commands and not in hours, so
a week away from the computer ages nothing. A run that failed counts for less
than one that went well (Ctrl-C and a closed pipe are how a long-running
program is left, and count as well; so does an exit status nobody recorded).
Because each share is taken within its own scope, ten commands run in one
directory weigh there as much as ten thousand do in the whole history, and that
is what makes the directory count. Only the few best of the whole history's run
are scored, together with those the directory, the project and the last
command know of, so a keystroke costs microseconds whatever the history's size.
The weights are those that typed the most of its owner's own history when it
was replayed (`inkling replay`); two more estimates were tried and left out,
because the replay showed they added nothing: what the session had run, and
what came after any command of the same program.

**Paths.** As fish does, a command from the history is only suggested if its
paths are there from here: every word shaped like a path (starting with `/`,
`~/`, `./` or `../`) must name something that exists, and what `cd`, `pushd`
or `rmdir` is given must be a directory. Words that merely have a slash in them
are not judged (`feature/login` is as often a branch as a path), nor are words
the shell would expand. The next likeliest command is suggested instead. What
was found is remembered until the next prompt.

**Names, when the history has nothing.** Then the last word is finished: in a
command's place (the first word, or after `|`, `&&`, `;`, `sudo`) with the name
of a program on the search path, the one used most if any has been, else as
much as all the names that fit share; elsewhere with the name of a file or
directory in the directory the word points into (a slash after a directory, a
backslash before a character bash would take for something else), as much as
the names that fit share, or the one changed last. `cd` is only finished with
directories, and hidden names only when the word starts with a dot. The
directory's names and the programs are read while the prompt is drawn, so no
key waits for them.

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
* Never after a key the terminal turns into a signal: Ctrl-C, Ctrl-Z, Ctrl-\.
  The shell deals with the signal after it has read the keys that came with it,
  and a command that runs in that moment is thrown out with the line; found by
  typing random keys at plain bash and at inkling and comparing. Only a new
  prompt ends this, not even Ctrl-G.
* Never inside a bracketed paste, or in the middle of an escape sequence or a
  character. A probe goes after a paste ends, once, whatever its length.
* One at a time. While a probe is out, the proxy reads nothing more from you
  (your keys wait in the kernel), and gives up after 80 ms, after which that
  prompt gets no more probes.

## Measurements

Release build, bash 5.3.15, this laptop (Omarchy, Arch Linux).

**The race** (`scripts/race`). inkling against plain bash, fish 4.9.3 and ble.sh
(built from its repository as of 2026-09-08), each on a pseudo-terminal of its
own whose output goes through a whole terminal emulator. Each types "git status"
one key at a time at a fresh prompt, towards the oldest command in the history,
"git status --short --branch", twenty times over; the four take turns key by
key, so that whatever else the machine does weighs on them all alike. A key is
done when the row shows what was typed, the cursor after it, and the rest of
the command after the cursor. Microseconds from the key:

| 100,001 commands in the history | echo, median | suggestion, median | suggestion, 95th percentile | worst |
| --- | ---: | ---: | ---: | ---: |
| bash alone (suggests nothing) | 278 | | | |
| inkling | 309 | 309 | 685 | 1,945 |
| fish | 389 | 389 | 22,244 | 29,950 |
| ble.sh | 4,956 | 4,956 | 36,469 | 42,788 |

With 1,001 commands the medians are 261 (bash), 333 (inkling), 379 (fish) and
4,909 (ble.sh), and the suggestion's 95th percentiles 932, 1,217 and 32,359.

So inkling's key and its suggestion reach the screen together, with bash's own
echo: what is left in those numbers is the kernel's pseudo-terminal and the
waking of a process, which no line editor gets under. fish's echo is about as
quick, but with a long history its suggestion comes tens of milliseconds later
whenever the match is an old command; ble.sh is slower on every key. Inside
inkling, finding the suggestion (the ranking, the path checks, the names) took
1.6 microseconds at the median, 12 at the 99th percentile and 43 at worst over
20,000 keys of its owner's own history.

`scripts/measure` times the rest: from a key to the first byte back, inkling
adds 0 to 70 microseconds at the median to plain bash (it added 300 to 500
before keys were drawn ahead). Starting a shell with a minimal startup file
takes 8 to 10 ms alone and about 21 ms with inkling, so about **12 ms** are
added (bash starts twice; with the block at the top of `~/.bashrc` the first
one stops there, and your real startup file runs once); the history is read
beside it, so the first prompt comes as soon with 400,000 commands as with
none. `inkling init bash` takes 0.7 ms to run and print. Output through the
proxy: 18 to 20 MiB/s against 28 to 35 MiB/s for `seq 1 2000000` straight to
the terminal's pseudo-terminal. Memory of the proxy, resident: 4.4 MiB with a
small history file, 9 MiB with 10,000 runs in Atuin, 31 MiB with 100,000 (peak
38 MiB while loading), and about 90 MiB with 400,000 that are nearly all
different. The program is 2.4 MB.

**The replay** (`inkling replay`), on its owner's own Atuin history: 62,982
commands from several machines over the years. The first 70% are learned and
the last 30% typed, each command after the ones before it, as described under
`inkling replay`: 18,736 commands, 210,375 characters.

| | typing saved | the very command after 1 character | 2 | 3 |
| --- | ---: | ---: | ---: | ---: |
| fish's order (the most recent) | 51.7% | 52.5% | 58.2% | 58.8% |
| inkling before (the most recent, this directory first) | 52.6% | 54.1% | 62.0% | 62.8% |
| inkling (the likeliest, as above) | 53.4% | 64.0% | 64.4% | 65.0% |
| knowing every command run before, and nothing of new ones | 50.9% | 87.3% | 83.4% | 83.3% |

The last row is a bound, not a rival: a suggester that always knows the command
when it has been run before. It types less of the commands it has seen than any
order can, and nothing of new ones, where inkling's suggestion of a similar
command still gives its first words. The history was read in 28 ms.

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
* Names are finished from the files and the programs on the search path,
  not from what bash's own completion knows (options, branches, hosts,
  aliases and functions).
* Only plain ASCII is drawn ahead of the shell; a key that types anything else
  waits for bash's report, as every key did before.
* Programs are looked for on the search path inkling was started with, and in
  `~/.local/bin`, `~/.cargo/bin`, mise's shims and `~/bin`; what a startup file
  adds to `PATH` after inkling's block is not seen.
* The replay cannot judge the path checks: whether a path was there when a
  command ran is not recorded anywhere.
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
fzf, Atuin) in a scratch home; drawing ahead, and the shell's redraw putting a
wrong guess right; the project, a `cd` into a directory that is gone, and a
file's name finished. `scripts/check-repo` adds shellcheck and the install and
uninstall tests, against a scratch `HOME` only. The race and the measurements
are tests too, run by hand: `scripts/race` (it needs fish and ble.sh named in
`RACE_FISH` and `RACE_BLESH`) and `scripts/measure`.

## Credits

The behaviour of ble.sh's auto-complete was read, and the choices here follow
it where they are alike; see `NOTICE`. No code was taken from it. Checking that
a suggestion's paths are there is fish's idea (it records the paths a command
used when it ran; inkling judges them by their shape), and drawing keys ahead
of the far end and putting a wrong guess right is mosh's. No code was taken
from either.

MIT licence, in `LICENSE`.

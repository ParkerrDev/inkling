#!/bin/bash
# inkling on Omarchy: build it, put the program in ~/.local/bin, and add one
# marked block to ~/.bashrc that starts it. Nothing here needs root. Safe to
# re-run: a re-run builds again and brings the block up to date where it is.
#
#   ~/.local/bin/inkling   the program
#   ~/.bashrc              one block between the lines
#                            # >>> inkling >>>
#                            # <<< inkling <<<
#                          at the top, so that the shell starts once and not
#                          twice (a file that is a link is edited where it
#                          really is, and stays a link)
#
# INKLING_PREBUILT=/path/to/inkling installs that program instead of building
# one. The only files touched are those two, under $HOME.
set -euo pipefail

HERE=$(dirname "$(readlink -f "$0")")
BIN=$HOME/.local/bin
START='# >>> inkling >>>'
END='# <<< inkling <<<'
say() { printf '\033[1m%s\033[0m\n' "$*"; }

BLOCK="$START
# Fish-like suggestions for bash: https://github.com/ParkerrDev/inkling
# Keep this at the top of the file, so that the shell starts once, not twice.
# Remove it by running inkling's uninstall.sh.
command -v inkling >/dev/null 2>&1 && eval \"\$(inkling init bash)\"
$END"

if [ -n "${INKLING_PREBUILT:-}" ]; then
  say "Using $INKLING_PREBUILT"
  PROGRAM=$INKLING_PREBUILT
else
  command -v cargo >/dev/null || {
    echo "inkling is built with Rust, and cargo is not installed. On Omarchy:" >&2
    echo "  omarchy install dev-env rust" >&2
    exit 1
  }
  say "Building (a few minutes the first time)"
  (cd "$HERE" && cargo build --release --locked --quiet)
  PROGRAM=$HERE/target/release/inkling
fi

say "Program"
mkdir -p "$BIN"
# By rename, so a copy that is running is never written into.
install -m 0755 "$PROGRAM" "$BIN/.inkling.new"
mv -f "$BIN/.inkling.new" "$BIN/inkling"
echo "  $("$BIN/inkling" version) in $BIN"

say ".bashrc"
rc=$HOME/.bashrc
# Where the file really is, if ~/.bashrc is a link.
target=$(readlink -f "$rc")
if [ ! -e "$target" ]; then
  mkdir -p "$(dirname "$target")"
  printf '%s\n' "$BLOCK" >"$target"
  echo "  made $rc with the block in it"
elif grep -qxF "$START" "$target"; then
  grep -qxF "$END" "$target" || {
    echo "  $target has the start of the block and not its end; fix that by hand, then run this again" >&2
    exit 1
  }
  new=$(mktemp "$target.inkling.XXXXXX")
  awk -v block="$BLOCK" -v start="$START" -v end="$END" '
    $0 == start { print block; skipping = 1; next }
    skipping { if ($0 == end) skipping = 0; next }
    { print }
  ' "$target" >"$new"
  if cmp -s "$new" "$target"; then
    rm -f "$new"
    echo "  the block is there and up to date"
  else
    chmod --reference="$target" "$new"
    mv -f "$new" "$target"
    echo "  the block is brought up to date where it was"
  fi
else
  new=$(mktemp "$target.inkling.XXXXXX")
  {
    # A first line that names the interpreter stays first.
    if head -n 1 "$target" | grep -q '^#!'; then
      head -n 1 "$target"
      printf '%s\n' "$BLOCK"
      tail -n +2 "$target"
    else
      printf '%s\n' "$BLOCK"
      cat "$target"
    fi
  } >"$new"
  chmod --reference="$target" "$new"
  mv -f "$new" "$target"
  echo "  the block is at the top of $rc"
fi

say "Yours to do, in a terminal"
echo "  open a new terminal      the next shell starts with suggestions"
echo "  inkling doctor           what is set up, and what works"
echo "  INKLING=0 bash           a shell without it, for one session"

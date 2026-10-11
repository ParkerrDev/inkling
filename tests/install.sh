#!/bin/bash
# install.sh and uninstall.sh, against a scratch HOME and nowhere else: the
# program lands where it should, the block goes at the top of ~/.bashrc and
# nothing else in it changes, a re-run changes nothing it should not, a link
# stays a link, and removing leaves ~/.bashrc as it was.
#
#   tests/install.sh
set -euo pipefail

ROOT=$(cd "$(dirname "$(readlink -f "$0")")/.." && pwd)
SCRATCH=$(mktemp -d "${TMPDIR:-/tmp}/inkling-install.XXXXXX")
trap 'rm -rf "$SCRATCH"' EXIT

# The real ~/.bashrc must come out of this exactly as it went in.
real_before=$(sha256sum "$HOME/.bashrc" 2>/dev/null || echo none)

failures=0
check() { # check DESCRIPTION COMMAND...
  local what=$1
  shift
  if "$@"; then
    printf '  ok    %s\n' "$what"
  else
    printf '  FAIL  %s\n' "$what"
    failures=$((failures + 1))
  fi
}

# shellcheck disable=SC2016 # the eval line is looked for as the text it is
EVAL_LINE='eval "$(inkling init bash)"'

(cd "$ROOT" && cargo build --quiet)
export INKLING_PREBUILT=$ROOT/target/debug/inkling
quiet() { "$@" >/dev/null 2>&1; }
# The scripts as a person would run them, in a world of their own.
as_home() { # as_home HOME COMMAND...
  local home=$1
  shift
  env -i HOME="$home" PATH="$home/.local/bin:/usr/bin:/bin" INKLING_PREBUILT="$INKLING_PREBUILT" "$@"
}

ORIGINAL='# my bashrc
[[ $- != *i* ]] && return
alias hx="helix"
export EDITOR=helix'

echo "install into a bashrc that exists"
H1=$SCRATCH/one
mkdir -p "$H1"
printf '%s\n' "$ORIGINAL" >"$H1/.bashrc"
cp "$H1/.bashrc" "$SCRATCH/original-one"
as_home "$H1" "$ROOT/install.sh" >"$SCRATCH/install.out" 2>&1 || { cat "$SCRATCH/install.out"; exit 1; }
check "the program is installed and runs" test -x "$H1/.local/bin/inkling"
check "it says its version" quiet as_home "$H1" "$H1/.local/bin/inkling" version
check "the block starts the file" test "$(head -n 1 "$H1/.bashrc")" = '# >>> inkling >>>'
check "the block has the eval line" grep -qF "$EVAL_LINE" "$H1/.bashrc"
check "there is one block" test "$(grep -c '^# >>> inkling >>>$' "$H1/.bashrc")" = 1
check "everything else follows, as it was" cmp -s <(tail -n "$(wc -l <"$SCRATCH/original-one")" "$H1/.bashrc") "$SCRATCH/original-one"
check "the doctor sees the block" bash -c "HOME='$H1' '$H1/.local/bin/inkling' doctor 2>&1 | grep -q 'starts inkling'"
cp "$H1/.bashrc" "$SCRATCH/after-first"
as_home "$H1" "$ROOT/install.sh" >/dev/null 2>&1
check "a re-run changes nothing" cmp -s "$H1/.bashrc" "$SCRATCH/after-first"

echo "the block is kept where it is, and brought up to date"
python3 - "$H1/.bashrc" <<'PY'
import sys
path = sys.argv[1]
text = open(path).read()
text = text.replace("Fish-like suggestions for bash", "OLD WORDING")
open(path, "w").write(text)
PY
as_home "$H1" "$ROOT/install.sh" >/dev/null 2>&1
check "the wording is brought up to date" cmp -s "$H1/.bashrc" "$SCRATCH/after-first"

echo "a shell that starts from it is wrapped (under script, on a pseudo-terminal)"
if command -v script >/dev/null 2>&1; then
  # shellcheck disable=SC2016 # the shell that script starts expands these
  wrapped=$(printf 'echo parent=$(</proc/$PPID/comm)\nexit\n' | as_home "$H1" TERM=xterm script -qec 'bash -i' /dev/null 2>&1 | tr -d '\r' | grep -c 'parent=inkling' || true)
  check "its parent is inkling" test "$wrapped" -ge 1
else
  echo "  script is not installed: skipped"
fi

echo "uninstall"
as_home "$H1" "$ROOT/uninstall.sh" >"$SCRATCH/uninstall.out" 2>&1 || { cat "$SCRATCH/uninstall.out"; exit 1; }
check "the program is gone" test ! -e "$H1/.local/bin/inkling"
check "the bashrc is as it was" cmp -s "$H1/.bashrc" "$SCRATCH/original-one"
quiet as_home "$H1" "$ROOT/uninstall.sh"
check "a second uninstall is fine" test $? -eq 0

echo "install with no bashrc at all"
H2=$SCRATCH/two
mkdir -p "$H2"
as_home "$H2" "$ROOT/install.sh" >/dev/null 2>&1
check "a bashrc is made with the block" grep -qF "$EVAL_LINE" "$H2/.bashrc"
as_home "$H2" "$ROOT/uninstall.sh" >/dev/null 2>&1
check "and uninstall leaves it empty of the block" test "$(grep -c inkling "$H2/.bashrc")" = 0

echo "a bashrc that is a link stays a link"
H3=$SCRATCH/three
mkdir -p "$H3" "$SCRATCH/dotfiles"
printf '%s\n' "$ORIGINAL" >"$SCRATCH/dotfiles/bashrc"
ln -s "$SCRATCH/dotfiles/bashrc" "$H3/.bashrc"
as_home "$H3" "$ROOT/install.sh" >/dev/null 2>&1
check "it is still a link" test -L "$H3/.bashrc"
check "the target has the block" grep -qF "$EVAL_LINE" "$SCRATCH/dotfiles/bashrc"
as_home "$H3" "$ROOT/uninstall.sh" >/dev/null 2>&1
check "the target is as it was" cmp -s "$SCRATCH/dotfiles/bashrc" "$SCRATCH/original-one"
check "and it is still a link" test -L "$H3/.bashrc"

echo "a file whose first line names the interpreter keeps it first"
H4=$SCRATCH/four
mkdir -p "$H4"
printf '#!/bin/bash\necho hi\n' >"$H4/.bashrc"
as_home "$H4" "$ROOT/install.sh" >/dev/null 2>&1
check "the interpreter line is first" test "$(head -n 1 "$H4/.bashrc")" = '#!/bin/bash'
check "the block is next" test "$(sed -n 2p "$H4/.bashrc")" = '# >>> inkling >>>'

echo "nothing outside the scratch home was touched"
check "the real bashrc is exactly as it was" test "$(sha256sum "$HOME/.bashrc" 2>/dev/null || echo none)" = "$real_before"

if [ "$failures" -gt 0 ]; then
  echo "$failures failed"
  exit 1
fi
echo "all passed"

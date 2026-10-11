#!/bin/bash
# Take inkling off this machine: the block in ~/.bashrc and the program in
# ~/.local/bin. The rest of ~/.bashrc is left exactly as it is, and there is
# nothing else to remove: inkling keeps no data of its own, and reads the
# history that Atuin and bash keep without changing it. Safe to re-run.
#
#   ./uninstall.sh
set -euo pipefail

BIN=$HOME/.local/bin
START='# >>> inkling >>>'
END='# <<< inkling <<<'
say() { printf '\033[1m%s\033[0m\n' "$*"; }

say ".bashrc"
target=$(readlink -f "$HOME/.bashrc")
if [ -e "$target" ] && grep -qxF "$START" "$target"; then
  grep -qxF "$END" "$target" || {
    echo "  $target has the start of the block and not its end; remove the block by hand" >&2
    exit 1
  }
  new=$(mktemp "$target.inkling.XXXXXX")
  awk -v start="$START" -v end="$END" '
    $0 == start { skipping = 1; next }
    skipping { if ($0 == end) skipping = 0; next }
    { print }
  ' "$target" >"$new"
  chmod --reference="$target" "$new"
  mv -f "$new" "$target"
  echo "  removed the block from $target"
else
  echo "  no block there"
fi

say "Program"
rm -f "$BIN/inkling" "$BIN/.inkling.new"
echo "  removed $BIN/inkling"
echo
echo "Shells that are open keep running inside it until they are closed."

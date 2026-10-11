# inkling: fish-like suggestions for bash. Printed by `inkling init bash`; put
# this in ~/.bashrc as   eval "$(inkling init bash)"   (install.sh does).
#
# It runs in two places. In the first shell it starts the proxy once, on a real
# terminal, and the shell becomes that proxy's child; in that child it
# installs the marks and the probe. Nowhere else does it do anything.

if [[ $- == *i* && ${INKLING-} != 0 ]] &&
  ((BASH_VERSINFO[0] > 5 || (BASH_VERSINFO[0] == 5 && BASH_VERSINFO[1] >= 1))); then
  if [[ -z ${INKLING_NONCE-} ]]; then
    # Not inside a proxy yet: start one in place of this shell. The shell is
    # started again exactly as it was started this time: the same words on
    # its command line, and the environment it began with (terminals that
    # add their own shell integration do it through both, and a variable set
    # since would be set twice).
    if [[ -t 0 && -t 1 && -t 2 && ${TERM-dumb} != dumb ]] && command -v inkling >/dev/null 2>&1; then
      mapfile -d '' -t __inkling_argv </proc/$$/cmdline 2>/dev/null
      ((${#__inkling_argv[@]})) || __inkling_argv=("$0")
      # The environment goes along in a variable of its own, quoted one entry
      # to a line. It is read now: once this process has become the proxy,
      # /proc/$$/environ is the proxy's, not this shell's.
      mapfile -d '' -t __inkling_environ </proc/$$/environ 2>/dev/null
      printf -v __inkling_environ '%q\n' "${__inkling_environ[@]}"
      # shellcheck disable=SC2093 # if the proxy cannot start, the shell goes on
      INKLING_ENVIRON=$__inkling_environ INKLING_HISTFILE=${HISTFILE-} \
        exec inkling run --arg0 "${__inkling_argv[0]}" -- "$BASH" "${__inkling_argv[@]:1}"
      unset -v __inkling_argv __inkling_environ
    fi
  elif [[ ${INKLING_SHELL_PID-} == "$$" ]]; then
    # The proxy's own shell. What it was given in the environment is kept as
    # plain shell variables, so that nothing this shell starts inherits it: a
    # terminal started from here gets a proxy of its own.
    __inkling_nonce=$INKLING_NONCE
    export -n INKLING_NONCE INKLING_SHELL_PID

    # A prompt is about to be drawn. Leaves $? as it found it.
    __inkling_prompt() {
      local status=$? flags=
      [[ -o vi ]] && flags=v
      printf '\e]7777;%s;P;%s\a' "$__inkling_nonce" "$flags"
      return "$status"
    }

    # Tells the proxy what is in the line being edited and where the cursor
    # is. It runs from a key binding that only the proxy ever sends. The
    # quoting is the shell's own (%q), one builtin call, nothing forked.
    # The binding passes $_ along as the last argument, which is how bash
    # puts $_ back as it was once the function has run.
    __inkling_probe() {
      printf '\e]7777;%s;L;%d;%d;%q\a' "$__inkling_nonce" "$READLINE_POINT" "${#READLINE_LINE}" "$READLINE_LINE"
    }
    # shellcheck disable=SC2016 # $_ is for the shell to expand when the key is pressed
    bind -x '"\C-x\C-_I\a": __inkling_probe "$_"' 2>/dev/null

    # A command is about to run.
    PS0=${PS0-}$'\e]7777;'$__inkling_nonce$';R\a'

    # PROMPT_COMMAND is an array in some setups and a string in others. The
    # hook goes first, not last: bash-preexec, which Atuin uses, ends its own
    # prompt command with a marker meaning "the next command is the person's",
    # and any command that runs after it would be taken for the person's.
    # The hook leaves $? as it found it, so that what follows sees it.
    if [[ ${PROMPT_COMMAND@a} == *a* ]]; then
      PROMPT_COMMAND=(__inkling_prompt "${PROMPT_COMMAND[@]}")
    else
      # shellcheck disable=SC2178,SC2128 # a string here, as the test above found
      PROMPT_COMMAND=__inkling_prompt${PROMPT_COMMAND:+$'\n'$PROMPT_COMMAND}
    fi
  fi
fi

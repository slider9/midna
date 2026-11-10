# midna shell integration (written by midnad; setting agents.adopt_typed).
# midna starts bash with `--init-file` pointing here (a login shell can't take one): run the
# startup files bash would have run, then before each prompt keep midna's shims first on
# PATH and send `claude` / `codex` aliases through them. (bash aliases can't contain a
# slash, so a binary typed in full runs as typed.) Git Bash on Windows gets a Windows path in
# MIDNA_SHIMS, while its PATH holds /c/… ones.
_midna_shims=$MIDNA_SHIMS
case "$_midna_shims" in [A-Za-z]:[\\/]*) _midna_shims=$(/usr/bin/cygpath -u "$_midna_shims" 2>/dev/null || printf %s "$_midna_shims") ;; esac
if [ -n "${MIDNA_BASH_LOGIN-}" ]; then
  unset MIDNA_BASH_LOGIN
  [ -r /etc/profile ] && . /etc/profile
  if [ -r ~/.bash_profile ]; then . ~/.bash_profile
  elif [ -r ~/.bash_login ]; then . ~/.bash_login
  elif [ -r ~/.profile ]; then . ~/.profile
  fi
else
  [ -r /etc/bash.bashrc ] && . /etc/bash.bashrc
  [ -r ~/.bashrc ] && . ~/.bashrc
fi
_midna_prompt() {
  local st=$? a def first rest real
  case "$PATH" in "$_midna_shims"|"$_midna_shims":*) ;; *) PATH="$_midna_shims:$PATH" ;; esac
  for a in claude codex; do
    def=$(alias "$a" 2>/dev/null) || continue
    eval "def=${def#alias $a=}"
    case "$def" in MIDNA_SHIM_REAL=*) continue ;; esac
    first=${def%% *}
    rest=${def#"$first"}
    case "$first" in "~"/*) real="$HOME${first#"~"}" ;; *) real=$first ;; esac
    case "$real" in /*/"$a") [ -x "$real" ] || continue ;; *) continue ;; esac
    alias "$a=MIDNA_SHIM_REAL=$(printf %q "$real") $(printf %q "$_midna_shims")/$a$rest"
  done
  return $st
}
PROMPT_COMMAND="_midna_prompt${PROMPT_COMMAND:+;$PROMPT_COMMAND}"

# Finds the daemon a scratch tmux server started. Diffing pgrep output is not
# enough: another server's daemon may start mid-test. toggle hands each daemon
# the server's unique @agenmux-generation as AGENMUX_GENERATION; only that
# variable is read from a candidate's environment.
scratch_daemon() {
  local generation pid
  generation="$(tmux -S "$1" show-option -gqv @agenmux-generation)"
  [ -n "$generation" ] || return 1
  for pid in $(pgrep -f 'agenmux daemon' 2>/dev/null); do
    if [ -r "/proc/$pid/environ" ]; then
      grep -qzxF "AGENMUX_GENERATION=$generation" "/proc/$pid/environ" 2>/dev/null
    else
      ps -E -ww -o command= -p "$pid" 2>/dev/null |
        grep -o 'AGENMUX_GENERATION=[^ ]*' | grep -qxF "AGENMUX_GENERATION=$generation"
    fi && {
      echo "$pid"
      return 0
    }
  done
  return 1
}

#!/usr/bin/env bash
# End-to-end regression for the preserved sidebar's native client key table.
# The first invocation omits a client to verify newest-real-client discovery.
set -euo pipefail
export TERM=xterm

DIR="$(cd "$(dirname "$0")/.." && pwd)"
BIN="${AGENMUX_BIN:-$DIR/target/release/agenmux}"
[ -x "$BIN" ] || exit 0
command -v tmux >/dev/null || exit 0
command -v expect >/dev/null || exit 0

tmp="$(mktemp -d "${TMPDIR:-/tmp}/agenmux-navigation.XXXXXX")"
sock="$tmp/sock"
input="$tmp/client-input"
client_pid=''
secondary_pid=''
input_open=0
cleaned=0
rows_own=''
vanished_rows=''

cleanup() {
  [ "$cleaned" -eq 0 ] || return
  cleaned=1
  tmux -S "$sock" kill-server 2>/dev/null || true
  if [ "$input_open" -eq 1 ]; then
    exec 9>&-
  fi
  [ -z "$client_pid" ] || wait "$client_pid" 2>/dev/null || true
  [ -z "$secondary_pid" ] || wait "$secondary_pid" 2>/dev/null || true
  rm -f "$input" "$sock" "$tmp/agenmux-keys" \
    "$tmp/agenmux-rows" "$tmp/agenmux-scan-cache" "$tmp/codex" \
    "$tmp/script.log" "$tmp/secondary.log" \
    "$tmp/wheel-reader" "$tmp/wheel-key" "$tmp/wheel-ready" \
    "$tmp/agenmux-wheel"
  [ -z "$rows_own" ] || rm -f "$rows_own"
  [ -z "$vanished_rows" ] || rm -f "$vanished_rows"
  rmdir "$tmp" 2>/dev/null || true
}
trap cleanup EXIT
trap 'exit 130' HUP INT TERM

printf '#!/bin/sh\nwhile :; do sleep 10; done\n' >"$tmp/codex"
chmod +x "$tmp/codex"
mkfifo "$input"

TMPDIR="$tmp" tmux -S "$sock" -f /dev/null new-session \
  -d -s navigation -x 120 -y 32 "$tmp/codex"
tmux -S "$sock" new-window -d -t navigation: "$tmp/codex"
tmux -S "$sock" set-option -g @agenmux-bin "$BIN"
tmux -S "$sock" set-option -g @agenmux-width 30
tmux -S "$sock" set-option -g prefix M-a
# The sidebar must behave like a regular pane for the user's root-table
# bindings. This deliberately differs from tmux's defaults so the test proves
# the configured command is inherited rather than hard-coded by the plugin.
tmux -S "$sock" bind-key -n C-l select-pane -R

# Attach through a genuine pseudo-terminal and relay test keys from the FIFO.
# Unlike script(1), expect does not require this test's own stdin to be a tty.
expect -f - "$input" "$sock" >"$tmp/script.log" 2>&1 <<'EXPECT' &
log_user 0
set timeout -1
set input [lindex $argv 0]
set socket [lindex $argv 1]
spawn tmux -S $socket attach-session -t navigation
set tmux_spawn $spawn_id
set keys [open $input r]
fconfigure $keys -blocking 0 -buffering none -translation binary
proc relay_key {} {
  set data [read $::keys]
  if {[string length $data] > 0} {
    send -i $::tmux_spawn -raw -- $data
  }
  if {[eof $::keys]} {
    close $::keys
    exit 0
  }
}
fileevent $keys readable relay_key
expect -i $tmux_spawn eof
EXPECT
client_pid=$!
# Opening the writer after expect starts unblocks its FIFO reader.
exec 9>"$input"
input_open=1

client=''
for _ in $(seq 1 30); do
  client="$(tmux -S "$sock" list-clients \
    -f '#{?#{m:*control-mode*,#{client_flags}},0,1}' \
    -F '#{client_name}' 2>/dev/null | head -n 1)"
  [ -n "$client" ] && break
  sleep 0.1
done
[ -n "$client" ] || {
  echo "FAIL navigation-key-table: no attached client"
  sed -n '1,20p' "$tmp/script.log"
  exit 1
}

server_pid="$(tmux -S "$sock" display-message -p '#{pid}')"
# No client argument exercises native newest-real-client discovery.
env TMPDIR="$tmp" TMUX="$sock,$server_pid,0" AGENMUX_DIR="$DIR" \
  "$BIN" toggle split

# Native toggle invokes setup, which must preserve the
# user's root binding and install synchronous search delivery before use.
normal_keys="$(tmux -S "$sock" list-keys -T agenmux)"
search_keys="$(tmux -S "$sock" list-keys -T agenmux-search)"
[ "$(tmux -S "$sock" show-option -gqv @agenmux-nav-version)" = 12 ] &&
  printf '%s' "$normal_keys" | grep -Fq 'C-l' &&
  printf '%s' "$normal_keys" | grep -Fq " key 'search'" &&
  printf '%s' "$search_keys" | grep -Fq 'text-6A' || {
  echo "FAIL navigation-key-table: native setup contract missing"
  exit 1
}

sidebar=''
first=''
for _ in $(seq 1 40); do
  sidebar="$(tmux -S "$sock" list-panes -t navigation: \
    -F '#{pane_id}	#{pane_title}' |
    awk -F'\t' '$2 == "agenmux" { print $1; exit }')"
  if [ -n "$sidebar" ]; then
    first="$(tmux -S "$sock" capture-pane -p -t "$sidebar" |
      sed -n '/❯/p' | head -n 1)"
    [ -n "$first" ] && break
  fi
  sleep 0.1
done
[ -n "$first" ] || {
  echo "FAIL navigation-key-table: sidebar did not render a selection"
  exit 1
}

table="$(tmux -S "$sock" display-message -p -c "$client" '#{client_key_table}')"
initial_focus="$(tmux -S "$sock" display-message -p -c "$client" \
  '#{pane_title}')"
initial_hint="$(tmux -S "$sock" capture-pane -p -t "$sidebar" | sed -n '2p')"
inactive_hint_hidden=0
if [ -n "$initial_hint" ] &&
  ! printf '%s' "$initial_hint" | grep -Eq 'esc clear|f status|/ search'; then
  inactive_hint_hidden=1
fi

# Opening prefix+w from the sidebar zooms that pane while choose-tree is open.
# The temporary full-window width must never be adopted as the user's sidebar
# width by the daemon's border-drag detector.
printf '\033aw' >&9
chooser_open_unzoomed=0
for _ in $(seq 1 20); do
  chooser_state="$(tmux -S "$sock" display-message -p -t "$sidebar" \
    '#{pane_in_mode}/#{window_zoomed_flag}')"
  if [ "$chooser_state" = 1/0 ]; then
    chooser_open_unzoomed=1
    break
  fi
  sleep 0.05
done
sleep 2.5
printf 'q' >&9
for _ in $(seq 1 20); do
  chooser_state="$(tmux -S "$sock" display-message -p -t "$sidebar" \
    '#{pane_in_mode}/#{window_zoomed_flag}')"
  [ "$chooser_state" = 0/0 ] && break
  sleep 0.05
done
chooser_width="$(tmux -S "$sock" show-option -gqv @agenmux-width)"

# A configured root-table binding must work from the selected sidebar. C-l
# moves right into the work pane and, because it does not re-enter the plugin
# table, leaves the client in the normal root table.
printf '\014' >&9
ctrl_l_works=0
for _ in $(seq 1 20); do
  ctrl_l_table="$(tmux -S "$sock" display-message -p -c "$client" \
    '#{client_key_table}')"
  ctrl_l_focus="$(tmux -S "$sock" display-message -p -c "$client" \
    '#{pane_title}')"
  if [ "$ctrl_l_table" = root ] && [ "$ctrl_l_focus" != agenmux ]; then
    ctrl_l_works=1
    break
  fi
  sleep 0.05
done

work="$(tmux -S "$sock" list-panes -t navigation: \
  -F '#{pane_id}	#{pane_title}' |
  awk -F'\t' '$2 != "agenmux" { print $1; exit }')"

# A second real terminal client proves navigation is scoped to the client that
# clicked, even though tmux shares the window's selected pane between viewers.
expect -f - "$sock" >"$tmp/secondary.log" 2>&1 <<'EXPECT' &
log_user 0
set timeout -1
set socket [lindex $argv 0]
spawn tmux -S $socket attach-session -t navigation
expect -i $spawn_id eof
EXPECT
secondary_pid=$!
secondary=''
for _ in $(seq 1 30); do
  secondary="$(tmux -S "$sock" list-clients \
    -f '#{?#{m:*control-mode*,#{client_flags}},0,1}' \
    -F '#{client_name}' 2>/dev/null |
    awk -v primary="$client" '$0 != primary { print; exit }')"
  [ -n "$secondary" ] && break
  sleep 0.1
done
[ -n "$secondary" ] || {
  echo "FAIL navigation-key-table: no secondary attached client"
  exit 1
}

# A missing click-origin client must not guess another viewer and move it.
tmux -S "$sock" switch-client -c "$client" -t "$work"
tmux -S "$sock" switch-client -c "$client" -T root
tmux -S "$sock" switch-client -c "$secondary" -T root
env TMPDIR="$tmp" TMUX="$sock,$server_pid,0" \
  "$BIN" click "$sidebar" 0 ''
sleep 0.1
missing_client_focus="$(tmux -S "$sock" display-message -p -c "$client" \
  '#{pane_id}')"
missing_client_table="$(tmux -S "$sock" display-message -p -c "$client" \
  '#{client_key_table}')"
missing_secondary_table="$(tmux -S "$sock" display-message -p -c "$secondary" \
  '#{client_key_table}')"
missing_client_noop=0
if [ "$missing_client_focus" = "$work" ] &&
  [ "$missing_client_table" = root ] &&
  [ "$missing_secondary_table" = root ]; then
  missing_client_noop=1
fi

# Re-enter through a real open-space coordinate. It must focus the sidebar,
# color its cursor green, and activate navigation only for the clicking client.
tmux -S "$sock" switch-client -c "$client" -t "$work"
tmux -S "$sock" switch-client -c "$client" -T root
tmux -S "$sock" switch-client -c "$secondary" -T root
pane_height="$(tmux -S "$sock" display-message -p -t "$sidebar" \
  '#{pane_height}')"
empty_click_y=$((pane_height - 1))
env TMPDIR="$tmp" TMUX="$sock,$server_pid,0" \
  "$BIN" click "$sidebar" "$empty_click_y" "$client"
empty_click_works=0
empty_click_green=0
for _ in $(seq 1 40); do
  empty_click_table="$(tmux -S "$sock" display-message -p -c "$client" \
    '#{client_key_table}')"
  empty_click_focus="$(tmux -S "$sock" display-message -p -c "$client" \
    '#{pane_id}')"
  secondary_click_table="$(tmux -S "$sock" display-message -p -c "$secondary" \
    '#{client_key_table}')"
  empty_click_cursor="$(tmux -S "$sock" capture-pane -e -p -t "$sidebar" |
    sed -n '/❯/p' | head -n 1)"
  case "$empty_click_cursor" in
  # the cursor row also carries a state background, so the green may sit a
  # sequence away from the mark
  *$'\033['*'32m'*'❯'*) empty_click_green=1 ;;
  esac
  if [ "$empty_click_table" = agenmux ] &&
    [ "$empty_click_focus" = "$sidebar" ] &&
    [ "$secondary_click_table" = root ] &&
    [ "$empty_click_green" -eq 1 ]; then
    empty_click_works=1
    break
  fi
  sleep 0.05
done
table="$empty_click_table"

# A row can outlive its agent pane until the daemon's next scan. Treat that
# stale record like open space instead of leaving the click dead.
rows_own="$tmp/agenmux-rows"
cp "$rows_own" "$rows_own.saved"
tmux -S "$sock" switch-client -c "$client" -T root
tmux -S "$sock" select-pane -t "$work"
printf '%%999999\tstale\n' >"$rows_own"
env TMPDIR="$tmp" TMUX="$sock,$server_pid,0" \
  "$BIN" click "$sidebar" 1 "$client"
mv "$rows_own.saved" "$rows_own"
stale_click_works=0
for _ in $(seq 1 20); do
  stale_click_table="$(tmux -S "$sock" display-message -p -c "$client" \
    '#{client_key_table}')"
  stale_click_focus="$(tmux -S "$sock" display-message -p -c "$client" \
    '#{pane_title}')"
  if [ "$stale_click_table" = agenmux ] &&
    [ "$stale_click_focus" = agenmux ]; then
    stale_click_works=1
    break
  fi
  sleep 0.05
done

# Header and separator rows are also non-agent locations.
non_agent_locations_work=1
for non_agent_y in 0 1; do
  tmux -S "$sock" switch-client -c "$client" -t "$work"
  tmux -S "$sock" switch-client -c "$client" -T root
  tmux -S "$sock" select-pane -t "$work"
  env TMPDIR="$tmp" TMUX="$sock,$server_pid,0" \
    "$BIN" click "$sidebar" "$non_agent_y" "$client"
  location_works=0
  for _ in $(seq 1 20); do
    location_table="$(tmux -S "$sock" display-message -p -c "$client" \
      '#{client_key_table}')"
    location_focus="$(tmux -S "$sock" display-message -p -c "$client" \
      '#{pane_id}')"
    if [ "$location_table" = agenmux ] &&
      [ "$location_focus" = "$sidebar" ]; then
      location_works=1
      break
    fi
    sleep 0.05
  done
  [ "$location_works" -eq 1 ] || non_agent_locations_work=0
done

# An actual agent record keeps the existing one-click direct jump. Pick a row
# outside this window so merely leaving the work pane cannot satisfy the test.
valid_row="$(awk -v work="$work" \
  '$1 ~ /^%/ && $1 != work { print NR; exit }' "$tmp/agenmux-rows")"
valid_target="$(awk -v work="$work" \
  '$1 ~ /^%/ && $1 != work { print $1; exit }' "$tmp/agenmux-rows")"
[ -n "$valid_row" ] && [ -n "$valid_target" ] || {
  echo "FAIL navigation-key-table: no cross-window agent row"
  exit 1
}

# Mouse events without a live origin must never guess another client, even
# when their row map still names a valid agent target.
tmux -S "$sock" switch-client -c "$client" -t "$work"
tmux -S "$sock" switch-client -c "$client" -T root
tmux -S "$sock" switch-client -c "$secondary" -T root
env TMPDIR="$tmp" TMUX="$sock,$server_pid,0" \
  "$BIN" click "$sidebar" "$valid_row" ''
sleep 0.1
agent_missing_focus="$(tmux -S "$sock" display-message -p -c "$client" \
  '#{pane_id}')"
agent_missing_primary_table="$(tmux -S "$sock" display-message -p -c "$client" \
  '#{client_key_table}')"
agent_missing_secondary_table="$(tmux -S "$sock" display-message -p \
  -c "$secondary" '#{client_key_table}')"
agent_missing_client_noop=0
if [ "$agent_missing_focus" = "$work" ] &&
  [ "$agent_missing_primary_table" = root ] &&
  [ "$agent_missing_secondary_table" = root ]; then
  agent_missing_client_noop=1
fi

# Likewise, a delayed event from a sidebar pane that has already vanished must
# not jump through a still-valid row map.
vanished_pane='%999998'
vanished_rows="$tmp/agenmux-rows-${vanished_pane#%}"
tmux -S "$sock" switch-client -c "$client" -t "$work"
tmux -S "$sock" switch-client -c "$client" -T root
printf '%s\tlive-target\n' "$valid_target" >"$vanished_rows"
tmux -S "$sock" set-option -g @agenmux-on 0
env TMPDIR="$tmp" TMUX="$sock,$server_pid,0" \
  "$BIN" click "$vanished_pane" 2 "$client"
tmux -S "$sock" set-option -g @agenmux-on 1
sleep 0.1
vanished_sidebar_focus="$(tmux -S "$sock" display-message -p -c "$client" \
  '#{pane_id}')"
vanished_sidebar_table="$(tmux -S "$sock" display-message -p -c "$client" \
  '#{client_key_table}')"
vanished_sidebar_noop=0
if [ "$vanished_sidebar_focus" = "$work" ] &&
  [ "$vanished_sidebar_table" = root ]; then
  vanished_sidebar_noop=1
fi

valid_click_works=0
valid_click_table=''
valid_click_focus=''
if [ -n "$valid_row" ] && [ -n "$valid_target" ]; then
  tmux -S "$sock" switch-client -c "$client" -t "$work"
  tmux -S "$sock" switch-client -c "$client" -T root
  tmux -S "$sock" select-pane -t "$work"
  env TMPDIR="$tmp" TMUX="$sock,$server_pid,0" \
    "$BIN" click "$sidebar" "$valid_row" "$client"
  for _ in $(seq 1 20); do
    valid_click_table="$(tmux -S "$sock" display-message -p -c "$client" \
      '#{client_key_table}')"
    valid_click_focus="$(tmux -S "$sock" display-message -p -c "$client" \
      '#{pane_id}')"
    if [ "$valid_click_table" = root ] &&
      [ "$valid_click_focus" = "$valid_target" ]; then
      valid_click_works=1
      break
    fi
    sleep 0.05
  done
fi

# Restore the sidebar as the interaction target for the keyboard checks below.
tmux -S "$sock" switch-client -c "$client" -t "$sidebar"
tmux -S "$sock" switch-client -c "$client" -T agenmux
for _ in $(seq 1 20); do
  table="$(tmux -S "$sock" display-message -p -c "$client" \
    '#{client_key_table}')"
  [ "$table" = agenmux ] && break
  sleep 0.05
done

control=''
control_flags=''
for _ in $(seq 1 20); do
  control="$(tmux -S "$sock" show-option -gqv @agenmux-control-client)"
  control_flags="$(tmux -S "$sock" list-clients \
    -f "#{==:#{client_name},$control}" -F '#{client_flags}' 2>/dev/null)"
  printf '%s' "$control_flags" | grep -Fq control-mode && break
  sleep 0.05
done
picker_start="$(tmux -S "$sock" capture-pane -p -t "$sidebar" |
  sed -n '/❯/p' | head -n 1)"
printf 'k' >&9
picker_reset="$picker_start"
for _ in $(seq 1 20); do
  picker_reset="$(tmux -S "$sock" capture-pane -p -t "$sidebar" |
    sed -n '/❯/p' | head -n 1)"
  [ -n "$picker_reset" ] && [ "$picker_reset" != "$picker_start" ] && break
  sleep 0.1
done
[ "$picker_reset" != "$picker_start" ] || {
  echo "FAIL navigation-key-table: pre-picker cursor did not move"
  exit 1
}
picker_before="$picker_reset"
printf 'u' >&9
picker_open=0
for _ in $(seq 1 40); do
  picker_frame="$(tmux -S "$sock" capture-pane -p -t "$sidebar")"
  printf '%s\n' "$picker_frame" | grep -Eq 'no releases found|↵ switch' &&
    {
      picker_open=1
      break
    }
  sleep 0.05
done
# A click over an agent row while the picker covers the list must focus the
# picker, not jump through it to the sidebar row drawn underneath.
tmux -S "$sock" switch-client -c "$client" -t "$work"
tmux -S "$sock" switch-client -c "$client" -T root
tmux -S "$sock" select-pane -t "$work"
env TMPDIR="$tmp" TMUX="$sock,$server_pid,0" \
  "$BIN" click "$sidebar" "$valid_row" "$client"
picker_click_works=0
for _ in $(seq 1 20); do
  picker_click_table="$(tmux -S "$sock" display-message -p -c "$client" \
    '#{client_key_table}')"
  picker_click_focus="$(tmux -S "$sock" display-message -p -c "$client" \
    '#{pane_id}')"
  picker_click_frame="$(tmux -S "$sock" capture-pane -p -t "$sidebar")"
  if [ "$picker_click_table" = agenmux ] &&
    [ "$picker_click_focus" = "$sidebar" ] &&
    printf '%s\n' "$picker_click_frame" | grep -Eq 'no releases found|↵ switch'; then
    picker_click_works=1
    break
  fi
  sleep 0.05
done
picker_click_rows=''
picker_click_first=''
if [ "$picker_click_works" -eq 0 ]; then
  picker_click_rows="$(tr '\n' '|' 2>/dev/null <"$tmp/agenmux-rows" || true)"
  picker_click_first="$(printf '%s\n' "$picker_click_frame" | sed -n '1p')"
fi
printf 'q' >&9
picker_reclaimed=0
picker_return=''
for _ in $(seq 1 40); do
  picker_table="$(tmux -S "$sock" display-message -p -c "$client" \
    '#{client_key_table}')"
  picker_frame="$(tmux -S "$sock" capture-pane -p -t "$sidebar")"
  picker_return="$(printf '%s\n' "$picker_frame" | sed -n '/❯/p' | head -n 1)"
  if [ "$picker_table" = agenmux ] && [ -n "$picker_return" ] &&
    ! printf '%s\n' "$picker_frame" | grep -Eq 'no releases found|↵ switch'; then
    picker_reclaimed=1
    break
  fi
  sleep 0.05
done
printf 'j' >&9
second="$picker_return"
for _ in $(seq 1 20); do
  second="$(tmux -S "$sock" capture-pane -p -t "$sidebar" |
    sed -n '/❯/p' | head -n 1)"
  [ -n "$second" ] && [ "$second" != "$picker_return" ] && break
  sleep 0.1
done
table_after_j="$(tmux -S "$sock" display-message -p -c "$client" '#{client_key_table}')"

printf 'k' >&9
third="$second"
for _ in $(seq 1 20); do
  third="$(tmux -S "$sock" capture-pane -p -t "$sidebar" |
    sed -n '/❯/p' | head -n 1)"
  [ "$third" = "$picker_return" ] && break
  sleep 0.1
done

# Preserved-pane mode: the pane has no stdin, so the handler must reach the
# daemon through its key FIFO. Settle-jump off — cursor movement alone here.
tmux -S "$sock" set-option -g @agenmux-wheel-jump off
env TMPDIR="$tmp" TMUX="$sock,$server_pid,0" \
  "$BIN" wheel "$sidebar" down
wheel_down="$third"
for _ in $(seq 1 20); do
  wheel_down="$(tmux -S "$sock" capture-pane -p -t "$sidebar" |
    sed -n '/❯/p' | head -n 1)"
  [ -n "$wheel_down" ] && [ "$wheel_down" != "$third" ] && break
  sleep 0.1
done
env TMPDIR="$tmp" TMUX="$sock,$server_pid,0" \
  "$BIN" wheel "$sidebar" up
wheel_up="$wheel_down"
for _ in $(seq 1 20); do
  wheel_up="$(tmux -S "$sock" capture-pane -p -t "$sidebar" |
    sed -n '/❯/p' | head -n 1)"
  [ "$wheel_up" = "$third" ] && break
  sleep 0.1
done

# Rapid native wheel ticks overwrite one settle deadline. After the one jump,
# returning to the sidebar must stay there—no older timer may fire later.
# Leave one real client so the daemon's established newest-client jump policy
# has one deterministic recipient.
kill "$secondary_pid" 2>/dev/null || true
wait "$secondary_pid" 2>/dev/null || true
secondary_pid=''
for _ in $(seq 1 20); do
  real_clients="$(tmux -S "$sock" list-clients \
    -f '#{?#{m:*control-mode*,#{client_flags}},0,1}' -F '#{client_name}' | wc -l)"
  [ "$real_clients" -eq 1 ] && break
  sleep 0.05
done
tmux -S "$sock" set-option -g @agenmux-wheel-jump 0.5
wheel_client="$(tmux -S "$sock" list-clients \
  -f '#{?#{m:*control-mode*,#{client_flags}},0,1}' \
  -F '#{client_activity} #{client_name}' | sort -n | tail -n 1 | cut -d' ' -f2-)"
env TMPDIR="$tmp" TMUX="$sock,$server_pid,0" "$BIN" wheel "$sidebar" down
sleep 0.05
env TMPDIR="$tmp" TMUX="$sock,$server_pid,0" "$BIN" wheel "$sidebar" up
sleep 0.05
env TMPDIR="$tmp" TMUX="$sock,$server_pid,0" "$BIN" wheel "$sidebar" down
wheel_delay_target=''
for _ in $(seq 1 20); do
  custom_cursor="$(tmux -S "$sock" capture-pane -p -t "$sidebar" |
    sed -n '/❯/p' | head -n 1)"
  if [ -n "$custom_cursor" ] && [ "$custom_cursor" != "$third" ]; then
    cursor_row="$(tmux -S "$sock" capture-pane -p -t "$sidebar" |
      awk '/❯/ { print NR; exit }')"
    if [ -n "$cursor_row" ] && [ "$cursor_row" -gt 1 ]; then
      map_row=$((cursor_row - 1)) # row map excludes the fixed header
      wheel_delay_target="$(sed -n "${map_row}p" "$tmp/agenmux-rows" |
        awk '{ print $1 }')"
    fi
  fi
  case "$wheel_delay_target" in %*) break ;; esac
  sleep 0.05
done
wheel_delay_jumped=0
case "$wheel_delay_target" in
%*)
  for _ in $(seq 1 30); do
    delay_focus="$(tmux -S "$sock" display-message -p -c "$wheel_client" '#{pane_id}')"
    [ "$delay_focus" = "$wheel_delay_target" ] &&
      {
        wheel_delay_jumped=1
        break
      }
    sleep 0.05
  done
  ;;
esac
wheel_delay_works=0
if [ "$wheel_delay_jumped" -eq 1 ]; then
  tmux -S "$sock" switch-client -c "$wheel_client" -t "$sidebar"
  tmux -S "$sock" switch-client -c "$wheel_client" -T agenmux
  sleep 0.65
  delay_focus="$(tmux -S "$sock" display-message -p -c "$wheel_client" '#{pane_id}')"
  [ "$delay_focus" = "$sidebar" ] && wheel_delay_works=1
fi
tmux -S "$sock" set-option -g @agenmux-wheel-jump off

# Simulate leaving through an agent jump, then restoring the exact client's
# processless-sidebar focus and navigation table.
tmux -S "$sock" switch-client -c "$client" -T root
tmux -S "$sock" switch-client -c "$client" -t "$work"
tmux -S "$sock" switch-client -c "$client" -t "$sidebar"
tmux -S "$sock" switch-client -c "$client" -T agenmux
return_table="$(tmux -S "$sock" display-message -p -c "$client" \
  '#{client_key_table}')"
return_focus="$(tmux -S "$sock" display-message -p -c "$client" \
  '#{pane_title}')"
printf 'j' >&9
fourth="$third"
for _ in $(seq 1 20); do
  fourth="$(tmux -S "$sock" capture-pane -p -t "$sidebar" |
    sed -n '/❯/p' | head -n 1)"
  [ -n "$fourth" ] && [ "$fourth" != "$third" ] && break
  sleep 0.1
done

# Live search has its own key table: normal-mode action keys become query text
# and results update per key. Enter accepts query and restores j/k navigation;
# Escape then clears query/filter and restores the complete list.
printf '/navigation' >&9
search_works=0
search_targets=0
search_table=''
search_frame=''
for _ in $(seq 1 60); do
  search_targets="$(awk '$1 ~ /^%/ { seen[$1]=1 } END { for (p in seen) n++; print n+0 }' \
    "$tmp/agenmux-rows")"
  search_frame="$(tmux -S "$sock" capture-pane -p -t "$sidebar" | head -n 1)"
  search_hint="$(tmux -S "$sock" capture-pane -p -t "$sidebar" | sed -n '2p')"
  search_table="$(tmux -S "$sock" display-message -p -c "$client" \
    '#{client_key_table}')"
  if [ "$search_targets" -eq 2 ] &&
    printf '%s' "$search_frame" | grep -Fq '/navigation' &&
    printf '%s' "$search_hint" | grep -Fq 'esc clear' &&
    [ "$search_table" = agenmux-search ]; then
    search_works=1
    break
  fi
  sleep 0.05
done
printf '\r' >&9
search_accept_works=0
for _ in $(seq 1 20); do
  accept_table="$(tmux -S "$sock" display-message -p -c "$client" \
    '#{client_key_table}')"
  accept_frame="$(tmux -S "$sock" capture-pane -p -t "$sidebar" | head -n 1)"
  accept_hint="$(tmux -S "$sock" capture-pane -p -t "$sidebar" | sed -n '2p')"
  if [ "$accept_table" = agenmux ] &&
    printf '%s' "$accept_frame" | grep -Fq '/navigation' &&
    printf '%s' "$accept_hint" | grep -Fq 'j/k'; then
    search_accept_works=1
    break
  fi
  sleep 0.05
done
accepted_cursor="$(tmux -S "$sock" capture-pane -p -t "$sidebar" |
  sed -n '/❯/p' | head -n 1)"
printf 'j' >&9
search_jk_works=0
for _ in $(seq 1 20); do
  filtered_cursor="$(tmux -S "$sock" capture-pane -p -t "$sidebar" |
    sed -n '/❯/p' | head -n 1)"
  if [ -n "$filtered_cursor" ] && [ "$filtered_cursor" != "$accepted_cursor" ]; then
    search_jk_works=1
    break
  fi
  sleep 0.05
done
printf '\033' >&9
search_blur_works=0
for _ in $(seq 1 20); do
  blur_table="$(tmux -S "$sock" display-message -p -c "$client" \
    '#{client_key_table}')"
  blur_targets="$(awk '$1 ~ /^%/ { seen[$1]=1 } END { for (p in seen) n++; print n+0 }' \
    "$tmp/agenmux-rows")"
  blur_frame="$(tmux -S "$sock" capture-pane -p -t "$sidebar" | head -n 1)"
  if [ "$blur_table" = agenmux ] && [ "$blur_targets" -eq 2 ] &&
    ! printf '%s' "$blur_frame" | grep -Fq '/navigation'; then
    search_blur_works=1
    break
  fi
  sleep 0.05
done

# `f` replaces text search and selects the next exact status. It is synchronous
# so fast presses cannot reorder; j/k still navigate the projected result list.
printf 'f' >&9
blocked_filter_works=0
for _ in $(seq 1 20); do
  blocked_targets="$(awk '$1 ~ /^%/ { seen[$1]=1 } END { for (p in seen) n++; print n+0 }' \
    "$tmp/agenmux-rows")"
  blocked_frame="$(tmux -S "$sock" capture-pane -p -t "$sidebar" | head -n 1)"
  blocked_hint="$(tmux -S "$sock" capture-pane -p -t "$sidebar" | sed -n '2p')"
  if [ "$blocked_targets" -eq 0 ] &&
    printf '%s' "$blocked_frame" | grep -Fq '[blocked]' &&
    printf '%s' "$blocked_hint" | grep -Fq 'f status'; then
    blocked_filter_works=1
    break
  fi
  sleep 0.05
done
printf 'f' >&9
working_filter_works=0
for _ in $(seq 1 20); do
  working_targets="$(awk '$1 ~ /^%/ { seen[$1]=1 } END { for (p in seen) n++; print n+0 }' \
    "$tmp/agenmux-rows")"
  working_frame="$(tmux -S "$sock" capture-pane -p -t "$sidebar" | head -n 1)"
  if [ "$working_targets" -eq 0 ] &&
    printf '%s' "$working_frame" | grep -Fq '[working]'; then
    working_filter_works=1
    break
  fi
  sleep 0.05
done
printf 'f' >&9
idle_filter_works=0
for _ in $(seq 1 20); do
  idle_targets="$(awk '$1 ~ /^%/ { seen[$1]=1 } END { for (p in seen) n++; print n+0 }' \
    "$tmp/agenmux-rows")"
  idle_frame="$(tmux -S "$sock" capture-pane -p -t "$sidebar" | head -n 1)"
  if [ "$idle_targets" -eq 2 ] &&
    printf '%s' "$idle_frame" | grep -Fq '[idle]'; then
    idle_filter_works=1
    break
  fi
  sleep 0.05
done
printf '\033' >&9
all_filter_works=0
for _ in $(seq 1 20); do
  all_targets="$(awk '$1 ~ /^%/ { seen[$1]=1 } END { for (p in seen) n++; print n+0 }' \
    "$tmp/agenmux-rows")"
  all_frame="$(tmux -S "$sock" capture-pane -p -t "$sidebar" | head -n 1)"
  if [ "$all_targets" -eq 2 ] &&
    ! printf '%s' "$all_frame" | grep -Eq '/navigation:1|\[(blocked|working|idle|done)\]'; then
    all_filter_works=1
    break
  fi
  sleep 0.05
done

printf 'q' >&9
exit_table=agenmux
q_left=0
# teardown (kill pane + restore layout) is the slowest step: allow 3s
for _ in $(seq 1 60); do
  exit_table="$(tmux -S "$sock" display-message -p -c "$client" \
    '#{client_key_table}')"
  exit_focus="$(tmux -S "$sock" display-message -p -c "$client" \
    '#{pane_title}')"
  sidebar_count="$(tmux -S "$sock" list-panes -a -F '#{pane_title}' \
    2>/dev/null | awk '$0 == "agenmux" { n++ } END { print n+0 }')"
  if [ "$exit_table" = root ] && [ "$exit_focus" != agenmux ] &&
    [ "$sidebar_count" -eq 0 ]; then
    q_left=1
    break
  fi
  sleep 0.05
done

# Escape clears filters without closing. Reopen, create a blocked projection,
# reset it with a literal escape byte, then close explicitly with q.
env TMPDIR="$tmp" TMUX="$sock,$server_pid,0" AGENMUX_DIR="$DIR" \
  "$BIN" toggle split "$client"
escape_ready=0
escape_sidebar=''
for _ in $(seq 1 40); do
  escape_table="$(tmux -S "$sock" display-message -p -c "$client" \
    '#{client_key_table}')"
  escape_focus="$(tmux -S "$sock" display-message -p -c "$client" \
    '#{pane_title}')"
  if [ "$escape_table" = agenmux ] && [ "$escape_focus" = agenmux ]; then
    escape_sidebar="$(tmux -S "$sock" list-panes -a \
      -f '#{==:#{pane_title},agenmux}' -F '#{pane_id}' | head -n 1)"
    [ -n "$escape_sidebar" ] || {
      sleep 0.05
      continue
    }
    escape_ready=1
    break
  fi
  sleep 0.05
done
printf 'f' >&9
for _ in $(seq 1 20); do
  escape_frame="$(tmux -S "$sock" capture-pane -p -t "$escape_sidebar" | head -n 1)"
  printf '%s' "$escape_frame" | grep -Fq '[blocked]' && break
  sleep 0.05
done
printf '\033' >&9
escape_reset=0
for _ in $(seq 1 20); do
  escape_table="$(tmux -S "$sock" display-message -p -c "$client" \
    '#{client_key_table}')"
  escape_focus="$(tmux -S "$sock" display-message -p -c "$client" \
    '#{pane_title}')"
  escape_frame="$(tmux -S "$sock" capture-pane -p -t "$escape_sidebar" | head -n 1)"
  if [ "$escape_table" = agenmux ] && [ "$escape_focus" = agenmux ] &&
    ! printf '%s' "$escape_frame" | grep -Eq '\[(blocked|working|idle|done)\]'; then
    escape_reset=1
    break
  fi
  sleep 0.05
done
printf 'q' >&9
escape_left=0
for _ in $(seq 1 20); do
  escape_table="$(tmux -S "$sock" display-message -p -c "$client" \
    '#{client_key_table}')"
  escape_focus="$(tmux -S "$sock" display-message -p -c "$client" \
    '#{pane_title}')"
  sidebar_count="$(tmux -S "$sock" list-panes -a -F '#{pane_title}' \
    2>/dev/null | awk '$0 == "agenmux" { n++ } END { print n+0 }')"
  if [ "$escape_table" = root ] && [ "$escape_focus" != agenmux ] &&
    [ "$sidebar_count" -eq 0 ]; then
    escape_left=1
    break
  fi
  sleep 0.05
done

# Uppercase Q closes too (alias of q).
env TMPDIR="$tmp" TMUX="$sock,$server_pid,0" AGENMUX_DIR="$DIR" \
  "$BIN" toggle split "$client"
close_ready=0
for _ in $(seq 1 40); do
  close_table="$(tmux -S "$sock" display-message -p -c "$client" \
    '#{client_key_table}')"
  close_focus="$(tmux -S "$sock" display-message -p -c "$client" \
    '#{pane_title}')"
  if [ "$close_table" = agenmux ] && [ "$close_focus" = agenmux ]; then
    close_ready=1
    break
  fi
  sleep 0.05
done
printf 'Q' >&9
q_closed=0
for _ in $(seq 1 20); do
  close_table="$(tmux -S "$sock" display-message -p -c "$client" \
    '#{client_key_table}')"
  sidebar_count="$(tmux -S "$sock" list-panes -a -F '#{pane_title}' \
    2>/dev/null | awk '$0 == "agenmux" { n++ } END { print n+0 }')"
  if [ "$close_table" = root ] && [ "$sidebar_count" -eq 0 ]; then
    q_closed=1
    break
  fi
  sleep 0.05
done

# The notification click helper must target the exact pane through the most
# recently active real client, and a stale notification must be a no-op.
notification_client="$(tmux -S "$sock" list-clients \
  -f '#{?#{m:*control-mode*,#{client_flags}},0,1}' \
  -F '#{client_activity}\t#{client_name}' | sort -n | tail -n 1 | cut -f2-)"
notification_open_works=0
notification_stale_noop=0
if [ -n "$notification_client" ]; then
  "$BIN" notification-open "$sock" "$work" ''
  notification_focus="$(tmux -S "$sock" display-message -p \
    -c "$notification_client" '#{pane_id}')"
  [ "$notification_focus" = "$work" ] && notification_open_works=1

  "$BIN" notification-open "$sock" '%999999' ''
  notification_after_stale="$(tmux -S "$sock" display-message -p \
    -c "$notification_client" '#{pane_id}')"
  [ "$notification_after_stale" = "$notification_focus" ] &&
    notification_stale_noop=1
fi

if [ "$table" = agenmux ] && [ "$initial_focus" = agenmux ] &&
  [ "$inactive_hint_hidden" -eq 1 ] &&
  [ "$chooser_open_unzoomed" -eq 1 ] && [ "$chooser_width" = 30 ] &&
  [ "$ctrl_l_works" -eq 1 ] &&
  [ "$missing_client_noop" -eq 1 ] &&
  [ "$empty_click_works" -eq 1 ] &&
  [ "$stale_click_works" -eq 1 ] &&
  [ "$non_agent_locations_work" -eq 1 ] &&
  [ "$agent_missing_client_noop" -eq 1 ] &&
  [ "$vanished_sidebar_noop" -eq 1 ] &&
  [ "$valid_click_works" -eq 1 ] &&
  [ "$picker_open" -eq 1 ] && [ "$picker_click_works" -eq 1 ] &&
  [ "$picker_reclaimed" -eq 1 ] &&
  [ "$table_after_j" = agenmux ] &&
  printf '%s' "$control_flags" | grep -Fq control-mode &&
  [ "$second" != "$picker_return" ] && [ "$third" = "$picker_return" ] &&
  [ "$wheel_down" != "$third" ] && [ "$wheel_up" = "$third" ] &&
  [ "$wheel_delay_works" -eq 1 ] &&
  [ "$return_table" = agenmux ] && [ "$return_focus" = agenmux ] &&
  [ "$fourth" != "$third" ] && [ "$search_works" -eq 1 ] &&
  [ "$search_accept_works" -eq 1 ] && [ "$search_jk_works" -eq 1 ] &&
  [ "$search_blur_works" -eq 1 ] && [ "$blocked_filter_works" -eq 1 ] &&
  [ "$working_filter_works" -eq 1 ] && [ "$idle_filter_works" -eq 1 ] &&
  [ "$all_filter_works" -eq 1 ] &&
  [ "$exit_table" = root ] && [ "$q_left" -eq 1 ] &&
  [ "$escape_ready" -eq 1 ] && [ "$escape_reset" -eq 1 ] &&
  [ "$escape_left" -eq 1 ] && [ "$close_ready" -eq 1 ] &&
  [ "$q_closed" -eq 1 ] &&
  [ "$notification_open_works" -eq 1 ] &&
  [ "$notification_stale_noop" -eq 1 ]; then
  echo "ok   attached-client-jk-navigation"
else
  echo "FAIL navigation-key-table: table=$table initial-focus=[$initial_focus] initial-hint=[$inactive_hint_hidden/$initial_hint] chooser=[$chooser_open_unzoomed/$chooser_state/$chooser_width] ctrl-l=[$ctrl_l_works/$ctrl_l_table/$ctrl_l_focus] missing-client=[$missing_client_noop/$missing_client_table/$missing_secondary_table/$missing_client_focus] empty-click=[$empty_click_works/$empty_click_table/$secondary_click_table/$empty_click_focus/green=$empty_click_green] stale-click=[$stale_click_works/$stale_click_table/$stale_click_focus] non-agent=[$non_agent_locations_work/$location_table/$location_focus] agent-missing-client=[$agent_missing_client_noop/$agent_missing_primary_table/$agent_missing_secondary_table/$agent_missing_focus] vanished-sidebar=[$vanished_sidebar_noop/$vanished_sidebar_table/$vanished_sidebar_focus] valid-click=[$valid_click_works/$valid_click_table/$valid_click_focus/$valid_target] picker=[$picker_open/click=$picker_click_works/$picker_click_table/$picker_click_focus/rows=$picker_click_rows/frame=$picker_click_first/$picker_reclaimed/$picker_table/$picker_before/$picker_return] after-j=$table_after_j control=[$control/$control_flags] first=[$first] second=[$second] third=[$third] wheel=[$wheel_down/$wheel_up/delay=$wheel_delay_works/target=$wheel_delay_target] return=[$return_table/$return_focus] fourth=[$fourth] search=[$search_works/$search_targets/$search_table/$search_frame/$search_hint/accept=$search_accept_works/$accept_table/$accept_frame/$accept_hint/jk=$search_jk_works/$accepted_cursor/$filtered_cursor/blur=$search_blur_works/$blur_table/$blur_targets] filters=[$blocked_filter_works/$blocked_targets/$blocked_frame/$blocked_hint/$working_filter_works/$working_targets/$working_frame/$idle_filter_works/$idle_targets/$idle_frame/$all_filter_works/$all_targets/$all_frame] q-leave=[$q_left/$exit_table/$exit_focus] escape=[$escape_ready/$escape_reset/$escape_left/$escape_table/$escape_focus/$escape_frame] Q-close=[$close_ready/$q_closed/$close_table] notification-open=[$notification_open_works/$notification_stale_noop/$notification_client]"
  exit 1
fi

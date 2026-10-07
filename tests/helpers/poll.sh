# Polling budgets for the shell harnesses. Source after DIR is set.
#
# `for _ in $(tries 40); do cond && break; sleep 0.05; done` keeps the hand
# counted budget as the floor but never polls fewer than AGENMUX_TEST_TRIES
# times: a loaded CI runner takes several seconds for work the daemon finishes
# locally in under a second, and a loop that exits on success costs a passing
# run nothing extra. Loops that must run to completion (stability checks) do
# not use this helper.
tries() {
  local floor="${AGENMUX_TEST_TRIES:-150}"
  seq 1 $(( $1 > floor ? $1 : floor ))
}

#!/usr/bin/env bash
# The root helpers on the gateway and on the host share a directory with the side they distrust,
# which can put a symlink wherever a name is. They must never follow one: not when they write, not
# when they set a mode, and not while a job runs (security-audit-0.5.2 G-5 and S-12,
# security-audit-0.8.0 INF-3).
#
# Runs as any user, with jq. A canary file stands for the root-owned file an attack would aim at,
# and every test plants symlinks to it at the worst moment. It has to come out untouched.
#
#   bash deploy/tests/helpers.sh
set -uo pipefail

deploy="$(cd "$(dirname "$0")/.." && pwd)"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
failed=0

canary="$work/canary"
new_canary() {
  printf 'canary\n' >"$canary"
  chmod 0600 "$canary"
}
canary_intact() {
  [ "$(cat "$canary")" = canary ] && [ "$(stat -c %a "$canary")" = 600 ]
}
check() {
  local what="$1"
  shift
  if "$@"; then
    printf 'ok      %s\n' "$what"
  else
    printf 'FAILED  %s\n' "$what"
    failed=1
  fi
}

# The other side wins every race: right after each file is written, its name is a symlink to the
# canary. Whatever the helper does with the name afterwards reaches the canary.
losing_every_race() {
  eval "real_$(declare -f safe_write)"
  safe_write() {
    real_safe_write "$@"
    local status=$?
    rm -f -- "$1"
    ln -s "$canary" "$1"
    return $status
  }
}

# Nothing these would reach is on a test machine, and nothing may be.
apt-get() { :; }
do-release-upgrade() { :; }
ufw() { :; }
nft() { :; }
logger() { :; }
fail2ban-client() {
  case "${1:-}" in
    status) [ $# -eq 1 ] && printf '%s\n' '`- Jail list:	sshd' ;;
  esac
  return 0
}

# ── the gateway ───────────────────────────────────────────────────────────────────────────────
(
  export UWUMAIL_GATEWAY_STATE="$work/gateway" UWUMAIL_GATEWAY_PRIVATE="$work/gateway-helper"
  mkdir -p "$UWUMAIL_GATEWAY_STATE" "$work/elsewhere"
  # shellcheck source=deploy/gateway/hardening/helper
  . "$deploy/gateway/hardening/helper"

  # What fail2ban was told lives where the gateway cannot write, and the old list is not written to.
  printf '192.0.2.1 0 192.0.2.1/32\n' >"$state/trusted"
  new_canary
  ln -s "$canary" "$state/ignoring"
  keep_the_trusted_out_of_jails
  check "gateway: the old ignoring list is not written through a symlink" canary_intact
  check "gateway: the ignoring list moved to the helper's own directory" \
    grep -qxF 192.0.2.1/32 "$UWUMAIL_GATEWAY_PRIVATE/ignoring"

  # Carrying off the bans must not move them into a directory the name points to.
  printf '{"action":"ban","ip":"198.51.100.7"}\n' >"$bans_file"
  ln -s "$work/elsewhere" "$bans_file.now"
  carry_out_bans
  check "gateway: waiting bans are not moved through a symlink" test -z "$(ls -A "$work/elsewhere")"

  losing_every_race
  new_canary
  job_state abcd running
  check "gateway: a job's state is not chmodded through a symlink" canary_intact

  new_canary
  cmd_machine
  check "gateway: machine.json is not chmodded through a symlink" canary_intact

  # A job whose log is swapped for a symlink while it runs, and one planted before it starts.
  do_reboot() {
    printf 'first\n' | job_log "$1"
    rm -f -- "$state/job-$1.log"
    ln -s "$canary" "$state/job-$1.log"
    printf 'second\n' | job_log "$1"
  }
  new_canary
  ln -s "$canary" "$state/job-efgh.log"
  run_job efgh reboot ""
  check "gateway: a job's log is never written through a symlink" canary_intact
  exit "$failed"
) || failed=1

# ── the host ──────────────────────────────────────────────────────────────────────────────────
(
  export UWUMAIL_HOST_STATE="$work/host" UWUMAIL_HOST_CONFIG="$work/no-such-host.conf"
  mkdir -p "$UWUMAIL_HOST_STATE/bridge"
  # shellcheck source=deploy/host/helper
  . "$deploy/host/helper"
  os_kind() { printf 'unknown'; }
  look_around() { :; }
  deployed() { :; }

  cmd_machine
  made_as_before() { [ ! -L "$1" ] && [ "$(stat -c %a "$1")" = 640 ]; }
  check "host: machine.json is a regular file with the mode it always had" made_as_before "$machine_file"

  losing_every_race
  new_canary
  job_state abcd running
  check "host: a job's state is not chmodded through a symlink" canary_intact

  new_canary
  cmd_machine
  check "host: machine.json is not chmodded through a symlink" canary_intact

  do_reboot() {
    printf 'first\n' | job_log "$1"
    rm -f -- "$bridge/job-$1.log"
    ln -s "$canary" "$bridge/job-$1.log"
    printf 'second\n' | job_log "$1"
  }
  new_canary
  ln -s "$canary" "$bridge/job-efgh.log"
  run_job efgh reboot
  check "host: a job's log is never written through a symlink" canary_intact
  exit "$failed"
) || failed=1

exit "$failed"

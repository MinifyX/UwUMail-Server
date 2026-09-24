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

  # The VPN's settings come from the side the helper distrusts.
  new_canary
  ln -s "$canary" "$vpn_request"
  refused_symlink() { ! take_vpn_request >/dev/null 2>&1; }
  check "host: vpn.json is not read through a symlink" refused_symlink
  rm -f -- "$vpn_request"
  printf '{"env":{"VPN_TYPE":"wireguard"}}' >"$vpn_request"
  taken() { [ "$(take_vpn_request)" = '{"env":{"VPN_TYPE":"wireguard"}}' ] && [ ! -e "$vpn_request" ]; }
  check "host: vpn.json is read once and removed, it holds keys" taken

  good='{"env":{"VPN_SERVICE_PROVIDER":"private internet access","VPN_TYPE":"wireguard","WIREGUARD_PRIVATE_KEY":"a+b/c="}}'
  lines_ok() {
    [ "$(vpn_env_lines "$good")" = "$(printf "VPN_SERVICE_PROVIDER='private internet access'\nVPN_TYPE='wireguard'\nWIREGUARD_PRIVATE_KEY='a+b/c='")" ]
  }
  check "host: known VPN settings become quoted .env.vpn lines" lines_ok
  refuses() { ! vpn_env_lines "$1" >/dev/null 2>&1; }
  check "host: a variable that is not on the list is refused" refuses '{"env":{"LD_PRELOAD":"/x"}}'
  check "host: a quote cannot end the value" refuses '{"env":{"OPENVPN_PASSWORD":"x'"'"'\nFOO=1"}}'
  check "host: a new line cannot start a variable" refuses '{"env":{"OPENVPN_PASSWORD":"x\nHTTPPROXY_LOG=on"}}'
  check "host: an unknown provider is refused" refuses '{"env":{"VPN_SERVICE_PROVIDER":"evil"}}'
  harmless() { printf '%s\n' "$1" | ovpn_is_harmless; }
  check "host: a plain OpenVPN file passes" harmless $'client\nremote 203.0.113.1 1194\nauth-user-pass\n<ca>\nx\n</ca>'
  not_harmless() { ! printf '%s\n' "$1" | ovpn_is_harmless; }
  check "host: an OpenVPN file that runs a script is refused" not_harmless $'client\n  up /tmp/x.sh'
  check "host: an OpenVPN file that loads a plugin is refused" not_harmless $'client\nplugin /x.so'
  check "host: an OpenVPN file that reads a login from a file is refused" not_harmless $'auth-user-pass /etc/shadow'

  vpn_status() { printf '{"configured":true,"provider":"nordvpn","type":"wireguard","state":"running","health":"healthy","always":true}'; }
  refresh_vpn_status
  vpn_written() { [ "$(jq -r '.vpn.provider + " " + (.verbs | join(","))' "$machine_file")" = "nordvpn os-update,reboot,uwumail-update,helper-update,vpn-apply,vpn-stop,vpn-remove" ] && made_as_before "$machine_file"; }
  check "host: the VPN's state is written into machine.json without asking apt" vpn_written

  COMPOSE_DIR="$work/compose"
  mkdir -p "$COMPOSE_DIR"
  printf 'UWUMAIL_HOSTNAME=mail.example.org\nCOMPOSE_PROFILES=antivirus\n' >"$COMPOSE_DIR/.env"
  set_profile add vpn
  profiles_are() { [ "$(compose_profiles)" = "$1" ]; }
  check "host: the VPN joins the profiles already there" profiles_are antivirus,vpn
  set_profile add vpn
  check "host: adding it twice keeps it once" profiles_are antivirus,vpn
  set_profile remove vpn
  set_profile remove antivirus
  no_profiles_line() { ! grep -q COMPOSE_PROFILES "$COMPOSE_DIR/.env" && grep -q UWUMAIL_HOSTNAME "$COMPOSE_DIR/.env"; }
  check "host: without profiles the line goes, the rest stays" no_profiles_line

  # Removing the VPN takes its settings and its OpenVPN file with it, and nothing else.
  set_profile add vpn
  mkdir -p "$COMPOSE_DIR/vpn"
  printf "VPN_TYPE='wireguard'\n" >"$COMPOSE_DIR/.env.vpn"
  printf 'client\n' >"$COMPOSE_DIR/vpn/custom.ovpn"
  compose() { :; }
  (exec 3>/dev/null && do_vpn_remove wxyz)
  vpn_gone() {
    [ ! -e "$COMPOSE_DIR/.env.vpn" ] && [ ! -e "$COMPOSE_DIR/vpn/custom.ovpn" ] && [ -z "$(compose_profiles)" ] &&
      grep -q UWUMAIL_HOSTNAME "$COMPOSE_DIR/.env" && [ "$(jq -r .state "$bridge/job-wxyz.json")" = "done" ]
  }
  check "host: removing the VPN takes its settings, its file and its profile" vpn_gone

  # A new release is only used when it matches its checksum; where it comes from is fixed here.
  fetch() {
    case "$1" in
      "$releases/update.sh") printf 'echo new\n' >"$2" ;;
      "$releases/update.sh.sha256") printf '%s  update.sh\n' "$(printf 'echo new\n' | sha256sum | cut -d' ' -f1)" >"$2" ;;
      "$releases/bad.sh") printf 'echo evil\n' >"$2" ;;
      "$releases/bad.sh.sha256") printf '%s  bad.sh\n' "$(printf 'echo new\n' | sha256sum | cut -d' ' -f1)" >"$2" ;;
      *) return 1 ;;
    esac
  }
  fetched() { fetch_checked update.sh "$work/fetched.sh" && [ "$(cat "$work/fetched.sh")" = "echo new" ]; }
  check "host: a release file that matches its checksum is taken" fetched
  refused_bad() { ! fetch_checked bad.sh "$work/bad.sh" 2>/dev/null; }
  check "host: a release file that does not match its checksum is refused" refused_bad
  update_from_github() { [ "$releases" = "https://github.com/MinifyX/UwUMail-Server/releases/latest/download" ]; }
  check "host: new versions come from the project's releases, nowhere else" update_from_github
  unset -f compose fetch

  losing_every_race
  new_canary
  job_state abcd running
  check "host: a job's state is not chmodded through a symlink" canary_intact

  new_canary
  cmd_machine
  check "host: machine.json is not chmodded through a symlink" canary_intact

  new_canary
  refresh_vpn_status
  check "host: the VPN's state is not written through a symlink" canary_intact

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

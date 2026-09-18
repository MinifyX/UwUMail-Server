#!/usr/bin/env bash
# Installs, updates and hardens the UwUMail Gateway on a Linux machine with systemd
# (Ubuntu 24.04+ or Debian 12+; built and run on Ubuntu 26.04).
#
#   sudo bash install.sh ./uwumail-gateway
#
# The same command does all of it, every time: a first install, an update to a newer gateway, and
# a check that everything around it is still in place. It is written to be run again and again.
#
# It installs the gateway itself — the system user, the binary, the configuration, the service —
# and then looks after the machine it runs on:
#
#   ufw                  only the ports the gateway needs, and SSH
#   fail2ban             against SSH guessing, and as the place the UwUMail server's bans go
#   unattended-upgrades  security updates install themselves; everything else is reported
#   sysctl, sshd         the settings a machine on the open internet should not be without
#
# One rule runs through all of it: nothing here may ever lock out the UwUMail server. Its tunnel
# comes from a home connection whose address changes nightly, and behind carrier-grade NAT the
# neighbours share it — so today's brute-forcer can be tomorrow's server. Bans are therefore always
# `proto tcp` (the tunnel is UDP and out of reach), fail2ban asks the helper before every ban, and
# a timer frees an address the server turns out to be using.
#
#   --no-harden   only the gateway itself, nothing about the machine
#   --check       change nothing, just say how things stand
set -uo pipefail

config=/etc/uwumail-gateway/gateway.toml
state=/var/lib/uwumail-gateway
helper_dir=/usr/local/lib/uwumail-gateway
# What this script wrote last time, so a file you changed since is never overwritten.
written="$state/written.sha256"

binary=""
harden=true
check=false
for argument in "$@"; do
  case "$argument" in
    --no-harden) harden=false ;;
    --check) check=true ;;
    -h | --help)
      sed -n '2,28p' "$0" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    -*)
      echo "unknown option: $argument" >&2
      exit 2
      ;;
    *) binary="$argument" ;;
  esac
done

if [ "$(id -u)" -ne 0 ]; then
  echo "please run this as root (sudo bash install.sh ...)" >&2
  exit 1
fi
if ! $check && [ -z "$binary" ]; then
  echo "usage: sudo bash install.sh ./uwumail-gateway [--no-harden] [--check]" >&2
  exit 2
fi

here="$(cd "$(dirname "$0")" && pwd)"
hardening="$here/hardening"

# What the report at the end is made of.
notes=()
warnings=()
note() { notes+=("$1"); }
warn() {
  warnings+=("$1")
  printf '  (>_<) %s\n' "$1" >&2
}
step() { printf '  %s\n' "$1"; }

# ── files this script owns ────────────────────────────────────────────────────────────────────
# Put a file in place unless someone changed it since we wrote it. Then the new version lands
# beside it as .new and is reported, the way a package manager treats a config file you edited.
place() {
  local source="$1" target="$2" mode="${3:-0644}"
  local now before
  now=$(sha256sum "$source" | cut -d' ' -f1)
  if [ -f "$target" ]; then
    local current
    current=$(sha256sum "$target" | cut -d' ' -f1)
    if [ "$current" = "$now" ]; then
      # Already what we would write. Noted all the same, so a later version of this file knows it
      # may replace it — a file that matches ours is ours, however it got there.
      remember "$target" "$now"
      return 0
    fi
    before=$(grep -F " $target" "$written" 2>/dev/null | cut -d' ' -f1)
    if [ -n "$before" ] && [ "$current" != "$before" ]; then
      install -m "$mode" "$source" "$target.new"
      warn "you changed $target; the new version is next to it as $target.new"
      return 1
    fi
  fi
  install -D -m "$mode" "$source" "$target"
  remember "$target" "$now"
  return 0
}

remember() {
  local target="$1" sum="$2"
  install -d -m 0755 "$(dirname "$written")"
  touch "$written"
  grep -vF " $1" "$written" >"$written.tmp" 2>/dev/null || true
  printf '%s %s\n' "$sum" "$target" >>"$written.tmp"
  mv -f "$written.tmp" "$written"
}

# ── the gateway itself ────────────────────────────────────────────────────────────────────────
install_gateway() {
  local before=""
  command -v uwumail-gateway >/dev/null 2>&1 && before=$(uwumail-gateway --version 2>/dev/null | awk '{print $2}')

  if ! id uwumail-gateway >/dev/null 2>&1; then
    useradd --system --no-create-home --home-dir /nonexistent --shell /usr/sbin/nologin uwumail-gateway
  fi
  # Installed first: the file may arrive without the executable bit (from a machine that has no
  # such bit at all), and running it from where it landed would fail for that reason alone.
  install -m 0755 "$binary" /usr/local/bin/uwumail-gateway.new
  trap 'rm -f /usr/local/bin/uwumail-gateway.new' EXIT
  # Fails early when the binary does not fit this machine, before it replaces a working one.
  /usr/local/bin/uwumail-gateway.new --version >/dev/null
  mv /usr/local/bin/uwumail-gateway.new /usr/local/bin/uwumail-gateway
  trap - EXIT

  install -d -m 0755 /etc/uwumail-gateway
  if [ ! -f "$config" ]; then
    install -m 0644 "$here/gateway.toml" "$config"
  fi
  /usr/local/bin/uwumail-gateway --config "$config" check-config >/dev/null

  place "$here/uwumail-gateway.service" /etc/systemd/system/uwumail-gateway.service
  systemctl daemon-reload
  systemctl enable uwumail-gateway >/dev/null 2>&1
  systemctl restart uwumail-gateway

  local after
  after=$(uwumail-gateway --version 2>/dev/null | awk '{print $2}')
  if [ -z "$before" ]; then
    note "Gateway|installed|$after"
  elif [ "$before" = "$after" ]; then
    note "Gateway|unchanged|$after"
  else
    note "Gateway|updated|$before → $after"
  fi
}

wait_for_gateway() {
  local _
  for _ in $(seq 1 10); do
    if systemctl is-active --quiet uwumail-gateway && [ -e "$state/identity.json" ]; then
      return 0
    fi
    sleep 1
  done
  systemctl is-active --quiet uwumail-gateway && return 0
  journalctl -u uwumail-gateway --no-pager --lines 30
  echo "the gateway did not start" >&2
  exit 1
}

# ── the machine around it ─────────────────────────────────────────────────────────────────────
# The port SSH really listens on, asked of sshd rather than guessed: locking yourself out of a VPS
# that has no console is the one mistake this script must not make.
ssh_port() {
  local port
  port=$(sshd -T 2>/dev/null | awk '/^port /{ print $2 }' | head -1)
  [[ "$port" =~ ^[0-9]+$ ]] || port=22
  printf '%s' "$port"
}

# The tunnel's UDP port from the configuration, so a gateway that moved it still gets let through.
tunnel_port() {
  local port
  port=$(sed -n 's/^[[:space:]]*tunnel[[:space:]]*=[[:space:]]*"\(.*\)".*/\1/p' "$config" 2>/dev/null | sed 's/.*://')
  [[ "$port" =~ ^[0-9]+$ ]] || port=443
  printf '%s' "$port"
}

set_up_firewall() {
  if ! command -v ufw >/dev/null 2>&1; then
    DEBIAN_FRONTEND=noninteractive apt-get install -y ufw >/dev/null 2>&1 || {
      warn "could not install ufw; the firewall is still up to you"
      note "Firewall|missing|ufw could not be installed"
      return 1
    }
  fi

  local ssh tunnel
  ssh=$(ssh_port)
  tunnel=$(tunnel_port)

  # SSH first and always, before anything is switched on.
  ufw allow "$ssh/tcp" comment "ssh" >/dev/null
  ufw allow 25/tcp comment "mail from other servers" >/dev/null
  ufw allow 80/tcp comment "certificate challenges" >/dev/null
  ufw allow 443/tcp comment "portal, web mail, JMAP" >/dev/null
  ufw allow 465/tcp comment "mail apps over TLS" >/dev/null
  ufw allow 587/tcp comment "mail apps with STARTTLS" >/dev/null
  ufw allow 993/tcp comment "mail apps reading mail" >/dev/null
  ufw allow "$tunnel/udp" comment "the tunnel to the UwUMail server" >/dev/null
  ufw default deny incoming >/dev/null 2>&1
  ufw default allow outgoing >/dev/null 2>&1

  if ! ufw status 2>/dev/null | head -1 | grep -q active; then
    # Checked rather than trusted: if the rule is not there, switching the firewall on cuts the
    # cable this script is running over.
    if ! ufw status 2>/dev/null | grep -q "^$ssh/tcp"; then
      warn "ufw has no rule for SSH on port $ssh; leaving the firewall off"
      note "Firewall|off|no SSH rule, not switched on"
      return 1
    fi
    ufw --force enable >/dev/null
  fi

  note "Firewall|ufw|$ssh 25 80 443 465 587 993/tcp · $tunnel/udp"
  retire_old_nftables "$tunnel"
}

# The gateway's documentation used to hand out an nftables rule set to write by hand. With ufw in
# charge, two firewalls would each have their own idea of what is open. Ours is recognised by the
# comment it was published with and stood down; anything else is left alone and only reported.
retire_old_nftables() {
  systemctl is-enabled nftables >/dev/null 2>&1 || return 0
  local rules=/etc/nftables.conf
  [ -f "$rules" ] || return 0
  # Our own rule set went out in more than one wording: the one from docs/gateway.md, which names
  # the tunnel on the UDP line, and the one machines ended up with, which says whose it is in the
  # first line. Both are ours; anything that says neither is someone else's and is left alone.
  if grep -qE "UwUMail Gateway|tunnel to the UwUMail server" "$rules" 2>/dev/null; then
    cp -a "$rules" "$rules.before-uwumail-ufw"
    systemctl disable --now nftables >/dev/null 2>&1
    # ufw brings its own table; the handwritten one goes with the service that loaded it.
    nft delete table inet filter >/dev/null 2>&1
    note "nftables|stood down|ufw took over, old rules kept as $rules.before-uwumail-ufw"
  else
    warn "nftables is enabled next to ufw with rules that are not the gateway's; check $rules"
    note "nftables|still on|rules that are not ours, left alone"
  fi
}

set_up_fail2ban() {
  if ! command -v fail2ban-client >/dev/null 2>&1; then
    DEBIAN_FRONTEND=noninteractive apt-get install -y fail2ban >/dev/null 2>&1 || {
      warn "could not install fail2ban"
      note "fail2ban|missing|could not be installed"
      return 1
    }
  fi
  # jq reads what the gateway writes; without it the helper cannot do its work.
  command -v jq >/dev/null 2>&1 || DEBIAN_FRONTEND=noninteractive apt-get install -y jq >/dev/null 2>&1

  place "$hardening/jail.local" /etc/fail2ban/jail.d/uwumail-gateway.local
  place "$hardening/filter-uwumail-server.conf" /etc/fail2ban/filter.d/uwumail-server.conf

  systemctl enable fail2ban >/dev/null 2>&1
  if ! systemctl restart fail2ban; then
    warn "fail2ban did not start; run: fail2ban-client -d"
    note "fail2ban|broken|did not start"
    return 1
  fi

  local jails=""
  local _
  for _ in $(seq 1 10); do
    jails=$(fail2ban-client status 2>/dev/null | sed -n 's/.*Jail list:[[:space:]]*//p')
    [ -n "$jails" ] && break
    sleep 1
  done
  if [ -z "$jails" ]; then
    warn "fail2ban is running but has no jails; run: fail2ban-client -d"
    note "fail2ban|no jails|check the configuration"
    return 1
  fi
  note "fail2ban|watching|$jails"
}

install_helper() {
  install -D -m 0755 "$hardening/helper" "$helper_dir/helper"
  remember "$helper_dir/helper" "$(sha256sum "$hardening/helper" | cut -d' ' -f1)"

  for unit in "$hardening"/units/*; do
    place "$unit" "/etc/systemd/system/$(basename "$unit")"
  done
  systemctl daemon-reload
  systemctl enable --now uwumail-gateway-helper.path uwumail-gateway-helper.timer >/dev/null 2>&1
  systemctl enable uwumail-gateway-machine.timer >/dev/null 2>&1
  systemctl start uwumail-gateway-machine.timer >/dev/null 2>&1

  # The login message, where Ubuntu shows it.
  if [ -d /etc/update-motd.d ]; then
    place "$hardening/motd" /etc/update-motd.d/98-uwumail-gateway 0755
  fi
}

set_up_updates() {
  if ! dpkg -s unattended-upgrades >/dev/null 2>&1; then
    DEBIAN_FRONTEND=noninteractive apt-get install -y unattended-upgrades >/dev/null 2>&1 || {
      warn "could not install unattended-upgrades; security updates will not install themselves"
      note "Updates|manual|unattended-upgrades is missing"
      return 1
    }
  fi
  place "$hardening/unattended-upgrades.conf" /etc/apt/apt.conf.d/52uwumail-gateway-upgrades
  systemctl enable --now unattended-upgrades >/dev/null 2>&1
}

set_up_kernel() {
  place "$hardening/sysctl.conf" /etc/sysctl.d/80-uwumail-gateway.conf &&
    sysctl --system >/dev/null 2>&1
}

set_up_ssh() {
  [ -d /etc/ssh/sshd_config.d ] || return 0
  local target=/etc/ssh/sshd_config.d/60-uwumail-gateway.conf
  if place "$hardening/sshd.conf" "$target"; then
    # Checked before it is loaded: a config file sshd refuses would leave this machine unreachable.
    if sshd -t 2>/dev/null; then
      systemctl reload ssh >/dev/null 2>&1 || systemctl reload sshd >/dev/null 2>&1
    else
      rm -f "$target"
      warn "the SSH settings were refused by sshd and were taken back out"
    fi
  fi

  # Not changed, only said: turning password logins off is the biggest single thing you can do for
  # this machine, and doing it behind your back could lock you out of a VPS with no console.
  if sshd -T 2>/dev/null | grep -q "^passwordauthentication yes"; then
    note "SSH|passwords on|see the advice below"
  else
    note "SSH|keys only|password logins are off"
  fi
}

# ── the report ────────────────────────────────────────────────────────────────────────────────
report() {
  local version
  version=$(uwumail-gateway --version 2>/dev/null | awk '{ print $2 }')
  printf '\n  UwUMail Gateway %s (=^-w-^=)\n\n' "${version:-?}"

  local line what how detail
  for line in "${notes[@]}"; do
    IFS='|' read -r what how detail <<<"$line"
    printf '  %-16s %-14s %s\n' "$what" "$how" "$detail"
  done

  # What the gateway sees of the server, and of the machine.
  local trusted=""
  [ -f "$state/trusted" ] && trusted=$(awk '{ print $1 }' "$state/trusted" | head -3 | paste -sd' ')
  if [ -n "$trusted" ]; then
    printf '  %-16s %-14s %s\n' "UwUMail server" "protected" "$trusted"
  else
    printf '  %-16s %-14s %s\n' "UwUMail server" "waiting" "no tunnel yet; it is protected once it connects"
  fi
  printf '\n'

  if $harden && [ -x "$helper_dir/helper" ]; then
    "$helper_dir/helper" report || true
  fi

  if [ ${#warnings[@]} -gt 0 ]; then
    printf '  Worth a look:\n'
    local warning
    for warning in "${warnings[@]}"; do
      printf '    · %s\n' "$warning"
    done
    printf '\n'
  fi

  if printf '%s\n' "${notes[@]}" | grep -q "^SSH|passwords on"; then
    cat <<'ADVICE'
  Password logins over SSH are still on. fail2ban slows guessing down; keys end it. Once your key
  works, this turns them off:

    echo 'PasswordAuthentication no' > /etc/ssh/sshd_config.d/61-no-passwords.conf
    sshd -t && systemctl reload ssh

  Keep the session you have open until a second one works.

ADVICE
  fi
}

# ── what actually runs ────────────────────────────────────────────────────────────────────────
if $check; then
  command -v uwumail-gateway >/dev/null 2>&1 || {
    echo "no gateway is installed here" >&2
    exit 1
  }
  version=$(uwumail-gateway --version 2>/dev/null | awk '{ print $2 }')
  if systemctl is-active --quiet uwumail-gateway; then
    note "Gateway|running|$version"
  else
    note "Gateway|stopped|$version"
  fi
  if ufw status 2>/dev/null | head -1 | grep -q active; then
    note "Firewall|ufw|active"
  else
    note "Firewall|ufw|not active"
  fi
  if command -v fail2ban-client >/dev/null 2>&1 && fail2ban-client ping >/dev/null 2>&1; then
    note "fail2ban|watching|$(fail2ban-client status 2>/dev/null | sed -n 's/.*Jail list:[[:space:]]*//p')"
  else
    note "fail2ban|not running|"
  fi
  [ -x "$helper_dir/helper" ] && "$helper_dir/helper" machine >/dev/null 2>&1
  report
  exit 0
fi

step "installing the gateway"
install_gateway
wait_for_gateway

if $harden; then
  if [ ! -d "$hardening" ]; then
    warn "this package has no hardening/ folder; only the gateway was installed"
  else
    step "looking after the machine"
    install_helper
    set_up_firewall
    set_up_fail2ban
    set_up_updates
    set_up_kernel
    set_up_ssh
    # Writes the first report, so the summary below has something to say.
    "$helper_dir/helper" machine >/dev/null 2>&1 || warn "could not look at the machine's updates"
    "$helper_dir/helper" tick >/dev/null 2>&1
  fi
fi

report
# Last, so it is the thing left on screen when a gateway is waiting to be paired.
sleep 1
/usr/local/bin/uwumail-gateway --config "$config" code || true

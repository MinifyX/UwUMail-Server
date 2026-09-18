#!/usr/bin/env bash
# Lets the UwUMail portal look after the machine it runs on: what the system has waiting, and the
# button that installs it. Debian 12+ or Ubuntu 24.04+ with systemd.
#
#   sudo bash install.sh                 # the compose file is in this directory, or /opt/uwumail
#   sudo bash install.sh --dir /srv/mail # it is somewhere else
#   sudo bash install.sh --check         # change nothing, just say how things stand
#   sudo bash install.sh --remove        # take it all back out
#
# Why this exists: the server's container is distroless, read-only, unprivileged and has every
# capability dropped but the one it needs for the low mail ports. It cannot ask apt what is waiting
# and it cannot recreate itself, and that is on purpose -- it is what makes a break-in worth little.
# This installs a small helper beside it that can, and a directory the two share.
#
# What the container may ask for is a verb from a fixed list -- install the system's updates,
# restart the machine, pull a new UwUMail -- plus, for the last one, a version number that has to
# look like one. Never a command, never a path, never an address. The docker socket stays where it
# is: handing that to a container is handing it the machine.
set -uo pipefail

state=/var/lib/uwumail-host
bridge="$state/bridge"
helper_dir=/usr/local/lib/uwumail-host
config_dir=/etc/uwumail-host
config="$config_dir/host.conf"
written="$state/written.sha256"
override_name=compose.override.uwumail-host.yaml

here="$(cd "$(dirname "$0")" && pwd)"
compose_dir=""
check=false
remove=false
service=uwumail

while [ $# -gt 0 ]; do
  case "$1" in
    --dir)
      compose_dir="${2:-}"
      shift 2
      ;;
    --service)
      service="${2:-uwumail}"
      shift 2
      ;;
    --check)
      check=true
      shift
      ;;
    --remove)
      remove=true
      shift
      ;;
    -h | --help)
      sed -n '2,26p' "$0" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *)
      echo "unknown option: $1" >&2
      exit 2
      ;;
  esac
done

if [ "$(id -u)" -ne 0 ]; then
  echo "please run this as root (sudo bash install.sh ...)" >&2
  exit 1
fi

notes=()
warnings=()
note() { notes+=("$1"); }
warn() {
  warnings+=("$1")
  printf '  (>_<) %s\n' "$1" >&2
}
step() { printf '  %s\n' "$1"; }

# Put a file in place unless someone changed it since we wrote it; then the new one lands beside it
# as .new and is reported, the way a package manager treats a config file you edited.
place() {
  local source="$1" target="$2" mode="${3:-0644}" now before current
  now=$(sha256sum "$source" | cut -d' ' -f1)
  if [ -f "$target" ]; then
    current=$(sha256sum "$target" | cut -d' ' -f1)
    if [ "$current" = "$now" ]; then
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
}

remember() {
  install -d -m 0755 "$(dirname "$written")"
  touch "$written"
  grep -vF " $1" "$written" >"$written.tmp" 2>/dev/null || true
  printf '%s %s\n' "$2" "$1" >>"$written.tmp"
  mv -f "$written.tmp" "$written"
}

# ── where the server lives ────────────────────────────────────────────────────────────────────
find_compose() {
  local candidate
  for candidate in "$compose_dir" "$here" /opt/uwumail; do
    [ -z "$candidate" ] && continue
    if [ -f "$candidate/compose.yaml" ] || [ -f "$candidate/docker-compose.yaml" ]; then
      printf '%s' "$(cd "$candidate" && pwd)"
      return 0
    fi
  done
  return 1
}

# ── taking it back out ────────────────────────────────────────────────────────────────────────
if $remove; then
  step "removing the helper"
  systemctl disable --now uwumail-host-task.path uwumail-host-machine.timer >/dev/null 2>&1
  rm -f /etc/systemd/system/uwumail-host-*.service /etc/systemd/system/uwumail-host-*.timer \
    /etc/systemd/system/uwumail-host-*.path
  systemctl daemon-reload
  rm -rf "$helper_dir" "$config_dir"
  found=$(find_compose) && [ -f "$found/$override_name" ] && rm -f "$found/$override_name" &&
    step "removed $found/$override_name; run: docker compose up -d"
  rm -rf "$state"
  echo "the portal can no longer look after this machine (=^･ω･^=)"
  exit 0
fi

if ! compose_dir=$(find_compose); then
  echo "no compose.yaml found. Pass --dir with the directory UwUMail runs from." >&2
  exit 2
fi

# ── what is already there ─────────────────────────────────────────────────────────────────────
if $check; then
  if [ -x "$helper_dir/helper" ]; then
    note "Helper|installed|$helper_dir/helper"
  else
    note "Helper|missing|"
  fi
  for unit in uwumail-host-machine.timer uwumail-host-task.path; do
    if systemctl is-enabled --quiet "$unit" 2>/dev/null; then
      note "$unit|on|"
    else
      note "$unit|off|"
    fi
  done
  if grep -q "$bridge" "$compose_dir/$override_name" 2>/dev/null; then
    note "compose|wired up|$compose_dir/$override_name"
  else
    note "compose|not wired up|$compose_dir"
  fi
  printf '\n'
  for line in "${notes[@]}"; do
    IFS='|' read -r what how detail <<<"$line"
    printf '  %-28s %-14s %s\n' "$what" "$how" "$detail"
  done
  [ -x "$helper_dir/helper" ] && { printf '\n'; "$helper_dir/helper" report; }
  exit 0
fi

# ── installing ────────────────────────────────────────────────────────────────────────────────
if ! command -v docker >/dev/null 2>&1; then
  warn "docker was not found; the portal will be able to install system updates but not update UwUMail"
fi
if ! command -v jq >/dev/null 2>&1; then
  step "installing jq, which the helper reads the portal's jobs with"
  DEBIAN_FRONTEND=noninteractive apt-get update -qq >/dev/null 2>&1
  DEBIAN_FRONTEND=noninteractive apt-get install -y -qq jq >/dev/null 2>&1 ||
    warn "jq could not be installed; the helper needs it"
fi

step "installing the helper"
install -d -m 0755 "$helper_dir"
place "$here/helper" "$helper_dir/helper" 0755

# The directory the two share. The container runs as 10001, so that is who may read and write here
# besides root. Nothing else on the machine can look in.
step "making the shared directory"
install -d -m 0755 "$state"
install -d -m 0770 -o root -g 10001 "$bridge" 2>/dev/null || install -d -m 0777 "$bridge"

install -d -m 0755 "$config_dir"
if [ ! -f "$config" ]; then
  {
    printf '# Where UwUMail runs, for the helper.\n'
    printf 'COMPOSE_DIR=%s\n' "$compose_dir"
    printf 'SERVICE=%s\n' "$service"
  } >"$config"
  chmod 0644 "$config"
fi

step "installing the units"
for unit in "$here"/units/*; do
  place "$unit" "/etc/systemd/system/$(basename "$unit")" 0644
done
systemctl daemon-reload
systemctl enable --now uwumail-host-machine.timer >/dev/null 2>&1
systemctl enable --now uwumail-host-task.path >/dev/null 2>&1

# ── letting the container see it ──────────────────────────────────────────────────────────────
# An override file of our own, so an override the admin wrote stays theirs.
step "wiring the directory into the container"
override="$compose_dir/$override_name"
tmp=$(mktemp)
{
  printf '# Written by deploy/host/install.sh. It lets the portal see what this machine needs.\n'
  printf '# Remove it, or run install.sh --remove, and the portal goes back to only showing commands.\n'
  printf 'services:\n'
  printf '  %s:\n' "$service"
  printf '    volumes:\n'
  printf '      - %s:/host\n' "$bridge"
} >"$tmp"
place "$tmp" "$override" 0644
rm -f "$tmp"

if [ -f "$compose_dir/compose.override.yaml" ] || [ -f "$compose_dir/docker-compose.override.yaml" ]; then
  warn "there is an override file of your own here; docker only reads one unless you name both. Start with: docker compose -f compose.yaml -f compose.override.yaml -f $override_name up -d"
fi

step "writing down what this machine looks like"
"$helper_dir/helper" machine >/dev/null 2>&1 || warn "could not look at the machine"

printf '\n'
"$helper_dir/helper" report
printf '\n'
if [ ${#warnings[@]} -gt 0 ]; then
  printf 'Worth a look:\n'
  for line in "${warnings[@]}"; do printf '  - %s\n' "$line"; done
  printf '\n'
fi
cat <<INFO
Almost there. The container has to be recreated once to see the new directory:

  cd $compose_dir && docker compose up -d

After that the portal shows this machine under Server, and can install what it needs.
INFO

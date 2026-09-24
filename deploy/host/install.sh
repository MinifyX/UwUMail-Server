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
# restart the machine, start or stop the VPN for pictures -- and, for the VPN, gluetun's settings,
# which the helper checks against a fixed list of variables. Never a command, never a path, never
# an address. The docker socket stays where it
# is: handing that to a container is handing it the machine.
set -uo pipefail

state=/var/lib/uwumail-host
bridge="$state/bridge"
helper_dir=/usr/local/lib/uwumail-host
config_dir=/etc/uwumail-host
config="$config_dir/host.conf"
written="$state/written.sha256"
# The one name docker compose reads without being told to.
override_name=compose.override.yaml

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
  # Only what we wrote. An override file somebody edited afterwards is theirs, and taking our
  # helper away is no reason to take their file with it.
  if found=$(find_compose); then
    for leftover in "$found/$override_name" "$found/compose.override.uwumail-host.yaml"; do
      [ -f "$leftover" ] || continue
      if grep -qF " $leftover" "$written" 2>/dev/null &&
        [ "$(sha256sum "$leftover" | cut -d' ' -f1)" = "$(grep -F " $leftover" "$written" | cut -d' ' -f1)" ]; then
        rm -f "$leftover"
        step "removed $leftover; run: cd $found && docker compose up -d"
      else
        step "left $leftover alone, you changed it; remove the /host line yourself"
      fi
    done
  fi
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
  if [ -d "$bridge" ]; then
    note "shared directory|$(stat -c '%a %u:%g' "$bridge" 2>/dev/null)|should be 770 0:10001"
  else
    note "shared directory|missing|$bridge"
  fi
  # The one that matters: a file only helps when the container can actually see it.
  if docker inspect "$service" --format '{{range .Mounts}}{{.Destination}} {{end}}' 2>/dev/null | grep -qw /host; then
    note "container|sees it|/host"
  else
    note "container|does NOT see it|run: cd $compose_dir && docker compose up -d"
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

# The directory the two share. The container runs as 10001:10001, so that is who may read and write
# here besides root; nothing else on the machine can look in. The group need not exist here as a
# name -- the number is what the kernel compares -- so this chowns by number and says so if even
# that fails, rather than falling back to a directory the whole machine can write to.
step "making the shared directory"
install -d -m 0755 "$state"
install -d -m 0770 "$bridge"
if chown 0:10001 "$bridge" 2>/dev/null; then
  chmod 0770 "$bridge"
else
  warn "could not give $bridge to the container's user (10001); the portal will not be able to write there"
fi

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
# It has to be called compose.override.yaml: that is the one name docker reads by itself. A file
# under any other name would sit there and never be looked at -- which is exactly what happened the
# first time this was tried.
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

# An earlier version of this script wrote a file under a name docker never reads by itself. If one
# is still lying there from then, it goes: it did nothing and only confuses the next person.
stale="$compose_dir/compose.override.uwumail-host.yaml"
if [ -f "$stale" ] && grep -qF " $stale" "$written" 2>/dev/null; then
  rm -f "$stale"
  grep -vF " $stale" "$written" >"$written.tmp" 2>/dev/null && mv -f "$written.tmp" "$written"
  step "removed $stale, which docker never read"
fi

theirs=false
if [ -f "$override" ] && ! grep -qF " $override" "$written" 2>/dev/null; then
  theirs=true
fi
if $theirs; then
  # Their file, their business. Say what to add and leave it alone.
  warn "$override is yours, so it was left as it is. Add these lines to it and run docker compose up -d:"
  printf '\n      volumes:\n        - %s:/host\n\n' "$bridge" >&2
else
  place "$tmp" "$override" 0644
fi
rm -f "$tmp"

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
# A container made before the directory was wired in does not see it until it is made again. One
# that does see it -- the helper updating itself from the portal, say -- needs nothing more.
if docker inspect "$service" --format '{{range .Mounts}}{{.Destination}} {{end}}' 2>/dev/null | grep -qw /host; then
  echo "The portal shows this machine under Server, and can install what it needs (=^･ω･^=)"
else
  cat <<INFO
Almost there. The container has to be recreated once to see the new directory:

  cd $compose_dir && docker compose up -d

After that the portal shows this machine under Server, and can install what it needs.
INFO
fi

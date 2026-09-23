#!/usr/bin/env bash
# Brings UwUMail Server to the newest version, where it runs.
#
#   cd /opt/uwumail && sudo bash update.sh
#
# It updates itself first, then looks at compose.yaml, makes a backup, fetches the new images and
# watches the server come back. If it does not come back, the old version does.
#
#   --dir DIR          where UwUMail lives (default: this directory, then /opt/uwumail)
#   --version TAG      switch to another tag: latest, beta, edge, or an exact version like 0.4.0
#   --no-backup        do not back up first
#   --no-antivirus     do not offer the virus scanner
#   --force            take the new compose.yaml even when this one was changed by hand
#   --keep-compose     leave compose.yaml alone, now and from now on
#   --no-self-update   do not fetch a newer update.sh first
#   --yes              ask nothing; every answer takes its default
#   --help
#
# What it never touches: your data, and a compose.yaml you edited yourself unless you say --force.
# Of the .env it only ever changes what it says it changes. The whole story: docs/deployment.md.
set -uo pipefail

repo=MinifyX/UwUMail-Server
releases="https://github.com/$repo/releases"
image=ghcr.io/minifyx/uwumail-server
service=uwumail
here="$(cd "$(dirname "$0")" && pwd)"

# What we were called with, for the copy that takes over after a self-update.
called_with=("$@")

dir=""
version=""
backup=true
antivirus=true
force=false
keep_compose=false
self_update=true
ask=true

die() {
  printf '\n  (>_<) %s\n' "$1" >&2
  exit 1
}
step() { printf '  %s\n' "$1"; }
warn() { printf '  (>_<) %s\n' "$1" >&2; }

while [ $# -gt 0 ]; do
  case "$1" in
    --dir) dir="${2:?--dir needs a directory}"; shift 2 ;;
    --version) version="${2:?--version needs a tag}"; shift 2 ;;
    --no-backup) backup=false; shift ;;
    --no-antivirus) antivirus=false; shift ;;
    --force) force=true; shift ;;
    --keep-compose) keep_compose=true; shift ;;
    --no-self-update) self_update=false; shift ;;
    --yes | -y) ask=false; shift ;;
    -h | --help)
      sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *) die "unknown option: $1" ;;
  esac
done

[ "$(id -u)" -eq 0 ] || die "please run this as root: sudo bash update.sh"
command -v docker >/dev/null 2>&1 || die "Docker is missing"
docker compose version >/dev/null 2>&1 || die "Docker Compose v2 is missing"

# ── where UwUMail lives ───────────────────────────────────────────────────────────────────────
if [ -z "$dir" ]; then
  for candidate in "$PWD" "$here" /opt/uwumail; do
    if [ -f "$candidate/compose.yaml" ] && [ -f "$candidate/.env" ]; then
      dir="$candidate"
      break
    fi
  done
fi
[ -n "$dir" ] || die "no compose.yaml with an .env next to it. Pass --dir with the directory UwUMail runs from."
dir="$(cd "$dir" && pwd)"
cd "$dir" || die "cannot go into $dir"
state="$dir/.uwumail-update"

# ── small helpers ─────────────────────────────────────────────────────────────────────────────
yesno() {
  local prompt="$1" fallback="$2" answer=""
  if ! $ask || ! have_tty; then
    [ "$fallback" = y ] && return 0 || return 1
  fi
  read -r -p "  $prompt [$([ "$fallback" = y ] && echo 'Y/n' || echo 'y/N')]: " answer </dev/tty
  answer="${answer:-$fallback}"
  case "$answer" in [yYjJ]*) return 0 ;; *) return 1 ;; esac
}

fetch() {
  local url="$1" target="$2"
  if command -v curl >/dev/null 2>&1; then
    curl -fsL --proto '=https' --tlsv1.2 -o "$target" "$url"
  elif command -v wget >/dev/null 2>&1; then
    wget -q --https-only -O "$target" "$url"
  else
    return 1
  fi
}

# A release file and the checksum next to it; nothing is used unless the two agree.
fetch_checked() {
  local name="$1" target="$2" base="${3:-$releases/latest/download}" sums want have
  fetch "$base/$name" "$target" || return 1
  sums="$target.sha256"
  fetch "$base/$name.sha256" "$sums" || return 1
  want=$(cut -d' ' -f1 <"$sums")
  have=$(sha256sum "$target" | cut -d' ' -f1)
  rm -f "$sums"
  [ -n "$want" ] && [ "$want" = "$have" ]
}

# Whether there is a terminal to ask on: after an exec there may be none, whatever /dev/tty
# looks like in the file system.
have_tty() { { : </dev/tty; } 2>/dev/null; }

hash_of() { sha256sum "$1" | cut -d' ' -f1; }
looks_like_version() { case "${1:-}" in [0-9]*) return 0 ;; *) return 1 ;; esac; }

# A value on its way into the .env has to be one line of plain characters, wherever it came from:
# a flag, or a compose.yaml somebody wrote by hand.
plain_value() { case "${1:-}" in "" | *[!a-zA-Z0-9.:_/+-]*) return 1 ;; *) return 0 ;; esac; }

# Reads one value out of the .env, without sourcing a file we did not write.
env_value() {
  local line
  line=$(grep -m1 "^$1=" "$dir/.env" 2>/dev/null) || return 1
  printf '%s' "${line#*=}"
}

# Writes one line of the .env, whether it is in there already, commented out, or missing. The file
# keeps its rights: it holds a gateway code and belongs to root alone.
set_env() {
  local key="$1" value="$2" file="$dir/.env" line found=false tmp="$dir/.env.tmp"
  plain_value "$value" || die "$key would become something odd, so nothing was written: $value"
  # The copy holds everything the .env holds, the gateway code included, so it is made with the
  # rights of the file it replaces rather than whatever the umask happens to be. A run that dies
  # in between leaves nothing readable behind either, which is what the trap is for.
  install -m 0600 /dev/null "$tmp"
  trap 'rm -f "$dir/.env.tmp"' EXIT INT TERM
  while IFS= read -r line || [ -n "$line" ]; do
    case "$line" in
      "$key="* | "#$key="*)
        if $found; then continue; fi
        printf '%s=%s\n' "$key" "$value" >>"$tmp"
        found=true
        ;;
      *) printf '%s\n' "$line" >>"$tmp" ;;
    esac
  done <"$file"
  $found || printf '%s=%s\n' "$key" "$value" >>"$tmp"
  cat "$tmp" >"$file"
  rm -f "$tmp"
}

printf '\n  UwUMail Server, update\n  ~~~~~~~~~~~~~~~~~~~~~~\n\n'

# ── a newer updater first ─────────────────────────────────────────────────────────────────────
# Reading the script and replacing the file are two different things: bash holds the old file
# open, so swapping it out under us is safe, and the new one takes over with exec right after.
if $self_update; then
  fresh=$(mktemp)
  if fetch_checked update.sh "$fresh" && [ "$(hash_of "$fresh")" != "$(hash_of "$0")" ]; then
    step "there is a newer update.sh; taking that one"
    install -m 0755 "$fresh" "$dir/update.sh"
    rm -f "$fresh"
    exec bash "$dir/update.sh" --no-self-update --dir "$dir" "${called_with[@]}"
  fi
  rm -f "$fresh"
fi

# ── what is running now ───────────────────────────────────────────────────────────────────────
running_version=$(docker inspect "$service" \
  --format '{{index .Config.Labels "org.opencontainers.image.version"}}' 2>/dev/null)
running_image=$(docker inspect "$service" --format '{{.Image}}' 2>/dev/null)
[ -n "$running_version" ] && step "running now: $running_version"

# ── the compose file ──────────────────────────────────────────────────────────────────────────
# What a hand-edited compose.yaml can say that the .env says just as well. Anything else stops us.
# shellcheck disable=SC2016  # the ${...} here are strings to compare against, not expansions
lift_into_env() {
  local file="$1" value
  value=$(grep -oE "image: $image:[^ ]+" "$file" | head -1 | cut -d: -f3)
  case "$value" in
    "" | '${UWUMAIL_VERSION'*) ;;
    *)
      step "moving the image tag $value into .env"
      set_env UWUMAIL_VERSION "$value"
      ;;
  esac
  lift_port "$file" 25 UWUMAIL_SMTP_BIND
  lift_port "$file" 80 UWUMAIL_HTTP_BIND
  lift_port "$file" 443 UWUMAIL_HTTPS_BIND
  lift_port "$file" 465 UWUMAIL_SUBMISSIONS_BIND
  lift_port "$file" 587 UWUMAIL_SUBMISSION_BIND
  lift_port "$file" 993 UWUMAIL_IMAPS_BIND
  lift_port "$file" 4190 UWUMAIL_MANAGESIEVE_BIND
  # A clamav service without its profile means: this machine wants the scanner at every start.
  if grep -q '^  clamav:' "$file" && ! grep -q '^      - antivirus' "$file"; then
    step "moving the virus scanner into .env"
    set_env COMPOSE_PROFILES antivirus
  fi
}

# One published port of that file into the .env, when somebody wrote a number where the stock
# file has the variable. The container side never moves, so it is what we look the port up by.
lift_port() {
  local file="$1" port="$2" key="$3" value
  value=$(grep -oE "^ *- \"[^\"]+:$port\"" "$file" | head -1 | sed -E "s/.*\"(.*):$port\"/\1/")
  case "$value" in
    "" | "\${$key"*) ;;
    *)
      step "moving port $port, which is at $value here, into .env"
      set_env "$key" "$value"
      ;;
  esac
}

# The same file with every value we understand written the way the stock file writes it. What is
# left over after that is a change nobody can translate, and then we stop. The last line takes
# the width of the comment column out of it as well: a port somebody wrote by hand shifts the
# comment behind it, and that is not a change worth stopping for.
# shellcheck disable=SC2016  # sed writes the ${...} out literally, that is the point
normalized() {
  sed -E \
    -e "s#(image: $image:).*#\1\\\$\{UWUMAIL_VERSION:-latest\}#" \
    -e 's#^( *- ")[^"]+(:25".*)#\1${UWUMAIL_SMTP_BIND:-25}\2#' \
    -e 's#^( *- ")[^"]+(:80".*)#\1${UWUMAIL_HTTP_BIND:-80}\2#' \
    -e 's#^( *- ")[^"]+(:443".*)#\1${UWUMAIL_HTTPS_BIND:-443}\2#' \
    -e 's#^( *- ")[^"]+(:465".*)#\1${UWUMAIL_SUBMISSIONS_BIND:-465}\2#' \
    -e 's#^( *- ")[^"]+(:587".*)#\1${UWUMAIL_SUBMISSION_BIND:-587}\2#' \
    -e 's#^( *- ")[^"]+(:993".*)#\1${UWUMAIL_IMAPS_BIND:-993}\2#' \
    -e 's#^( *- ")[^"]+(:4190".*)#\1${UWUMAIL_MANAGESIEVE_BIND:-4190}\2#' \
    -e 's|[[:space:]]+#| #|' \
    "$1" | sha256sum | cut -d' ' -f1
}

# True when this compose.yaml is one we put here: either we wrote down its checksum, or it is
# exactly the file that came with the version running now.
compose_is_ours() {
  local mine
  mine=$(hash_of "$dir/compose.yaml")
  [ -f "$state" ] && grep -qxF "compose $mine" "$state" && return 0
  looks_like_version "$running_version" || return 1
  local theirs
  theirs=$(mktemp)
  if fetch_checked compose.yaml "$theirs" "$releases/download/v$running_version" &&
    [ "$(hash_of "$theirs")" = "$mine" ]; then
    rm -f "$theirs"
    return 0
  fi
  rm -f "$theirs"
  return 1
}

if ! $keep_compose && [ -f "$state" ] && grep -qx "compose keep" "$state"; then
  keep_compose=true
  step "compose.yaml is yours; leaving it alone"
fi

stock=$(mktemp)
compose_known=false
if $keep_compose; then
  :
elif fetch_checked compose.yaml "$stock"; then
  if [ "$(hash_of "$stock")" = "$(hash_of "$dir/compose.yaml")" ]; then
    step "compose.yaml is the current one"
    compose_known=true
  elif compose_is_ours; then
    install -m 0644 "$stock" "$dir/compose.yaml"
    step "compose.yaml brought up to date"
    compose_known=true
  elif [ "$(normalized "$dir/compose.yaml")" = "$(normalized "$stock")" ]; then
    lift_into_env "$dir/compose.yaml"
    install -m 0644 "$stock" "$dir/compose.yaml"
    step "compose.yaml brought up to date, your changes live in .env now"
    compose_known=true
  elif $force; then
    cp -p "$dir/compose.yaml" "$dir/compose.yaml.bak"
    lift_into_env "$dir/compose.yaml.bak"
    install -m 0644 "$stock" "$dir/compose.yaml"
    warn "compose.yaml replaced as asked; the old one is next to it as compose.yaml.bak"
    compose_known=true
  else
    printf '\n'
    warn "your compose.yaml is not the one this version ships, and not everything in it fits"
    warn "into .env. Nothing was changed. This is what differs:"
    printf '\n'
    diff -u "$dir/compose.yaml" "$stock" | sed -n '3,40p'
    cat <<-CHOICE

	  Your file on purpose, say so once and it stops asking:  sudo bash update.sh --keep-compose
	  Take the new one, yours stays as compose.yaml.bak:      sudo bash update.sh --force

	CHOICE
    rm -f "$stock"
    exit 1
  fi
else
  warn "could not fetch the current compose.yaml; going on with the one that is here"
fi
rm -f "$stock"
compose_hash=$(hash_of "$dir/compose.yaml")

# ── the ManageSieve port ──────────────────────────────────────────────────────────────────────
# 4190 came later than the other ports. Where something else on this machine holds it already,
# UwUMail takes the next free one rather than failing to start over a port few people need.
if grep -q ':4190"' "$dir/compose.yaml" && ! env_value UWUMAIL_MANAGESIEVE_BIND >/dev/null &&
  command -v ss >/dev/null 2>&1 && ss -Hltn 'sport = :4190' 2>/dev/null | grep -q . &&
  ! docker port "$service" 4190 >/dev/null 2>&1; then
  sieve_port=14190
  while ss -Hltn "sport = :$sieve_port" 2>/dev/null | grep -q .; do sieve_port=$((sieve_port + 1)); done
  set_env UWUMAIL_MANAGESIEVE_BIND "$sieve_port"
  warn "port 4190 is taken on this machine, so ManageSieve (mail rules) listens on $sieve_port instead"
fi

# ── the virus scanner ─────────────────────────────────────────────────────────────────────────
memory_kb=$(awk '/^MemTotal:/ {print $2}' /proc/meminfo 2>/dev/null || echo 0)
profiles=$(env_value COMPOSE_PROFILES || printf '')
add_antivirus=false
case ",$profiles," in
  *,antivirus,*) ;;
  *)
    if $antivirus && [ "${memory_kb:-0}" -ge 2500000 ] && grep -q '^  clamav:' "$dir/compose.yaml"; then
      yesno "This version can scan mail for viruses (ClamAV, about 1 GB of memory). Add it?" y &&
        add_antivirus=true
    fi
    ;;
esac

if [ -n "$version" ]; then
  plain_value "$version" || die "a version is letters, digits, dots, dashes and underscores"
  set_env UWUMAIL_VERSION "$version"
  step "switching to the $version tag"
fi

# ── a backup first ────────────────────────────────────────────────────────────────────────────
if $backup; then
  if docker compose ps --status running --services 2>/dev/null | grep -qx "$service"; then
    step "backing up first"
    # exec does not go through the image entrypoint, so the binary is named here.
    if ! docker compose exec -T "$service" uwumail-server backup run; then
      warn "no backup was made; that needs a backup server set up in the portal"
      yesno "Update anyway?" y || die "stopped, nothing was changed"
    fi
  else
    warn "UwUMail is not running, so nothing was backed up"
  fi
fi

# ── the new version ───────────────────────────────────────────────────────────────────────────
if $add_antivirus; then
  step "switching the virus scanner on"
  docker compose exec -T "$service" uwumail-server settings set spam.antivirus.enabled true >/dev/null ||
    warn "could not switch the scanner on; the portal does it too, under Spam filter"
  set_env COMPOSE_PROFILES antivirus
fi

step "fetching the images"
docker compose pull --quiet || die "the images could not be fetched; nothing was changed"

step "starting the new version"
docker compose up -d || die "the new version did not start. What it says: cd $dir && docker compose logs"

printf '  waiting for the server'
healthy=false
for _ in $(seq 1 60); do
  printf '.'
  sleep 2
  case "$(docker inspect --format '{{.State.Health.Status}}' "$service" 2>/dev/null)" in
    healthy)
      healthy=true
      break
      ;;
    unhealthy) break ;;
  esac
done
printf '\n'

# ── back to the old one, when the new one does not come up ────────────────────────────────────
if ! $healthy; then
  warn "the new version did not come up healthy; putting the one from before back"
  if looks_like_version "$running_version"; then
    set_env UWUMAIL_VERSION "$running_version"
  elif [ -n "$running_image" ]; then
    docker tag "$running_image" "$image:rollback" >/dev/null 2>&1 && set_env UWUMAIL_VERSION rollback
  fi
  docker compose up -d >/dev/null 2>&1
  printf '\n'
  warn "UwUMail runs on the version from before, and .env now says which one that is."
  warn "What went wrong: cd $dir && docker compose logs $service"
  exit 1
fi

{
  printf '# Written by update.sh: what it put here, so it knows what it may replace.\n'
  if $keep_compose; then
    printf 'compose keep\n'
  elif $compose_known; then
    printf 'compose %s\n' "$compose_hash"
  fi
} >"$state"
chmod 0644 "$state"

new_version=$(docker inspect "$service" \
  --format '{{index .Config.Labels "org.opencontainers.image.version"}}' 2>/dev/null)
printf '\n'
if [ -n "$new_version" ] && [ "$new_version" != "$running_version" ]; then
  printf '  UwUMail is on %s now (=^･ω･^=)\n\n' "$new_version"
else
  printf '  UwUMail is up to date (=^･ω･^=)\n\n'
fi
printf '  What is new: %s/latest\n\n' "$releases"

#!/usr/bin/env bash
# UwUMail Server, from an empty machine to a running mail server.
#
#   curl -fsSLO https://github.com/MinifyX/UwUMail-Server/releases/latest/download/install.sh
#   sudo bash install.sh
#
# It asks what it needs to know, sets up /opt/uwumail, starts the server and shows the one-time
# code for the setup assistant. Every answer is a flag as well, so it can run without questions:
#
#   sudo bash install.sh --hostname mail.example.com --email me@example.org --yes
#
#   --dir DIR            where UwUMail lives (default /opt/uwumail)
#   --hostname NAME      the public name of this mail server
#   --email ADDRESS      where Let's Encrypt warns before a certificate expires (optional)
#   --language de|en     the language of bounces and other mail the server writes
#   --gateway-code CODE  pairing code of a UwUMail Gateway (uwugw1...), for a server at home
#   --version TAG        latest (default), beta, edge, or an exact version like 0.4.0
#   --with-antivirus     bring the virus scanner along even on a small machine
#   --no-antivirus       leave the virus scanner out
#   --no-host-helper     do not let the portal look after this machine
#   --yes                ask nothing; whatever is not passed keeps its default
#   --help
#
# The whole way, with DNS, ports and a gateway: docs/install.md.
set -uo pipefail

repo=MinifyX/UwUMail-Server
releases="https://github.com/$repo/releases"
here="$(cd "$(dirname "$0")" && pwd)"

dir=/opt/uwumail
hostname_answer=""
email=""
language=de
gateway_code=""
version=latest
antivirus=auto
host_helper=true
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
    --hostname) hostname_answer="${2:?--hostname needs a name}"; shift 2 ;;
    --email) email="${2:-}"; shift 2 ;;
    --language) language="${2:?--language needs de or en}"; shift 2 ;;
    --gateway-code) gateway_code="${2:-}"; shift 2 ;;
    --version) version="${2:?--version needs a tag}"; shift 2 ;;
    --with-antivirus) antivirus=true; shift ;;
    --no-antivirus) antivirus=false; shift ;;
    --no-host-helper) host_helper=false; shift ;;
    --yes | -y) ask=false; shift ;;
    -h | --help)
      sed -n '2,25p' "$0" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *) die "unknown option: $1" ;;
  esac
done

[ "$(id -u)" -eq 0 ] || die "please run this as root: sudo bash install.sh"

# ── what has to be here already ───────────────────────────────────────────────────────────────
command -v docker >/dev/null 2>&1 || die "Docker is missing. On Ubuntu: apt install -y docker.io docker-compose-v2"
docker compose version >/dev/null 2>&1 ||
  die "Docker Compose v2 is missing. On Ubuntu: apt install -y docker-compose-v2"
docker info >/dev/null 2>&1 || die "Docker is installed but not running: systemctl start docker"

fetch() {
  local url="$1" target="$2"
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL --proto '=https' --tlsv1.2 -o "$target" "$url"
  elif command -v wget >/dev/null 2>&1; then
    wget -q --https-only -O "$target" "$url"
  else
    die "neither curl nor wget is here, so nothing can be downloaded"
  fi
}

# Downloads a release file and the checksum next to it, and only keeps it when they agree.
fetch_checked() {
  local name="$1" target="$2" base sums want have
  case "$version" in
    [0-9]*) base="$releases/download/v$version" ;;
    *) base="$releases/latest/download" ;;
  esac
  fetch "$base/$name" "$target" || return 1
  sums="$target.sha256"
  fetch "$base/$name.sha256" "$sums" || return 1
  want=$(cut -d' ' -f1 <"$sums")
  have=$(sha256sum "$target" | cut -d' ' -f1)
  rm -f "$sums"
  # A file whose checksum does not match is not a file we install.
  [ -n "$want" ] && [ "$want" = "$have" ]
}

# Next to a checkout the files are right here; otherwise they come from the newest release,
# where a name may not start with a dot: .env.example travels as env.example.
take() {
  local name="$1" target="$2" asset="${3:-$1}"
  if [ -f "$here/$name" ]; then
    install -m 0644 "$here/$name" "$target"
    step "$name from this directory"
  else
    fetch_checked "$asset" "$target" ||
      die "$asset could not be downloaded, or its checksum did not match; nothing was installed"
    step "$name from the $version release"
  fi
}

# ── the questions ─────────────────────────────────────────────────────────────────────────────
askfor() {
  local prompt="$1" fallback="${2:-}" answer=""
  if ! $ask; then
    printf '%s' "$fallback"
    return 0
  fi
  if [ -n "$fallback" ]; then
    read -r -p "  $prompt [$fallback]: " answer </dev/tty
  else
    read -r -p "  $prompt: " answer </dev/tty
  fi
  printf '%s' "${answer:-$fallback}"
}

yesno() {
  local prompt="$1" fallback="$2" answer=""
  if ! $ask; then
    [ "$fallback" = y ] && return 0 || return 1
  fi
  read -r -p "  $prompt [$([ "$fallback" = y ] && echo 'Y/n' || echo 'y/N')]: " answer </dev/tty
  answer="${answer:-$fallback}"
  case "$answer" in [yYjJ]*) return 0 ;; *) return 1 ;; esac
}

if $ask && [ ! -r /dev/tty ]; then
  die "there is no terminal to ask on. Pass --hostname ... --yes, or start this from a shell."
fi

printf '\n  UwUMail Server\n  ~~~~~~~~~~~~~~\n\n'

if [ -f "$dir/.env" ]; then
  die "$dir is already set up. Newer version? cd $dir && sudo bash update.sh"
fi

[ -n "$hostname_answer" ] || hostname_answer=$(askfor "Public name of this mail server (mail.example.com)")
case "$hostname_answer" in
  *.*) ;;
  *) die "the host name needs a dot in it, like mail.example.com" ;;
esac
case "$hostname_answer" in
  *[!a-zA-Z0-9.-]*) die "the host name may only hold letters, digits, dots and dashes" ;;
esac

[ -n "$email" ] || email=$(askfor "E-mail for certificate warnings (optional)")
if [ -n "$email" ]; then
  case "$email" in
    *@*.*) ;;
    *) die "that does not look like an e-mail address: $email" ;;
  esac
fi

language=$(askfor "Language of the mail the server writes (de or en)" "$language")
case "$language" in de | en) ;; *) die "the language is de or en, not $language" ;; esac

if [ -z "$gateway_code" ] && $ask; then
  if yesno "Does this server run at home behind a UwUMail Gateway?" n; then
    gateway_code=$(askfor "Pairing code of the gateway (uwugw1...)")
  fi
fi
if [ -n "$gateway_code" ]; then
  case "$gateway_code" in
    uwugw1*) ;;
    *) die "a pairing code starts with uwugw1, this one does not: $gateway_code" ;;
  esac
fi

# The scanner wants about a gigabyte for itself, so a small machine is better off without it.
memory_kb=$(awk '/^MemTotal:/ {print $2}' /proc/meminfo 2>/dev/null || echo 0)
if [ "$antivirus" = auto ]; then
  if [ "${memory_kb:-0}" -lt 2500000 ]; then
    antivirus=false
    step "this machine has under 2.5 GB of memory, so the virus scanner stays out (--with-antivirus overrides)"
  elif yesno "Install the virus scanner (ClamAV, about 1 GB of memory)?" y; then
    antivirus=true
  else
    antivirus=false
  fi
fi

if $host_helper && $ask; then
  yesno "Let the portal show and install this machine's system updates?" y || host_helper=false
fi

# ── the files ─────────────────────────────────────────────────────────────────────────────────
printf '\n'
step "setting up $dir"
install -d -m 0755 "$dir"
take compose.yaml "$dir/compose.yaml"
take .env.example "$dir/.env.example" env.example
take update.sh "$dir/update.sh"
chmod 0755 "$dir/update.sh"

# Writes one line of the .env, whether it is in there already, commented out, or missing.
set_env() {
  local key="$1" value="$2" file="$3" line found=false
  local tmp="$file.tmp"
  : >"$tmp"
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
  mv -f "$tmp" "$file"
}

install -m 0600 "$dir/.env.example" "$dir/.env"
set_env UWUMAIL_HOSTNAME "$hostname_answer" "$dir/.env"
set_env UWUMAIL_ACME_EMAIL "$email" "$dir/.env"
set_env UWUMAIL_LANGUAGE "$language" "$dir/.env"
set_env UWUMAIL_VERSION "$version" "$dir/.env"
set_env UWUMAIL_GATEWAY_CODE "$gateway_code" "$dir/.env"
$antivirus && set_env COMPOSE_PROFILES antivirus "$dir/.env"
step "wrote $dir/.env"

# ── the helper that looks after the machine ───────────────────────────────────────────────────
# It goes in before the first start, so the container is made with the shared directory already.
if $host_helper; then
  step "installing the helper for system updates"
  helper_tmp=$(mktemp -d)
  if fetch_checked uwumail-host.tar.gz "$helper_tmp/uwumail-host.tar.gz" &&
    tar -xzf "$helper_tmp/uwumail-host.tar.gz" -C "$helper_tmp"; then
    bash "$helper_tmp/uwumail-host/install.sh" --dir "$dir" >/dev/null ||
      warn "the helper did not install; the portal will only show the commands to copy"
  else
    warn "the helper could not be downloaded; the portal will only show the commands to copy"
  fi
  rm -rf "$helper_tmp"
fi

# ── the first start ───────────────────────────────────────────────────────────────────────────
cd "$dir" || die "cannot go into $dir"

step "fetching the images (this takes a moment)"
docker compose pull --quiet || die "the images could not be fetched"

if $antivirus; then
  # Settings live in the database, and a running server reads them at its next start, so this is
  # the one moment where switching the scanner on costs nothing.
  step "switching the virus scanner on"
  docker compose run --rm --no-deps uwumail settings set spam.antivirus.enabled true >/dev/null ||
    warn "could not switch the scanner on; the portal does it too, under Spam filter"
fi

step "starting UwUMail"
docker compose up -d || die "UwUMail did not start. What went wrong: cd $dir && docker compose logs"

# ── waiting for it ────────────────────────────────────────────────────────────────────────────
printf '  waiting for the server'
code=""
for _ in $(seq 1 60); do
  printf '.'
  sleep 2
  health=$(docker inspect --format '{{.State.Health.Status}}' uwumail 2>/dev/null)
  [ "$health" = healthy ] || continue
  code=$(docker compose logs uwumail 2>/dev/null |
    grep -oE 'one-time code [a-z0-9]{4}-[a-z0-9]{4}-[a-z0-9]{4}' | tail -1 | awk '{print $NF}')
  [ -n "$code" ] && break
done
printf '\n\n'

if [ -z "$code" ]; then
  warn "the server did not say hello in two minutes. What it says: cd $dir && docker compose logs uwumail"
  exit 1
fi

cat <<DONE
  UwUMail is running (=^･ω･^=)

  Open        https://$hostname_answer/setup
  One-time code   $code

  A new code is made at every start until the first admin account exists, so if this one is
  gone: cd $dir && docker compose logs uwumail | grep "one-time code"

  Next version:   cd $dir && sudo bash update.sh
  What is next:   https://github.com/$repo/blob/main/docs/install.md
DONE

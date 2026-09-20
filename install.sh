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
#   --smtp-bind X        where UwUMail listens when that port is taken on this machine: a port
#   --http-bind X        or an address:port, for 25, 80, 443, 465, 587 and 993 in this order.
#   --https-bind X       Without them it looks at the six itself and asks about every one it
#   --submissions-bind X finds taken. What arrives from outside keeps its own number either
#   --submission-bind X  way, so whatever sits in front has to send it to the new one.
#   --imaps-bind X
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
smtp_bind=""
http_bind=""
https_bind=""
submissions_bind=""
submission_bind=""
imaps_bind=""
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
    --smtp-bind) smtp_bind="${2:?--smtp-bind needs a port}"; shift 2 ;;
    --http-bind) http_bind="${2:?--http-bind needs a port}"; shift 2 ;;
    --https-bind) https_bind="${2:?--https-bind needs a port}"; shift 2 ;;
    --submissions-bind) submissions_bind="${2:?--submissions-bind needs a port}"; shift 2 ;;
    --submission-bind) submission_bind="${2:?--submission-bind needs a port}"; shift 2 ;;
    --imaps-bind) imaps_bind="${2:?--imaps-bind needs a port}"; shift 2 ;;
    --version) version="${2:?--version needs a tag}"; shift 2 ;;
    --with-antivirus) antivirus=true; shift ;;
    --no-antivirus) antivirus=false; shift ;;
    --no-host-helper) host_helper=false; shift ;;
    --yes | -y) ask=false; shift ;;
    -h | --help)
      sed -n '2,30p' "$0" | sed 's/^# \{0,1\}//'
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
    curl -fsL --proto '=https' --tlsv1.2 -o "$target" "$url"
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
# Whether there is a terminal to ask on. A device node that is there is not the same as one that
# answers, so this opens it rather than looking at it.
have_tty() { { : </dev/tty; } 2>/dev/null; }

askfor() {
  local prompt="$1" fallback="${2:-}" answer=""
  if ! $ask || ! have_tty; then
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
  if ! $ask || ! have_tty; then
    [ "$fallback" = y ] && return 0 || return 1
  fi
  read -r -p "  $prompt [$([ "$fallback" = y ] && echo 'Y/n' || echo 'y/N')]: " answer </dev/tty
  answer="${answer:-$fallback}"
  case "$answer" in [yYjJ]*) return 0 ;; *) return 1 ;; esac
}

if $ask && ! have_tty; then
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
    *[!a-zA-Z0-9.@_+-]*) die "an e-mail address here may only hold letters, digits and .@_+-" ;;
    *@*.*) ;;
    *) die "that does not look like an e-mail address: $email" ;;
  esac
fi

# The version becomes part of a download address and a line in the .env.
case "$version" in
  *[!a-zA-Z0-9._-]*) die "a version is letters, digits, dots, dashes and underscores: $version" ;;
esac

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

# ── the ports ─────────────────────────────────────────────────────────────────────────────────
# Docker only finds out that it cannot have a port when everything else is done already, and then
# it is the start that fails, on a machine that looks installed. So we look first, while an
# answer can still go into the .env.

# Whether something on this machine is listening there. Without ss or netstat nobody can say, and
# then Docker is the one who will.
port_busy() {
  local port="$1"
  if command -v ss >/dev/null 2>&1; then
    ss -Hltn "sport = :$port" 2>/dev/null | grep -q .
  elif command -v netstat >/dev/null 2>&1; then
    netstat -ltn 2>/dev/null | awk '{ print $4 }' | grep -qE "[:.]$port$"
  else
    return 1
  fi
}

# A port, or an address:port, the way Compose wants it. The number has to be one a port can be:
# 70000 has the shape and would fail at the start, which is the failure this whole block exists to
# prevent.
valid_bind() {
  [[ "$1" =~ ^(\[[0-9a-fA-F:]+\]:|[0-9]{1,3}(\.[0-9]{1,3}){3}:)?[0-9]{1,5}$ ]] || return 1
  local port=$((10#${1##*:}))
  [ "$port" -ge 1 ] && [ "$port" -le 65535 ]
}

# The port out of either form.
port_of() { printf '%s' "${1##*:}"; }

# The first free port from there on, so what we suggest is one that works.
free_from() {
  local port="$1"
  while [ "$port" -lt 65535 ] && port_busy "$port"; do port=$((port + 1)); done
  printf '%s' "$port"
}

# One port. What was passed wins; a free port needs nothing; a taken one is a question, or a line
# for the message at the end when there is nobody to ask. The answer goes into plan_result,
# because a subshell could not tell us what it found.
plan_result=""
ports_taken=""
plan_port() {
  local what="$1" port="$2" suggestion="$3" flag="$4" given="$5" answer
  plan_result="$given"
  if [ -n "$given" ]; then
    valid_bind "$given" || die "$flag wants a port from 1 to 65535, or an address:port, not $given"
    port_busy "$(port_of "$given")" && warn "$flag points at $given, and that one is taken as well"
    return 0
  fi
  port_busy "$port" || return 0
  if ! $ask || ! have_tty; then
    ports_taken="$ports_taken
        port $port ($what): $flag <port>"
    return 0
  fi
  warn "port $port is taken on this machine ($what)"
  answer=$(askfor "Which port should UwUMail listen on instead?" "$(free_from "$suggestion")")
  valid_bind "$answer" || die "that is not a port from 1 to 65535, or an address:port: $answer"
  plan_result="$answer"
}

plan_port "mail from other servers" 25 1025 --smtp-bind "$smtp_bind"
smtp_bind="$plan_result"
plan_port "certificate challenges" 80 8081 --http-bind "$http_bind"
http_bind="$plan_result"
plan_port "the portal and the apps" 443 8443 --https-bind "$https_bind"
https_bind="$plan_result"
plan_port "mail apps, TLS" 465 1465 --submissions-bind "$submissions_bind"
submissions_bind="$plan_result"
plan_port "mail apps, STARTTLS" 587 1587 --submission-bind "$submission_bind"
submission_bind="$plan_result"
plan_port "mail apps, IMAP" 993 1993 --imaps-bind "$imaps_bind"
imaps_bind="$plan_result"

if [ -n "$ports_taken" ]; then
  die "these ports are taken on this machine, and there is no terminal to ask on. Say where
      UwUMail should listen instead, and nothing else has to move:
$ports_taken"
fi

# Moving a mail port moves only this side of it: 25, 465, 587 and 993 are what the world knocks on.
if [ -n "$smtp_bind$submissions_bind$submission_bind$imaps_bind" ]; then
  if [ -n "$gateway_code" ]; then
    step "the gateway brings mail in through the tunnel, so the moved ports only matter to mail apps in your own network"
  else
    step "mail still arrives on 25, 465, 587 and 993 from outside: the router or firewall in front has to send those to the ports above"
  fi
fi

# ── the files ─────────────────────────────────────────────────────────────────────────────────
printf '\n'
step "setting up $dir"
install -d -m 0755 "$dir"
take compose.yaml "$dir/compose.yaml"
take .env.example "$dir/.env.example" env.example
take update.sh "$dir/update.sh"
chmod 0755 "$dir/update.sh"

# Writes one line of the .env, whether it is in there already, commented out, or missing. The
# file keeps the rights it has: it holds the gateway code and belongs to root alone.
set_env() {
  local key="$1" value="$2" file="$3" line found=false
  local tmp="$file.tmp"
  # The copy holds everything the .env holds, the gateway code included, so it is made with the
  # rights of the file it replaces rather than whatever the umask happens to be. A run that dies
  # in between leaves nothing readable behind either, which is what the trap is for.
  install -m 0600 /dev/null "$tmp"
  trap "rm -f '$tmp'" EXIT INT TERM
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

install -m 0600 "$dir/.env.example" "$dir/.env"
set_env UWUMAIL_HOSTNAME "$hostname_answer" "$dir/.env"
set_env UWUMAIL_ACME_EMAIL "$email" "$dir/.env"
set_env UWUMAIL_LANGUAGE "$language" "$dir/.env"
set_env UWUMAIL_VERSION "$version" "$dir/.env"
set_env UWUMAIL_GATEWAY_CODE "$gateway_code" "$dir/.env"
$antivirus && set_env COMPOSE_PROFILES antivirus "$dir/.env"
# Only the ports that had to move; the rest keeps the commented-out line and its explanation.
[ -n "$smtp_bind" ] && set_env UWUMAIL_SMTP_BIND "$smtp_bind" "$dir/.env"
[ -n "$http_bind" ] && set_env UWUMAIL_HTTP_BIND "$http_bind" "$dir/.env"
[ -n "$https_bind" ] && set_env UWUMAIL_HTTPS_BIND "$https_bind" "$dir/.env"
[ -n "$submissions_bind" ] && set_env UWUMAIL_SUBMISSIONS_BIND "$submissions_bind" "$dir/.env"
[ -n "$submission_bind" ] && set_env UWUMAIL_SUBMISSION_BIND "$submission_bind" "$dir/.env"
[ -n "$imaps_bind" ] && set_env UWUMAIL_IMAPS_BIND "$imaps_bind" "$dir/.env"
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

# The name goes to the web server that kept port 443, and it is not passing it on yet.
if [ -n "$https_bind" ] && [ -z "$gateway_code" ]; then
  cat <<MOVED

  The web port moved to $https_bind, so $hostname_answer will only answer once the web server in
  front of UwUMail sends that name on to it. Until then, from your own network:

      https://<this machine's address>:$(port_of "$https_bind")/setup

  The browser warns about the certificate there; that is the self-signed one from before
  Let's Encrypt could be asked. How the web server in front is set up:
  https://github.com/$repo/blob/main/docs/deployment.md#behind-a-reverse-proxy
MOVED
fi

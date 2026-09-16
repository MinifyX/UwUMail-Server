#!/usr/bin/env bash
# Installs or updates the UwUMail Gateway on a Linux machine with systemd (Debian 12+, Ubuntu 24.04+).
#
#   sudo bash install.sh ./uwumail-gateway
#
# Creates the system user uwumail-gateway, installs the binary to /usr/local/bin, the configuration
# to /etc/uwumail-gateway/gateway.toml (an existing one is kept) and the systemd service, then
# (re)starts it and shows the pairing code. The key and the pairing live in /var/lib/uwumail-gateway.
set -euo pipefail

binary="${1:?usage: sudo bash install.sh ./uwumail-gateway}"
here="$(cd "$(dirname "$0")" && pwd)"
config=/etc/uwumail-gateway/gateway.toml

if [ "$(id -u)" -ne 0 ]; then
  echo "please run this as root (sudo bash install.sh ...)" >&2
  exit 1
fi

if ! id uwumail-gateway >/dev/null 2>&1; then
  useradd --system --no-create-home --home-dir /nonexistent --shell /usr/sbin/nologin uwumail-gateway
fi
# Install first: the file may arrive without the executable bit (scp from a machine that has no
# such bit at all), and running it from here would fail for that reason alone.
install -m 0755 "$binary" /usr/local/bin/uwumail-gateway.new
trap 'rm -f /usr/local/bin/uwumail-gateway.new' EXIT
# Fails early when the binary does not fit this machine, before it replaces a working one.
/usr/local/bin/uwumail-gateway.new --version
mv /usr/local/bin/uwumail-gateway.new /usr/local/bin/uwumail-gateway
trap - EXIT

install -d -m 0755 /etc/uwumail-gateway
if [ ! -f "$config" ]; then
  install -m 0644 "$here/gateway.toml" "$config"
fi
/usr/local/bin/uwumail-gateway --config "$config" check-config

install -m 0644 "$here/uwumail-gateway.service" /etc/systemd/system/uwumail-gateway.service
systemctl daemon-reload
systemctl enable uwumail-gateway >/dev/null
systemctl restart uwumail-gateway

for _ in $(seq 1 10); do
  if systemctl is-active --quiet uwumail-gateway && [ -e /var/lib/uwumail-gateway/identity.json ]; then
    break
  fi
  sleep 1
done
if ! systemctl is-active --quiet uwumail-gateway; then
  journalctl -u uwumail-gateway --no-pager --lines 30
  echo "the gateway did not start" >&2
  exit 1
fi
echo "the UwUMail Gateway is running (=^･ω･^=)"
sleep 1
/usr/local/bin/uwumail-gateway --config "$config" code || true

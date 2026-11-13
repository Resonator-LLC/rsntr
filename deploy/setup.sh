#!/usr/bin/env bash
# Sets up the host of the official servers. Run as root on the droplet
# from a copy of this directory; idempotent. Binaries are installed by
# deploy.sh, secrets by hand (see README.md).
#
#   INSTANCES="rsntr.net:9898:7777 resonator.network:9897:7778" ./setup.sh
#
# Each instance is DOMAIN:HTTP_PORT:FEDERATION_UDP_PORT.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
INSTANCES=${INSTANCES:-"rsntr.net:9898:7777 resonator.network:9897:7778"}
RELAY_HOST=${RELAY_HOST:-rly.rsntr.net}

export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y -qq caddy ufw unattended-upgrades sqlite3 >/dev/null

for user in resonator resonator-push iroh-relay; do
  id "$user" >/dev/null 2>&1 || useradd --system --user-group --home-dir /nonexistent --shell /usr/sbin/nologin "$user"
done
install -d -m 755 /etc/resonator
install -d -m 750 -o root -g resonator-push /etc/resonator-push
install -d -m 750 -o root -g iroh-relay /etc/iroh-relay /etc/iroh-relay/tls

# Configuration (the push relay's secrets are added by hand).
install -m 640 -o root -g resonator-push "$here/config/relay.toml" /etc/resonator-push/relay.toml
# Secrets (by hand): apns.p8 and sealing.key owned by resonator-push, mode 600.
[[ -f /etc/resonator-push/env ]] || install -m 640 -o root -g resonator-push /dev/null /etc/resonator-push/env
install -m 640 -o root -g iroh-relay "$here/config/iroh-relay.toml" /etc/iroh-relay/relay.toml
install -m 644 "$here/Caddyfile" /etc/caddy/Caddyfile

domains=()
for instance in $INSTANCES; do domains+=("${instance%%:*}"); done
for instance in $INSTANCES; do
  IFS=: read -r domain port fedport <<<"$instance"
  others=""
  for other in "${domains[@]}"; do
    [[ "$other" == "$domain" ]] || others+="${others:+, }\"$other\""
  done
  sed -e "s/@DOMAIN@/$domain/g" -e "s/@PORT@/$port/g" -e "s/@FEDPORT@/$fedport/g" \
    -e "s/@OTHERS@/$others/g" "$here/config/server.toml.in" >"/etc/resonator/$domain.toml"
  chmod 644 "/etc/resonator/$domain.toml"
  install -d -m 750 -o resonator -g resonator "/var/lib/resonator/$domain" "/var/backups/resonator/$domain"
done

# The certificate copy for QUIC address discovery.
cat >/usr/local/sbin/iroh-relay-cert <<SH
#!/bin/sh
# Copies Caddy's certificate for $RELAY_HOST to iroh-relay when it changed.
set -eu
src=\$(find /var/lib/caddy/.local/share/caddy/certificates -type d -name '$RELAY_HOST' | head -1)
[ -n "\$src" ] || { echo "no certificate for $RELAY_HOST yet"; exit 0; }
dst=/etc/iroh-relay/tls
if ! cmp -s "\$src/$RELAY_HOST.crt" "\$dst/cert.pem"; then
  install -m 640 -o root -g iroh-relay "\$src/$RELAY_HOST.key" "\$dst/key.pem"
  install -m 640 -o root -g iroh-relay "\$src/$RELAY_HOST.crt" "\$dst/cert.pem"
  echo "certificate updated"
fi
SH
chmod 755 /usr/local/sbin/iroh-relay-cert

install -m 644 "$here"/systemd/* /etc/systemd/system/
systemctl daemon-reload

ufw default deny incoming >/dev/null
ufw default allow outgoing >/dev/null
ufw allow 22/tcp >/dev/null
ufw allow 80/tcp >/dev/null
ufw allow 443 >/dev/null
ufw allow 7842/udp >/dev/null
ufw allow 7777:7787/udp >/dev/null
ufw --force enable >/dev/null

systemctl enable --now unattended-upgrades >/dev/null
systemctl reload-or-restart caddy
systemctl enable --now iroh-relay-cert.timer >/dev/null
for domain in "${domains[@]}"; do
  systemctl enable "resonator@$domain" "resonator-backup@$domain.timer" >/dev/null
  systemctl start "resonator-backup@$domain.timer"
done
systemctl enable resonator-push iroh-relay >/dev/null
echo "setup done; install the binaries with deploy.sh, then start the services"

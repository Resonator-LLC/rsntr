# Deploying the official servers

How Resonator LLC runs `rsntr.net` and `resonator.network`. The scripts
are generic enough to copy for your own servers: change the names in
`provision.sh`, `setup.sh`, `Caddyfile` and `config/`.

## What runs where

One droplet (DigitalOcean `rsntr`, fra1, 4 vCPU / 8 GB, Ubuntu 26.04):

| Service | Listens | Public name |
|---|---|---|
| `resonator@rsntr.net` | 127.0.0.1:9898, udp 7777 (federation), udp 7779 (WebRTC) | `https://rsntr.net` |
| `resonator@resonator.network` | 127.0.0.1:9897, udp 7778, udp 7780 | `https://resonator.network` |
| `resonator-push` | 127.0.0.1:9900 | `https://psh.rsntr.net` |
| `iroh-relay` 1.4.1 | 127.0.0.1:3341 (HTTPS), udp 7842 (QUIC address discovery) | `https://rly.rsntr.net` |
| Caddy | 80, 443 (tcp and udp) | TLS for all of the above |

- A server has one wave domain, so each address domain is its own
  instance with its own database. They federate with each other like any
  two servers. `other_servers` makes each one's registration screen link
  to the other.
- Federation keys: `/var/lib/resonator/<domain>/federation.key`, bound by
  the `_resonator.<domain>` TXT records. Losing a key means a key rotation
  for every peer, so it is in the droplet backups. Keep a copy offline too.
- iroh-relay reads Caddy's certificate for `rly.rsntr.net` (copied by
  `iroh-relay-cert.timer`) for QUIC address discovery; its relay traffic
  (WebSocket) goes through Caddy.

## From nothing

```sh
deploy/provision.sh                        # droplet, cloud firewall, rly/psh DNS
scp -r deploy root@<ip>:/root/ && ssh root@<ip> bash /root/deploy/setup.sh
scp target/iroh-relay/bin/iroh-relay root@<ip>:/usr/local/bin/   # see deploy.sh
# Push relay secrets, owned by resonator-push, mode 600:
#   /etc/resonator-push/apns.p8, /etc/resonator-push/sealing.key
#   /etc/resonator-push/env: RESONATOR_PUSH_APNS_KEY_ID=..., RESONATOR_PUSH_APNS_TEAM_ID=...
deploy/deploy.sh root@<ip>                 # build, test, install, health check
ssh root@<ip> systemctl start iroh-relay
# Per domain: print the key and publish its TXT record.
ssh root@<ip> 'cd /var/lib/resonator/rsntr.net && sudo -u resonator resonator --config /etc/resonator/rsntr.net.toml federation key'
POINT_APEX=1 deploy/provision.sh           # point the wave domains at the droplet
ssh root@<ip> systemctl reload caddy       # retry certificates right away
```

## Day to day

- **Deploy:** `deploy/deploy.sh root@rsntr.net`. It keeps the old binaries
  as `.prev` and rolls back when a health check fails.
- **Logs:** `journalctl -u resonator@rsntr.net -f`, `-u resonator-push`,
  `-u iroh-relay`, `-u caddy`.
- **Backups:** daily per domain to `/var/backups/resonator/<domain>`,
  kept 14 days (`resonator-backup@<domain>.timer`), plus DigitalOcean's
  weekly droplet backups. To restore: stop the instance, `gunzip` a copy to
  `/var/lib/resonator/<domain>/resonator.db` (owner `resonator`), and start it.
- **Health:** `curl https://rsntr.net/healthz`,
  `curl https://psh.rsntr.net/health`, `curl https://rly.rsntr.net/`.
- **Metrics:** only on the host: `curl 127.0.0.1:9898/metrics`,
  `curl 127.0.0.1:9090/metrics` (relay).
- **Access:** ssh as root from the admin address in `provision.sh` only.

#!/usr/bin/env bash
# Creates the droplet of the official servers, its cloud firewall and the
# DNS records of the relays on DigitalOcean. Idempotent: what exists is
# left alone or updated. Needs doctl with access to the domains.
#
#   deploy/provision.sh                 # droplet, firewall, rly/psh records
#   POINT_APEX=1 deploy/provision.sh    # also point the wave domains at it
#
# Other setups: set the variables below in the environment.
set -euo pipefail

D=(doctl --context "${DO_CONTEXT:-resonator}")
NAME=${NAME:-rsntr}
REGION=${REGION:-fra1}
SIZE=${SIZE:-s-4vcpu-8gb}          # 4 vCPU, 8 GB RAM, 160 GB disk
IMAGE=${IMAGE:-ubuntu-26-04-x64}
SSH_KEYS=${SSH_KEYS:-59285118}     # comma-separated DO key ids
ADMIN_IP=${ADMIN_IP:-162.55.149.231}  # the only address allowed on ssh
RELAY_ZONE=${RELAY_ZONE:-rsntr.net}   # rly.<zone>, psh.<zone>
WAVE_DOMAINS=${WAVE_DOMAINS:-"rsntr.net resonator.network"}

droplet() { "${D[@]}" compute droplet list --format Name,"$1" --no-header | awk -v n="$NAME" '$1==n {print $2}'; }

ip=$(droplet PublicIPv4)
if [[ -z "$ip" ]]; then
  "${D[@]}" compute droplet create "$NAME" --region "$REGION" --size "$SIZE" --image "$IMAGE" \
    --ssh-keys "$SSH_KEYS" --enable-ipv6 --enable-backups --tag-names resonator,"$NAME" --wait >/dev/null
  ip=$(droplet PublicIPv4)
fi
ip6=$(droplet PublicIPv6)
id=$(droplet ID)
echo "droplet $NAME: $ip $ip6 (id $id)"

# record ZONE NAME TYPE DATA: create or update one record.
record() {
  local zone=$1 name=$2 type=$3 data=$4 rec
  rec=$("${D[@]}" compute domain records list "$zone" --format ID,Type,Name,Data --no-header |
    awk -v n="$name" -v t="$type" '!done && $2==t && $3==n {print $1" "$4; done=1}')
  if [[ -z "$rec" ]]; then
    "${D[@]}" compute domain records create "$zone" --record-type "$type" --record-name "$name" \
      --record-data "$data" --record-ttl 300 >/dev/null
    echo "dns: created $type $name.$zone -> $data"
  elif [[ "${rec#* }" != "$data" ]]; then
    "${D[@]}" compute domain records update "$zone" --record-id "${rec%% *}" --record-data "$data" \
      --record-ttl 300 >/dev/null
    echo "dns: updated $type $name.$zone -> $data (was ${rec#* })"
  else
    echo "dns: $type $name.$zone -> $data"
  fi
}

for host in rly psh; do
  record "$RELAY_ZONE" "$host" A "$ip"
  [[ -n "$ip6" ]] && record "$RELAY_ZONE" "$host" AAAA "$ip6"
done
if [[ "${POINT_APEX:-0}" == 1 ]]; then
  for zone in $WAVE_DOMAINS; do
    record "$zone" @ A "$ip"
    [[ -n "$ip6" ]] && record "$zone" @ AAAA "$ip6"
  done
fi

in="protocol:icmp,address:0.0.0.0/0,address:::/0"
in+=" protocol:tcp,ports:22,address:$ADMIN_IP/32"
for p in 80 443; do in+=" protocol:tcp,ports:$p,address:0.0.0.0/0,address:::/0"; done
# HTTP/3, QUIC address discovery, the federation endpoints of the servers.
for p in 443 7842 7777-7787; do in+=" protocol:udp,ports:$p,address:0.0.0.0/0,address:::/0"; done
out="protocol:tcp,ports:all,address:0.0.0.0/0,address:::/0 protocol:udp,ports:all,address:0.0.0.0/0,address:::/0 protocol:icmp,address:0.0.0.0/0,address:::/0"
fw=$("${D[@]}" compute firewall list --format ID,Name --no-header | awk -v n="$NAME" '$2==n {print $1}')
if [[ -z "$fw" ]]; then
  "${D[@]}" compute firewall create --name "$NAME" --droplet-ids "$id" \
    --inbound-rules "$in" --outbound-rules "$out" >/dev/null
  echo "cloud firewall: created"
else
  "${D[@]}" compute firewall update "$fw" --name "$NAME" --droplet-ids "$id" \
    --inbound-rules "$in" --outbound-rules "$out" >/dev/null
  echo "cloud firewall: updated"
fi

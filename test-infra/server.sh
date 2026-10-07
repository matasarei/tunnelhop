#!/bin/sh
# Entry point for the test server roles. Configs come from /data, written by gen.sh.
set -eu

# Add an iptables rule unless it is already there (a restarted container keeps its rules).
rule() {
    iptables -t nat -C "$@" 2>/dev/null || iptables -t nat -A "$@"
}

nat() {
    # Tunnel traffic leaves as this container's own address: that is what tells tunnels apart.
    rule POSTROUTING -s "$TUNNEL_SUBNET" -o eth0 -j MASQUERADE
    # Probe requests through the tunnel go straight to whatsmyip, so it sees this container's
    # address rather than whatever Docker's port proxy would show.
    rule PREROUTING -s "$TUNNEL_SUBNET" -d "$PROBE_ADDRESS" -p tcp --dport 8080 \
        -j DNAT --to-destination "$WHATSMYIP:8080"
}

case "${1:-}" in
wg)
    # Set up by hand rather than with wg-quick, which wants resolvconf and a full system.
    ip link del wg0 2>/dev/null || true
    ip link add wg0 type wireguard
    wg setconf wg0 /data/wg0.conf
    ip address add "$TUNNEL_ADDRESS" dev wg0
    ip link set wg0 up
    nat
    echo "WireGuard up: $(wg show wg0 listen-port) on $TUNNEL_ADDRESS"
    exec sleep infinity
    ;;
ovpn)
    nat
    exec openvpn --config /data/server.conf
    ;;
whatsmyip)
    exec python3 /usr/local/bin/whatsmyip.py
    ;;
*)
    echo "usage: server.sh wg|ovpn|whatsmyip" >&2
    exit 2
    ;;
esac

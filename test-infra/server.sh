#!/bin/sh
# Entry point for the test server roles. Configs come from /data, written by gen.sh.
set -eu

nat() {
    iptables -t nat -C POSTROUTING -s "$TUNNEL_SUBNET" -o eth0 -j MASQUERADE 2>/dev/null \
        || iptables -t nat -A POSTROUTING -s "$TUNNEL_SUBNET" -o eth0 -j MASQUERADE
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

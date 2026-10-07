# One image for every test server role: WireGuard, OpenVPN, the "what's my IP" endpoint, and
# the generator that writes their keys and the matching client configs.
FROM debian:12-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        wireguard-tools openvpn iptables iproute2 python3 openssl ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY server.sh gen.sh whatsmyip.py /usr/local/bin/
RUN chmod +x /usr/local/bin/server.sh /usr/local/bin/gen.sh

ENTRYPOINT ["/usr/local/bin/server.sh"]

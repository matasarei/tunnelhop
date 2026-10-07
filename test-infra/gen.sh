#!/bin/sh
# Write fresh keys and configs for the test servers into data/ and the matching client configs
# into out/. Both are gitignored: this is throwaway test material, never real VPN credentials.
# Refuses to overwrite unless FORCE=1, because regenerating invalidates configs already copied
# into the test VM.
set -eu

infra=/infra
data="$infra/data"
out="$infra/out"
server="${SERVER_ADDRESS:-192.168.77.1}"

if [ -e "$data" ] && [ "${FORCE:-0}" != 1 ]; then
    echo "$data exists; run with FORCE=1 to regenerate" >&2
    exit 1
fi
rm -rf "$data" "$out"
mkdir -p "$data/wg-a" "$data/wg-b" "$data/ovpn" "$out"
umask 077

# wireguard <name> <port> <third octet>
wireguard() {
    name=$1 port=$2 net=$3
    server_key=$(wg genkey)
    client_key=$(wg genkey)
    cat >"$data/$name/wg0.conf" <<EOF
[Interface]
PrivateKey = $server_key
ListenPort = $port

[Peer]
PublicKey = $(printf '%s' "$client_key" | wg pubkey)
AllowedIPs = 10.77.$net.2/32
EOF
    cat >"$out/$name.conf" <<EOF
[Interface]
PrivateKey = $client_key
Address = 10.77.$net.2/32
DNS = 1.1.1.1

[Peer]
PublicKey = $(printf '%s' "$server_key" | wg pubkey)
AllowedIPs = 0.0.0.0/0, ::/0
Endpoint = $server:$port
PersistentKeepalive = 25
EOF
}

wireguard wg-a 51820 1
wireguard wg-b 51821 2

# OpenVPN: a throwaway CA, a server and a client certificate, and a tls-crypt key.
pki=$(mktemp -d)
ec="-newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes"
openssl req -x509 $ec -days 3650 -subj "/CN=TunnelHop test CA" \
    -keyout "$pki/ca.key" -out "$pki/ca.crt" 2>/dev/null
# certificate <name> <extendedKeyUsage>
certificate() {
    openssl req $ec -subj "/CN=$1" -keyout "$pki/$1.key" -out "$pki/$1.csr" 2>/dev/null
    printf 'basicConstraints=CA:FALSE\nkeyUsage=digitalSignature,keyAgreement\nextendedKeyUsage=%s\n' "$2" \
        >"$pki/$1.ext"
    openssl x509 -req -in "$pki/$1.csr" -CA "$pki/ca.crt" -CAkey "$pki/ca.key" \
        -CAcreateserial -days 3650 -extfile "$pki/$1.ext" -out "$pki/$1.crt" 2>/dev/null
}
certificate th-test-server serverAuth
certificate th-test-client clientAuth
openvpn --genkey secret "$pki/tls-crypt.key"

cp "$pki/ca.crt" "$pki/tls-crypt.key" "$data/ovpn/"
cp "$pki/th-test-server.crt" "$data/ovpn/server.crt"
cp "$pki/th-test-server.key" "$data/ovpn/server.key"
cat >"$data/ovpn/server.conf" <<EOF
port 1194
proto udp
dev tun
topology subnet
server 10.77.3.0 255.255.255.0
ca /data/ca.crt
cert /data/server.crt
key /data/server.key
dh none
tls-crypt /data/tls-crypt.key
data-ciphers AES-256-GCM:CHACHA20-POLY1305
push "redirect-gateway def1"
push "dhcp-option DNS 1.1.1.1"
keepalive 10 60
persist-key
persist-tun
verb 3
EOF

# Only directives TunnelHop's sanitizer allows, with everything inline.
inline() { printf '<%s>\n' "$1"; grep -v '^#' "$2"; printf '</%s>\n' "$1"; }
{
    cat <<EOF
client
dev tun
proto udp
remote $server 1194
resolv-retry infinite
nobind
persist-key
persist-tun
remote-cert-tls server
data-ciphers AES-256-GCM:CHACHA20-POLY1305
verb 3
EOF
    inline ca "$pki/ca.crt"
    inline cert "$pki/th-test-client.crt"
    inline key "$pki/th-test-client.key"
    inline tls-crypt "$pki/tls-crypt.key"
} >"$out/ovpn.ovpn"

rm -rf "$pki"
chmod -R a+rX "$data" "$out"
echo "Wrote $data (servers) and $out (clients: wg-a.conf, wg-b.conf, ovpn.ovpn) for $server"

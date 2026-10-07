# Test servers

WireGuard and OpenVPN servers for TunnelHop's live tests, plus a "what's my IP" endpoint. They
run in the `th-servers` VM (192.168.77.10) on the lab network described in
[DEVELOPMENT.md](../docs/DEVELOPMENT.md) §2 and §4. Everything here is throwaway test material:
the keys are generated on the spot and never committed.

| Service | Listens on | Tunnel subnet | NATs out as |
|---|---|---|---|
| `wg-a` | 51820/udp | 10.77.1.0/24 | 172.31.77.11 |
| `wg-b` | 51821/udp | 10.77.2.0/24 | 172.31.77.12 |
| `ovpn` | 1194/udp | 10.77.3.0/24 | 172.31.77.13 |
| `whatsmyip` | 8080/tcp | n/a | n/a |

## Set up

In the `th-servers` VM, with Docker installed and this directory copied in:

```sh
docker compose run --rm gen   # keys, server configs in data/, client configs in out/
docker compose up -d --build
```

`gen` refuses to overwrite existing keys; `FORCE=1 docker compose run --rm gen` regenerates them,
after which the client configs in the test VM must be copied again. Set `SERVER_ADDRESS` if the
servers are not at 192.168.77.10.

Copy `out/` (`wg-a.conf`, `wg-b.conf`, `ovpn.ovpn`) to the PC, then into the test VM with
`scripts\vm.ps1 configs <path to out>`.

## The "what's my IP" endpoint

`http://192.168.77.10:8080/` answers with the caller's source address:

- from the test VM with no tunnel: `192.168.77.20`, the stand-in for the ISP address;
- through `wg-a`, `wg-b` or `ovpn`: that server's address in the table above.

So one probe URL tells "no tunnel" from each tunnel, the way a public service tells the ISP
address from each VPN exit. The service's `probe_urls` setting points here in tests (step 14).

## Notes

- `wg-a` and `wg-b` need the `wireguard` kernel module on the VM's host kernel (any current
  Debian or Ubuntu has it).
- The OpenVPN server authenticates by certificate only. Username and password checks arrive with
  the OpenVPN driver (step 16).
- Each server also NATs its tunnel to the internet, so traffic through a tunnel reaches it.

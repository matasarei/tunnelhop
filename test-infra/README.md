# Test servers

WireGuard and OpenVPN servers for TunnelHop's live tests, plus a "what's my IP" endpoint. They
run as containers in Docker Desktop on the PC, and the `th-test` VM reaches them over the lab
switch described in [DEVELOPMENT.md](../docs/DEVELOPMENT.md) §2 and §4. Everything here is
throwaway test material: the keys are generated on the spot and never committed.

| Service | Reached by `th-test` at | Tunnel subnet | NATs out as |
|---|---|---|---|
| `wg-a` | 192.168.77.1:41820/udp | 10.77.1.0/24 | 172.31.77.11 |
| `wg-b` | 192.168.77.1:41821/udp | 10.77.2.0/24 | 172.31.77.12 |
| `ovpn` | 192.168.77.1:1194/udp | 10.77.3.0/24 | 172.31.77.13 |
| `whatsmyip` | 198.18.77.2:8080/tcp | n/a | n/a (172.31.77.10) |

Ports are published on those two lab addresses only, so nothing here is reachable from the PC's
other networks, and `up` fails until the `th-lab` switch gives the PC those addresses.

## Set up

On the PC, in this directory:

```sh
docker compose run --rm gen     # keys, server configs in data/, client configs in out/
docker compose up -d --build    # the servers (needs th-lab)
```

`gen` refuses to overwrite existing keys; `FORCE=1 docker compose run --rm gen` regenerates them,
after which the client configs in the test VM must be copied again. Copy them in with
`scripts\vm.ps1 configs test-infra\out`. `SERVER_ADDRESS` and `PROBE_ADDRESS` override the two
lab addresses.

## The "what's my IP" endpoint

`http://198.18.77.2:8080/` answers with the caller's source address:

- with no tunnel, it sees Docker's port proxy (`172.31.77.1`): the stand-in for the ISP address;
- through `wg-a`, `wg-b` or `ovpn`, it sees that server's own address from the table. Each server
  sends probe requests arriving through its tunnel straight to the endpoint.

So one probe URL tells "no tunnel" from each tunnel, the way a public service tells the ISP
address from each VPN exit. The service's `probe_urls` setting points here in tests (step 14).

The endpoint has its own address, off the lab subnet, so that requests to it always go through
the tunnel that is up. OpenVPN's `redirect-gateway` exempts the server's own address, and an
address on the lab subnet is on-link: sent straight over the switch, where WireGuard's own
kill-switch blocks it. 198.18.77.2 (from 198.18.0.0/15, reserved for network testing) is held by
the PC on `th-lab`, and the VM reaches it through its gateway.

`gen` sits behind a compose profile, which `docker compose build` skips: after changing
`gen.sh`, run it as `docker compose run --build --rm gen`.

## Checked on 2026-10-07

From `th-test`, with the stock WireGuard 1.1.1 and OpenVPN 2.7.7 clients and the generated
configs: the endpoint answered `172.31.77.1` without a tunnel and `.11`, `.12` and `.13` through
`wg-a`, `wg-b` and `ovpn`, and traffic reached the internet through the WireGuard tunnels. The
generated client configs pass TunnelHop's sanitizers.

## Notes

- The OpenVPN server authenticates by certificate only. Username and password checks arrive with
  the OpenVPN driver (step 16).
- Each server also NATs its tunnel to the internet, so traffic through a tunnel reaches it.

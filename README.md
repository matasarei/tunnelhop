# TunnelHop

[![CI](https://github.com/matasarei/tunnelhop/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/matasarei/tunnelhop/actions/workflows/ci.yml)
[![Latest release](https://img.shields.io/github/v/release/matasarei/tunnelhop?include_prereleases&sort=semver)](https://github.com/matasarei/tunnelhop/releases)
[![Platform: Windows 10 | 11](https://img.shields.io/badge/platform-Windows%2010%20%7C%2011-0078D6)](docs/guide/en.md#1-what-you-need)
[![Licence: MIT OR Apache-2.0](https://img.shields.io/badge/licence-MIT%20OR%20Apache--2.0-blue)](#licence)

VPN server rotation for Windows. TunnelHop connects to your provider's WireGuard servers,
switches between them on a schedule, and blocks all traffic while it switches, so nothing
leaves the PC outside the tunnel.

**User guide:** [English](docs/guide/en.md) · [Українська](docs/guide/uk.md) · [Русский](docs/guide/ru.md)

## What it does

- **Rotates servers** at an interval you choose (15 minutes to 4 hours in the tray menu), or
  when you ask, and
  checks that the public IP actually changed.
- **Blocks traffic while it switches**, with its own Windows Filtering Platform kill-switch:
  nothing goes out between one tunnel and the next, or if a server stops answering.
- **Takes your provider's files as they are**: WireGuard `.conf` files, recommended, or
  OpenVPN `.ovpn` files where WireGuard isn't offered. Anything in them that could run a
  program is refused.
- **Moves to other PCs** in one file: export your servers, protected by a password, and
  import them at home or across an office.
- **Keeps sign-ins sealed**: OpenVPN usernames and passwords are stored encrypted for the PC
  by the TunnelHop service, never by the app.
- In eleven languages, following Windows: English, Ukrainian, Russian, Spanish, French,
  German, Portuguese, Italian, Polish, Simplified Chinese and Japanese.

> [!WARNING]
> TunnelHop installs a Windows service that runs as LocalSystem, and a kill-switch that
> blocks **all** traffic, your local network included, while no tunnel is up. The block
> survives crashes and restarts on purpose. If it ever stays after you expect it gone, see
> [If the internet stays blocked](docs/guide/en.md#8-if-the-internet-stays-blocked).

## Get started

1. Install [WireGuard for Windows](https://www.wireguard.com/install/).
2. Download `TunnelHop_<version>_x64-setup.exe` from
   [Releases](https://github.com/matasarei/tunnelhop/releases/latest) and run it.
3. Add your provider's `.conf` files, and choose Connect.

The [user guide](docs/guide/en.md) has every step, including OpenVPN and moving your setup to
another PC.

**Status:** 0.9, the first public release. Every feature planned for 1.0 is in; it has not yet
been used widely. The installer isn't code-signed yet, so Windows SmartScreen warns before it
runs.

## Requirements

- Windows 10 or 11, 64-bit
- WireGuard for Windows 0.5.3 or newer, and/or OpenVPN Community 2.6 or newer (not OpenVPN
  Connect)

## Documentation

- [User guide](docs/guide/en.md): installing, adding servers, exporting to other PCs
- [docs/SPEC.md](docs/SPEC.md): how it works, and the security rules it follows
- [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md): building from source, the test VM, cutting a
  release
- [docs/TESTING.md](docs/TESTING.md): what is tested, and how
- [docs/PLAN.md](docs/PLAN.md): the development plan and progress

## Licence

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or
  <https://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or <https://opensource.org/licenses/MIT>)

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion
in TunnelHop by you, as defined in the Apache-2.0 license, shall be dual licensed as above,
without any additional terms or conditions.

TunnelHop includes third-party software under its own licences. Their notices are in
[THIRD-PARTY-NOTICES.txt](THIRD-PARTY-NOTICES.txt) and, for the Rust standard library,
[THIRD-PARTY-NOTICES-RUST.html](THIRD-PARTY-NOTICES-RUST.html). The installer puts both next to
the program.

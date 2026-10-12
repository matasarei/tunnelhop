# Contributing to TunnelHop

Thanks for helping. TunnelHop runs as a LocalSystem service and controls the PC's firewall, so
a few rules here are stricter than usual. They protect your machine as much as the users'.

For a security problem, don't open an issue or a pull request: follow [SECURITY.md](SECURITY.md).

## Before you start

- **For anything bigger than a small fix, open an issue first** and say what you'd like to
  change. It saves you building something that doesn't fit.
- **[docs/SPEC.md](docs/SPEC.md) is the source of truth** for behaviour, formats and security
  rules. A change in behaviour changes the SPEC in the same pull request.
- **[docs/PLAN.md](docs/PLAN.md)** says what's done and what's next.

## Setting up

You need Windows 10 or 11 (x64), [rustup](https://rustup.rs) (the toolchain is pinned in
`rust-toolchain.toml` and installs itself), and Node.js 24.

```powershell
git config core.hooksPath .githooks   # once per clone: the checks below run before every push
cd app; npm ci; npm run build; cd ..   # the app embeds the built window, so Rust needs it first
cargo test --workspace
```

[docs/DEVELOPMENT.md](docs/DEVELOPMENT.md) covers the rest: building the app and the installer,
the Hyper-V test VM, and the lab VPN servers.

## Safety rules

These are not negotiable, and a pull request that breaks one is closed.

- **Never run the kill-switch, the leak test or the live tunnel tests on your own PC.** The
  kill-switch's firewall filters are meant to survive crashes and restarts, so a bug can take
  your machine offline until you remove them by hand. Run them in a virtual machine, with a
  checkpoint taken first (DEVELOPMENT.md, sections 3–6). The ordinary `cargo test` is safe:
  the live tests are `#[ignore]`d.
- **Never commit real VPN configs, keys or credentials**, yours or anyone's, not even in a test.
  Test fixtures are obviously fake; the lab generates its own throwaway keys.
- **TunnelHop only manages tunnels named `th-…`.** Code must never touch a user's own
  WireGuard or OpenVPN tunnels, and never sets WireGuard's `DangerousScriptExecution`.
- **No code copied from wireguard-windows, OpenVPN or other VPN clients**, whatever their
  licence. Read them to understand the behaviour if you like, then write it fresh.
- **No code from anywhere else under a different licence** unless the pull request says where
  it's from and under what licence, so it can be checked first.

## Making a change

- **One branch and one pull request per change.** A fix and an unrelated refactor are two pull
  requests.
- **Tests come with the change**: in `crates/*/tests` or beside the code for Rust, and vitest
  (`app/src/**/*.test.ts`) for the window's own logic. Tests that need the service running or a
  real tunnel are `#[ignore]` live tests, run in the VM.
- **The pre-push hook must pass**: formatting, the app's type check and tests, the user-guide
  check, clippy with warnings as errors, and every test. Never push with `--no-verify`.
- **Commit messages** start with a short sentence saying what changed (no `feat:` prefixes),
  and the body says why. Look at `git log` for the style.
- **If you change what the window says**, change every file in `app/locales/`. The user
  guides in `docs/guide/` quote those labels, and the pre-push check fails when a quoted label
  no longer exists.
- **If you add, remove or update a dependency**, regenerate the third-party notices with
  `node scripts/third-party-notices.mjs` and commit them too (it needs cargo-about, see
  DEVELOPMENT.md, "Cutting a release").

## Licence

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion
in TunnelHop by you, as defined in the Apache-2.0 license, shall be dual licensed under the MIT
and Apache-2.0 licences, without any additional terms or conditions. See the
[README](README.md#licence).

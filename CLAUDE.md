# TunnelHop

WireGuard/OpenVPN rotation orchestrator for Windows: a privileged service (`th-service`), an
unelevated Tauri UI, and an app-owned WFP kill-switch.

## Read first
- `docs/SPEC.md`: behaviour, formats and security rules. It's the source of truth.
- `docs/PLAN.md`: what's done, what's next, and how each step is proven. Resume with
  `/gku:implement docs/PLAN.md --continue`.
- `docs/DEVELOPMENT.md`: the Windows PC, the Hyper-V test VMs, and how builds reach them.

## Layout
- `crates/th-core`: platform-independent logic (import, sanitizers, rotation engine, IPC).
  Tested everywhere, no Windows APIs.
- `crates/th-service`: the Windows service (from plan step 10).
- `app/`: the Tauri UI (from step 17).

## Commands
- Everything: `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
- Before every push, the same plus the app's type check runs in `.githooks/pre-push` (about a
  minute). Enable it once per clone: `git config core.hooksPath .githooks`. CI runs only the
  platform-independent th-core job (on a Linux runner) and, when service code changes, the
  Windows release gate. A `v*` tag builds the installer into a draft release (`release.yml`).
- Core only: `cargo test -p th-core`
- Live tests (`--ignored`) run only inside the `th-test` VM, through PowerShell Direct (DEVELOPMENT.md §5).

## Hard rules
- **Never run the kill-switch, leak tests or live tunnel tests on the host machine.** WFP
  filters persist across crashes and reboots by design; a bug cuts the host off, and this
  session with it. Use `th-test`, and take a checkpoint first.
- **Never commit real VPN configs, keys or credentials.** Provider files live outside the repo.
  Test fixtures are obviously fake.
- **Only manage tunnels prefixed `th-`.** Never touch the user's own tunnels, and never set
  WireGuard's `DangerousScriptExecution`.
- **No code copied from wireguard-windows, OpenVPN or other VPN clients.** Write it fresh.
- **One branch and PR per plan step** (or per closely related pair). Record rulings in the
  commit body and in `docs/PLAN.md`.

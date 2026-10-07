# TunnelHop v1 — development plan

**Type:** feature
**Spec:** [`docs/SPEC.md`](SPEC.md) is the source of truth for behaviour, formats and security
rules. This plan says what to build, in what order, and how each step is proven.
**Environment:** [`docs/DEVELOPMENT.md`](DEVELOPMENT.md) covers the Windows PC, the Hyper-V test
VM and the safety rules for live tests.

Resume with `/gku:implement docs/PLAN.md --continue`, which picks up at the first unticked
step. Progress is recorded in this file and committed with each step.

## Summary
- **Done (on macOS, merged to `main`):** the platform-independent core `th-core`, covering
  bundle import, config sanitizers, the rotation engine and the IPC protocol, plus OpenVPN
  credential handling and the change-the-IP rotation rule. It's tested on Linux and Windows CI.
- **Next (on a Windows PC):** the privileged service, the WireGuard and OpenVPN drivers, the
  WFP kill-switch, wiring it all to the engine, then the Tauri UI and the installer
  (steps 10–19).
- **Biggest risk:** the WFP kill-switch. A bug either leaks traffic or leaves the machine
  offline. Its filters are persistent by design, so every kill-switch and leak test runs in
  the Hyper-V test VM, never on the host (see DEVELOPMENT.md).

## Design notes for the remaining work

These complement the spec with implementation choices it doesn't make.

### Layout still to create
```
crates/th-service/            # Windows only (cfg(windows)); stub main elsewhere
  src/main.rs                 # `th-service run|install|uninstall|disengage`
  src/svc.rs                  # windows-service control handler (STOP, PRESHUTDOWN)
  src/pipe.rs                 # named pipe server \\.\pipe\tunnelhop, SDDL-restricted
  src/store.rs                # %ProgramData%\TunnelHop, ACLs, DPAPI secrets, settings.json
  src/runtimes.rs             # locate wireguard.exe / openvpn.exe, check minimum versions
  src/drivers/wireguard.rs    # WireguardDriver
  src/drivers/openvpn.rs      # OpenvpnDriver
  src/drivers/ovpn_mgmt.rs    # management protocol client (cross-platform, fake-server tests)
  src/killswitch.rs           # WFP provider/sublayer, persistent filters
  src/probe.rs                # HTTPS IP probe (reqwest + rustls)
  src/orchestrator.rs         # rotation engine + drivers + kill-switch + timers + pipe events
app/                          # Tauri v2 + Svelte 5 + Vite, runs unelevated
test-infra/                   # WireGuard + OpenVPN test servers (docker compose, in Docker Desktop on the PC)
docs/TESTING.md               # the full manual acceptance procedure (step 19)
```

### Service
- `th-service.exe` runs as LocalSystem, start type automatic. It owns tunnels, the kill-switch,
  timers, bundles and secrets.
- Pipe SDDL `D:(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;OW)(A;;0x12019b;;;IU)`: interactive users get
  read and write without `FILE_CREATE_PIPE_INSTANCE` (step 11 ruling). Requests are validated with
  `th_core::ipc::Request::validate` before anything else.
- Storage under `%ProgramData%\TunnelHop\` with an ACL granting only SYSTEM and Administrators
  full control (SPEC §2.3). Secrets are DPAPI blobs from `CryptProtectData`, LocalSystem scope.
- Imports arrive as bytes over the pipe (`ImportBundle`); the service never opens a path the UI
  names.

### Kill-switch (WFP)
- A fixed provider GUID and a fixed sublayer GUID. Every filter is `FWPM_FILTER_FLAG_PERSISTENT`.
- `engage(endpoints)` at `ALE_AUTH_CONNECT_V4/V6` and `ALE_AUTH_RECV_ACCEPT_V4/V6`:
  - Permit loopback, DHCPv4 (UDP 67/68), DHCPv6 (UDP 546/547), ICMPv6 NDP types 133–137,
    and each endpoint's IP, port and protocol.
  - Block everything else. Permits at weight 15, the block at 0, inside our sublayer.
- `allow_interface(luid)` / `revoke_interface(luid)` add and remove the permit for a tunnel
  interface. `disengage()` deletes everything in our sublayer.
- On start, if filters exist and no tunnel is up, keep them and report `Blocked`.
- **Dev builds only:** a safety timer that disengages after 120 s unless the test confirms it
  should stay (see step 13). It must not exist in release builds, and CI checks that.

### Drivers
- Specified in SPEC §4.2 (WireGuard) and §4.3 (OpenVPN). Implementation notes:
  - WireGuard: `ConvertInterfaceAliasToLuid(<tunnel name>)` after the service reaches
    `SERVICE_RUNNING`. `Endpoint` in the copied config is rewritten to the resolved IP.
  - OpenVPN: spawn with `CREATE_SUSPENDED`, assign to the Job Object, then resume. Map
    `>STATE:` `CONNECTED` to up (the LUID from the reported local IP), `RECONNECTING` to
    reconnecting, and `EXITING` to down.

### Wiring the engine (step 14)
- Call `Rotator::set_entry_hosts` from `ImportedBundle.endpoints` (hosts as written).
- Call `Rotator::set_missing_credentials` from the credential store: profiles in
  `needs_credentials` that have no stored secret.
- Emit `Event::Alert` when `Status.ip_changed == Some(false)` ("the public IP did not change")
  and when a rotation ends `Blocked`.
- Probe URLs are configurable (`settings.json` → `probe_urls`), so tests can use the lab's
  "what's my IP" endpoints.

## Acceptance criteria

- [x] `cargo test -p th-core` passes on Linux and on `windows-latest` CI.
- [x] Hostile zips are refused and leave nothing under the bundle root: `../` paths, absolute
  paths, symlinks, duplicate names, more than 500 entries, a ratio over 100, more than 50 MB,
  a missing profile file, an unknown manifest field, or Windows-reserved names.
- [x] `.ovpn` script, plugin and file directives are refused with file and line, including
  quoted, escaped and chunk-split variants. So are WireGuard hook keys.
- [ ] After import, no plaintext `auth.txt` exists on disk, and `bundles\`, `tunnels\` and
  `secrets\` are unreadable by a standard user (`icacls` shows SYSTEM and Administrators only).
- [ ] The UI runs without elevation: Task Manager's "Elevated" column shows No, and no UAC
  prompt appears at launch or on connect.
- [ ] Two bundles that both contain `wireguard/ua-kyiv-01.conf` import, and each connects
  under its own `WireGuardTunnel$th-…` service.
- [ ] During 20 consecutive "Rotate Now" switches in the test VM, a packet capture on its
  network adapter shows **zero** packets other than to the VPN server endpoints, DHCP and
  NDP, across WireGuard→OpenVPN and OpenVPN→WireGuard switches.
- [ ] If `th-service.exe` is killed mid-rotation, traffic stays blocked. After a restart the
  service reconnects or offers "Disconnect & unblock", and leaves no stray
  `WireGuardTunnel$th-*` or `openvpn.exe`.
- [ ] The OpenVPN management port refuses a client without the password, and the port differs
  between two consecutive connects.
- [x] A one-profile bundle rotates by reconnecting that profile (engine level).
- [ ] When all candidates fail, the state is `Blocked`, the tray shows an alert, and no traffic
  leaves the adapter.
- [ ] An OpenVPN profile with a bare `auth-user-pass` (Proton) imports, is reported as needing
  credentials, and is never picked for rotation until they are set. After the UI asks once
  and stores them via DPAPI, the profile connects. (The engine part is done.)
- [ ] A timer or manual rotation that keeps the same public IP tries another candidate,
  preferring a different entry. If none changes it, it stays connected and the tray says the
  IP did not change. (The engine part is done.)
- [ ] A rotation interval set in the tray survives a service restart and overrides the
  manifest's `interval_minutes`.
- [ ] Tray "Exit" and the NSIS uninstall both leave no tunnel services, no filters under our
  provider (`netsh wfp show filters`), and working internet.
- [ ] `wireguard.exe` < 0.5.3 or `openvpn.exe` < 2.6 shows as "runtime too old" in the UI, and
  that protocol is disabled.
- [ ] The release build contains no dev-only safety timer or SSH exception (CI check).

## Steps

Test order: build-then-cover, with each step's tests landing in the same commit. One branch per
step (or per closely related pair), opened as a PR against `main`.

### M0–M1: core (done on macOS)
1. [x] Repo, spec rewrite, `docs/manifest.schema.json`.
2. [x] Cargo workspace (edition 2024, toolchain 1.99) and CI (Linux: `th-core`; Windows:
   workspace).
3. [x] Manifest model and validation (`crates/th-core/src/manifest.rs`).
4. [x] Safe zip import with staging (`crates/th-core/src/bundle.rs`).
5. [x] OpenVPN and WireGuard sanitizers (`crates/th-core/src/sanitize/`).
6. [x] Tunnel names (`crates/th-core/src/tunnel_name.rs`).
7. [x] Driver trait and test doubles (`crates/th-core/src/driver.rs`, `src/testing.rs`).
8. [x] Rotation engine (`crates/th-core/src/rotation.rs`).
9. [x] IPC protocol (`crates/th-core/src/ipc.rs`).

### M2: service skeleton + WireGuard
10. [x] The Windows service shell: `run`/`install`/`uninstall`/`disengage`, the control
    handler, the ACL'd store and DPAPI, runtime discovery and version checks, and logging.
    - Also: store OpenVPN credentials per bundle and profile from `SetCredentials`, and
      report `has_credentials` per profile.
    - Also (step 26): `Secret` wipes its memory on drop (`zeroize`).
    - Create: `crates/th-service/src/{svc.rs,store.rs,runtimes.rs}`
    - Modify: `crates/th-service/src/main.rs`
    - Test: unit tests for version parsing and the credential store's key layout. In the
      test VM: `th-service install`, `sc query TunnelHop` shows RUNNING, and `icacls` on
      `%ProgramData%\TunnelHop` matches.
    - **Landed (2026-10-07), VM check pending:** `crates/th-service` is now a library plus
      `main.rs`, with `svc.rs` (SCM registration, STOP/PRESHUTDOWN, restart on crash),
      `store.rs` (layout, ACLs, `settings.json`), `credentials.rs` (DPAPI store,
      `has_credentials`), `runtimes.rs`, `logging.rs`, and `win.rs` (every `unsafe` Win32
      call). `Secret` and `Credentials` in `th-core` are zeroized on drop. 130 tests pass on
      the PC; `th-service runtimes`, `run` from a console, and an unelevated `install` behave.
    - **Proven in `th-test` (2026-10-07, Windows 11 Home 26300):** `install` gives a RUNNING,
      automatic LocalSystem service with the restart actions and the non-crash flag set;
      `icacls` shows SYSTEM and Administrators on the store and the restricted entries on
      `secrets\`; the pipe opens; STOP and start work; `vm.ps1 test lib -AsSystem` passes
      (`prepare` replaces a planted junction); an administrator cannot read a credential file
      SYSTEM wrote; `uninstall` removes the service, the data and the pipe. The first run found
      the C runtime DLL missing and `uninstall` unable to delete `secrets\` (both fixed).
11. [x] The named pipe server with SDDL, dispatching `th_core::ipc` requests.
    - Create: `crates/th-service/src/pipe.rs`, `crates/th-service/tests/pipe.rs`
      (`#[cfg(windows)]`)
    - Test: on Windows CI, connect as the runner user and do a `Ping` round-trip; a request
      that fails `validate` gets `Response::Error`.
    - **Landed (2026-10-07):** `pipe.rs` (`Listener`, the `Handler` trait, `pipe::connect` for
      clients) and `tests/pipe.rs` (10 tests: ping, validation before the handler, wrong
      version, request before hello, second hello, malformed line, events to every client,
      name squatting, shutdown), plus a unit test that pins the pipe's DACL. The service opens
      the pipe at start; until step 14 its handler answers `GetStatus` and refuses the rest.
12. [x] WireguardDriver: connect, disconnect, status and reconcile. Also the test servers and
    the VM helper.
    - Create: `crates/th-service/src/drivers/{mod.rs,wireguard.rs}`,
      `test-infra/docker-compose.yml`, `test-infra/README.md`, `scripts/vm.ps1`
    - Test servers (DEVELOPMENT.md §4):
      - two WireGuard servers and one OpenVPN server, each with its own tunnel subnet;
      - a "what's my IP" HTTP endpoint on each tunnel-side address;
      - a script that writes matching client configs into `test-infra/out/` (add it to
        `.gitignore`).
    - `scripts/vm.ps1`: build, copy into `th-test` over PowerShell Direct, run a test,
      checkpoint, restore.
    - Test: unit tests for the config-copy `Endpoint` rewrite. In the test VM:
      `cargo test -p th-service --test wg_live -- --ignored` against the test servers
      (connect, service RUNNING, disconnect, service gone).
    - **Landed (2026-10-07), live check pending:** `drivers/wireguard.rs`, `tests/wg_live.rs`,
      `test-infra/` (Dockerfile, compose, `gen.sh`, `server.sh`, `whatsmyip.py`) and
      `scripts/vm.ps1`. The unit tests pass on the PC. The test servers run in Docker Desktop
      on the PC and were checked with clients in containers: all three tunnels connect, and
      the probe tells them apart.
    - **Proven in `th-test` (2026-10-07):** `wg_live` passes against `wg-a` (connect, RUNNING,
      interface, reconcile reads the endpoints back, reconnect over a leftover, disconnect
      twice). From the VM the probe at `http://198.18.77.2:8080/` answers `172.31.77.1` with no
      tunnel, `.11` through `wg-a`, `.12` through `wg-b` and `.13` through `ovpn`, and traffic
      reaches the internet through the WireGuard tunnels.

### M3: kill-switch + live rotation (test VM only)
13. [x] The WFP kill-switch: engage, allow/revoke interface, disengage and reconcile, all
    persistent. Plus the dev-build safety timer.
    - Create: `crates/th-service/src/killswitch.rs`
    - Modify: `crates/th-service/src/main.rs` (`disengage` calls `KillSwitch::disengage`)
    - Safety timer: behind a `dev-safety` Cargo feature, off by default. When on, an engage
      not confirmed within 120 s is disengaged automatically. CI builds the release
      configuration and fails if the feature's marker symbol is present.
    - Test (`killswitch_live`, `--ignored`, in the VM, after a checkpoint):
      - after engage, `curl https://example.com` fails while a TCP connect to a test-server
        endpoint succeeds;
      - after disengage, curl works;
      - kill the process while engaged, restart, and the filters still exist;
      - with `dev-safety`, an unconfirmed engage lifts by itself.
    - Open question to settle here: how our filters arbitrate with WireGuard's own
      kill-switch sublayer when both are active (SPEC assumes a block in any sublayer wins).
    - **Landed and proven in `th-test` (2026-10-07):** `killswitch.rs` (the rule set as plain
      data with unit tests, and `WfpKillSwitch`, which implements the engine's `KillSwitch`
      trait), `tests/killswitch_live.rs`, `th-service disengage`, and the CI marker check. In
      the VM: while engaged only a permitted endpoint is reachable, and the internet, DNS and
      the LAN are blocked; engaging again replaces the endpoint set; a WireGuard tunnel carries
      traffic only while its interface is allowed; a block from a process that aborted
      survives, and `th-service disengage` lifts it; with `dev-safety`, an unconfirmed engage
      lifts itself and a confirmed one stays; afterwards `netsh wfp show filters` lists
      nothing under TunnelHop's provider. Settled: see "Kill-switch and WireGuard's own" below.
14. [x] IP probe and orchestrator wiring: the rotation engine with real drivers and kill-switch,
    timers, settings overrides, and pipe events (see "Wiring the engine" above).
    - The probe URLs come from `settings.json` (`probe_urls`), defaulting to ipify then ipinfo.
      Tests point them at the test servers' "what's my IP" endpoints, because the lab's
      public address is the same with or without a tunnel (DEVELOPMENT.md §4).
    - A small CLI over the pipe (`th-service ctl rotate-now`, `status`, `import <zip>`) so the
      leak test can drive the service without the UI.
    - Create: `crates/th-service/src/{probe.rs,orchestrator.rs}`
    - Modify: `crates/th-service/src/pipe.rs` (route `Connect`/`RotateNow`/`SetAutoRotate`/
      `SetInterval`/`Disconnect`/`Shutdown`/`SetCredentials`/`ImportBundle` to the
      orchestrator)
    - Test: the leak test in the VM (`pktmon` over 20 rotations, WireGuard only; procedure in
      DEVELOPMENT.md).
    - **Landed and proven in `th-test` (2026-10-07):** `probe.rs`, `orchestrator.rs` (generic,
      10 tests with th-core's doubles), the service running it, `th-service ctl`,
      `bundle::open` in th-core, test bundles from `gen.sh`, and `scripts/leak-test.ps1`.
      In the VM, through the installed service and `ctl`: import, select and connect, then
      rotations alternate between `wg-a` and `wg-b` with the probe seeing `172.31.77.11` and
      `.12`, and disconnect leaves no tunnel and no block. The leak test over 20 rotations:
      **0 frames sent by the VM outside the tunnel** (4,144 of 4,256 captured frames were
      WireGuard; the rest ARP and one inbound TCP reset from outside).

### M4: OpenVPN
15. [x] The management protocol client: auth, `state on`, `hold release`, `>STATE`/`>PASSWORD`
    parsing, and `signal SIGTERM`.
    - Also: escape username and password for the `username`/`password` commands, so quotes
      and backslashes can't break the line.
    - Create: `crates/th-service/src/drivers/ovpn_mgmt.rs`, `crates/th-service/tests/ovpn_mgmt.rs`
    - Test: cross-platform, against a fake TCP server replaying transcripts: wrong password,
      a state sequence, a password prompt (with quotes and backslashes in the credentials),
      and a server-side close.
    - **Landed (2026-10-07):** `drivers/ovpn_mgmt.rs` (a line parser plus the session) and
      `tests/ovpn_mgmt.rs`: 8 transcript tests, the four the plan names plus a port that never
      asks for a password (not ours), a notification arriving between a command and its answer,
      a refused command whose error carries no credential, and an endless line. Also
      `tests/ovpn_mgmt_live.rs`, run in `th-test` against the real OpenVPN 2.7.7 and the lab
      server: login, hold release, CONNECTED with a `10.77.3.x` address in about a second,
      then SIGTERM, a clean exit within 15 s, and no route left behind.
16. [x] OpenvpnDriver: random port and password file, Job Object spawn, credentials from
    DPAPI, the teardown timeout, and reconnect grace.
    - Create: `crates/th-service/src/drivers/openvpn.rs`
    - Modify: `crates/th-service/src/orchestrator.rs` (register the driver by protocol)
    - Test: `ovpn_live` (`--ignored`) in the VM against the test servers, then the leak test
      again with mixed-protocol rotation.
    - Note from step 12: a *killed* `openvpn.exe` leaves its routes (`0.0.0.0/1` and
      `128.0.0.0/1` through the dead tunnel, and a host route to the server). Stop it through
      the management interface; on reconcile after a crash, remove routes whose next hop is a
      tunnel that no longer exists.
    - Note from step 15: the management port is a local TCP port, so another local process could
      bind it first and pose as OpenVPN, and the client would then hand it the VPN credentials
      when it asks. Before connecting, check that the listening socket on that port belongs to
      the `openvpn.exe` the driver started (`GetExtendedTcpTable` gives the owning process).
    - **Landed (2026-10-07):**
      - `drivers/openvpn.rs`, registered in `svc.rs`. The orchestrator already picks drivers by
        protocol, so it needed no change.
      - `rewrite_remotes`/`installed_remotes` in th-core's OpenVPN sanitizer.
      - Job, resume, listener-owner, address, interface and route helpers in `win.rs`.
      - `tests/ovpn_live.rs`: connect and disconnect; credentials (none, wrong, right with a
        quote and a backslash); the crash cleanup. Passed three runs in a row from a restored VM.
      - The lab server now checks `th-test-user` but still admits certificate-only clients.
      - The mixed leak test: 20 rotations, public IPs `.11` ×8, `.12` ×7, `.13` ×5, and
        **0 frames sent by the VM outside the tunnel**.
    - **Found along the way:**
      - A second `hold release` raced OpenVPN's credential prompt; `start()` now consumes the
        startup hold.
      - CONNECTED arrives while the tunnel address is still tentative (about 2 s), so the
        driver waits until it is usable.
      - Step 14's `Running::stop` panicked on every service stop ("no reactor running"), so
        the service never disconnected or lifted the block before exiting; the SCM restarted
        it. Fixed, and panics are now logged.
    - **Correction to step 12's note:** with the data-channel-offload adapter, a killed OpenVPN's
      `0.0.0.0/1` and `128.0.0.0/1` go with the adapter. What stays is the host route to the
      server, on the physical interface. The crash test asserts exactly that.

### M5: UI + installer
17. [ ] The Tauri app shell: pipe client, tray menu, close-to-tray, and autostart.
    - Create: `app/` (`npm create tauri-app@latest`, Svelte + TS),
      `app/src-tauri/src/{pipe_client.rs,tray.rs}`
    - Modify: `Cargo.toml` (add `app/src-tauri` to the workspace)
    - Test: in the VM, by hand: every tray item works, and the UI process is not elevated.
18. [ ] The dashboard and import: drag-and-drop or picker, bytes over the pipe, the error list.
    - Also: when `Imported.needs_credentials` is non-empty, ask once for a username and
      password for all of them. The profile list marks any profile still waiting, with an
      action to set them.
    - Create: `app/src/routes/Dashboard.svelte`, `app/src/lib/Import.svelte`, `app/src/lib/ipc.ts`
    - Test: `npm run check`, plus importing good and hostile bundles in the VM.
19. [ ] NSIS installer hooks (service install and uninstall, full cleanup) and
    `docs/TESTING.md` covering every acceptance criterion.
    - Create: `app/src-tauri/windows/hooks.nsh`, `docs/TESTING.md`
    - Modify: `app/src-tauri/tauri.conf.json` (`bundle.windows.nsis.installerHooks`,
      `th-service.exe` as a resource)
    - Test: in the VM from the clean checkpoint: install, connect, rotate, uninstall.
      `sc query TunnelHop` should show the service gone, `netsh wfp show filters` should show
      no provider GUID, and internet should work. Then one final pass on the host PC itself.

### Postponed nits
20. [ ] `crates/th-core/src/ipc.rs` (`FrameBuffer::next_frame`): remember how far the buffer
    has been scanned, so a 27 MB import isn't rescanned on every push.
21. [ ] `crates/th-core/src/ipc.rs` (`Request::validate`): validate the import payload without
    decoding 20 MB just to discard it, or hand the decoded bytes back to the caller.
22. [ ] `crates/th-core/src/rotation.rs` (`Rotator::settle`): don't ignore the steady-state
    `engage` result; log it or surface it in `Status`.
23. [ ] `.github/workflows/ci.yml`: pin actions by commit SHA and add
    `permissions: contents: read`.

### Added after checking real ProtonVPN files
24. [x] OpenVPN credentials when the config has none (core part). The service, management and
    UI parts are folded into steps 10, 15 and 18.
25. [x] Rotations must change the public IP (engine). The service part is folded into step 14.
26. [x] `Secret` wipes its memory on drop — `zeroize` on `ipc::Secret` and
    `sanitize::Credentials`, and on the `auth.txt` text while it's parsed (step 10's branch).

## How to check it
- `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
  (on the PC, and on CI for every PR)
- Live tests, in the test VM only: `cargo test -p th-service -- --ignored`, run through
  PowerShell Direct as described in DEVELOPMENT.md
- The leak procedure in DEVELOPMENT.md: an empty capture apart from VPN endpoints, DHCP and NDP

## Do not touch
- **The user's own WireGuard and OpenVPN tunnels.** Only `th-`-prefixed tunnel services and our
  own `openvpn.exe` job are ever managed. Never set
  `HKLM\Software\WireGuard\DangerousScriptExecution`.
- **No `requireAdministrator` manifest on the UI.** All privilege lives in `th-service.exe`.
- **Never reimplement tunnel crypto or drivers.** Delegating to the installed runtimes is the
  premise.
- **No code copied from wireguard-windows, OpenVPN or other VPN clients.** Describe behaviour,
  write it fresh.
- **Never commit real VPN configs or credentials.** `.gitignore` refuses `*.conf`, `*.ovpn`,
  `auth.txt` and `*.zip` outside test fixtures; fixtures stay obviously fake.
- **Never run the kill-switch or live tests on the host PC** before step 19's final pass.

## Decisions so far

Made with the developer:
- **Architecture:** privileged service plus an unelevated Tauri v2 UI. Bundles are hostile by
  default. An app-owned WFP kill-switch across rotation.
- **Runtimes:** WireGuard ≥ 0.5.3, OpenVPN ≥ 2.6.
- **Allowlist additions:** `reneg-sec <n>` and `setenv CLIENT_CERT 0` (ProtonVPN ships both).
- **Missing OpenVPN credentials:** asked for once in the UI and stored via DPAPI. An `auth.txt`
  in the bundle stays supported.
- **Rotations must visibly change the public IP.** A same-IP result is a soft failure, never a
  reason to block.
- **Testing:** the Windows phase runs on an x64 Windows PC, with live tests in a Hyper-V VM
  controlled through PowerShell Direct (DEVELOPMENT.md).

Assumed and open to change:
- Svelte 5 for the UI.
- LAN blocked while the kill-switch is engaged.
- "Ping" dropped from the dashboard.
- Probe endpoints: ipify, then ipinfo `/ip`.

Implementation rulings worth knowing (full text in the commit bodies):
- **Import** returns `ImportedBundle` with the manifest, endpoints, credentials and
  `needs_credentials`. Only referenced files are written.
- **OpenVPN tokenizer:** it mirrors OpenVPN's own quoting, escaping and comment rules. Lines
  over 255 bytes and NUL bytes are refused, inside inline blocks too, because OpenVPN reads in
  256-byte chunks.
- **Certificates and keys** must be inline in the `.ovpn`, not referenced as separate files.
- **Engine safety:** a kill-switch engage failure aborts before teardown, and a teardown
  failure stops in `Blocked` rather than raising a second tunnel. `stay_down` keeps the
  kill-switch engaged.
- **Credentials over IPC** are refused if they contain CR, LF or NUL or exceed 256 characters.
  Profile ids follow the manifest's `^[a-z0-9-]{1,40}$`.
- **Protocol version** stays 1 until something speaking it ships.
- **Store ACLs (step 10):** the root and `secrets\` are created with their DACL already set,
  so they're never briefly open to what `ProgramData` grants. Each start re-applies the DACLs.
  A pre-existing root that is a link, or is owned by anyone but SYSTEM, Administrators or the
  service, is treated as planted: it's deleted and created again. Owners aren't changed.
- **Credential files (step 10):** `secrets\credentials\<bundle_id>\bundle.cred` holds the
  bundle-wide default (`SetCredentials` with no profiles); `profile.<id>.cred` holds one
  profile's credentials and wins over the default. Each is DPAPI under the service's account
  with fixed extra entropy, and never `CRYPTPROTECT_LOCAL_MACHINE`. Ids are re-validated
  before they become file names.
- **Runtimes (step 10):** an executable whose version can't be read counts as "too old"
  (found `unknown`), which disables its protocol. `th-service runtimes` prints what was found.
- **`settings.json`:** unknown fields are ignored, and invalid values are dropped one by one.
- **`disengage`** is a no-op until step 13 gives it a kill-switch to remove.
- **`install` registers the executable where it is.** It must sit in a directory only
  administrators can write (Program Files; `C:\th` in the VM is locked down for this).
- **Pipe rights (step 11):** for a pipe, `GW` includes `FILE_CREATE_PIPE_INSTANCE` (the
  `FILE_APPEND_DATA` bit), so the plan's `GRGW` would let any logged-in user add an instance
  of `\\.\pipe\tunnelhop` and impersonate the service to the UI. Interactive users get
  `0x12019b` instead. Clients must therefore open the pipe with an explicit mask (read,
  write-data, write-attributes) rather than `GENERIC_WRITE`: `pipe::connect` does that, with
  `SECURITY_IDENTIFICATION` so a server can never act as its client. The owner (`OW`, the
  creating process) keeps full control so the service can add instances.
- **Pipe protocol (step 11):** `Hello` within 10 s or the connection is dropped; a request
  before `Hello`, a wrong version, a second `Hello`, an oversized or unparsable line all close
  or refuse as tested. `Ping` is answered by the pipe itself. The first instance uses
  `FILE_FLAG_FIRST_PIPE_INSTANCE`, so the service refuses to start if the name is taken.
  Remote clients are rejected.
- **Pipe limits (step 11 review):** at most 4 KB before `Hello`; 16 clients in all and 4 per
  Windows logon session; a client whose message takes over 30 s to write (it stopped reading)
  is dropped. There is no idle timeout: the UI waits for events for hours. All are fields of
  `pipe::Limits`, so tests can shorten them.
- **Clients verify the server (step 11 review):** `pipe::connect` refuses a pipe whose owner is
  not SYSTEM or Administrators. While the service is stopped any user could create the name,
  and the UI would otherwise send that impostor its credentials.
- **A failed pipe stops the service** with service-specific code 2, and `install` enables the
  restart actions for non-crash failures, so the SCM restarts it rather than reporting RUNNING.
- **Our tunnels (step 12):** `tunnel_name::is_ours` accepts exactly the generated shape, `th-`
  and 12 lowercase hex characters. The prefix alone would have claimed a user's `th-home`.
- **WireGuard endpoints (step 12):** the copy in `tunnels\` has each peer's `Endpoint`
  rewritten to the resolved address, peers in file order. A count mismatch is an error.
  `reconcile` reads the endpoints back from that copy and leaves `profile_id` empty; the
  orchestrator maps the tunnel name back to a profile.
- **Test servers in Docker Desktop, not a VM (step 12):** they run as containers on the PC,
  published only on addresses the PC holds on `th-lab`: the VPN servers on 192.168.77.1, the
  probe on 198.18.77.2. That saves a VM's RAM and an OS install; the kill-switch and every live
  test still run only in `th-test`. WireGuard is published on 41820/41821, because Windows
  reserves blocks of the dynamic port range (here 51786–51885) for Hyper-V and WinNAT.
- **One probe endpoint in the lab (step 12):** instead of one "what's my IP" per tunnel,
  `http://198.18.77.2:8080/` answers with the caller's address. Each server NATs its tunnel
  out through its own address and sends probe requests from its tunnel straight to the
  endpoint, so that one URL tells "no tunnel" (`172.31.77.1`, Docker's proxy) from each tunnel
  (`.11`, `.12`, `.13`), as a public service would. It is off the lab subnet so it is always
  routed through the tunnel: OpenVPN's `redirect-gateway` exempts the server's address, and
  an on-link address bypasses the tunnel and is blocked by WireGuard's own kill-switch.
- **Leak filter:** allows only the VPN ports on 192.168.77.1, which is also the VM's gateway.
- **Static C runtime (`.cargo/config.toml`):** Windows binaries link the CRT statically, so
  `th-service.exe` runs where the Visual C++ Redistributable is absent (a fresh Windows).
- **`secrets\` and uninstall:** Administrators may list and delete there but not read
  (`0x110081` on folders, `0x110080` on files), so an uninstall running as an administrator
  removes the store. The blobs stay DPAPI-sealed for SYSTEM regardless.
- **Tests that need SYSTEM** run through `scripts\vm.ps1 test <target> -AsSystem`.
- **The test VM is Windows 11 Home**, which most users run. PowerShell Direct and everything
  the tests need work on it.
- **Kill-switch filters (step 13):** a fixed provider and sublayer, the sublayer at the
  highest weight. Permits (weight 15): loopback, DHCPv4 and DHCPv6, ICMPv6 types 133–137, and
  each endpoint by address, port and protocol. Blocks (weight 0) on all four ALE layers carry
  `FWPM_FILTER_FLAG_CLEAR_ACTION_RIGHT`, so no other sublayer's permit overrides them.
  Interface permits are named per LUID and survive a re-engage. Every change is one
  transaction; `disengage` also removes the sublayer and provider.
- **Kill-switch and WireGuard's own (step 13, settled):** when a WireGuard config routes
  everything, WireGuard's own kill-switch is active too, and traffic flows only when both
  sublayers permit it: ours through the interface permit, WireGuard's through its tunnel. Our
  block is final and WireGuard's stays in force independently; neither weakens the other.
- **Dev safety timer (step 13):** the `dev-safety` feature lifts an engage not confirmed within
  120 s (`TH_DEV_SAFETY_SECONDS` in tests). Allowing a tunnel interface counts as confirmation.
  A dev build logs the marker on every command, which keeps the marker in the binary; CI fails
  if a release build contains it, or if a dev-safety build does not.
- **Live kill-switch tests refuse to run unless `TH_TEST_VM=1`**, which only `scripts\vm.ps1`
  sets, a second guard beside `#[ignore]` against running one on the PC.
- **Orchestrator (step 14):** importing a bundle does not activate it; `SelectBundle` does, and
  first disconnects and lifts the block of the bundle before. At start the service reopens the
  active bundle, adopts that bundle's leftover tunnel, removes any other `th-` tunnel, and keeps
  a block it finds (`Blocked`; Disconnect lifts it even with no bundle). STOP and PRESHUTDOWN
  disconnect and lift the block, bounded to 20 s. `probe_urls` is read at start.
- **The probe** follows no redirects, ignores system proxy settings, and treats an answer over
  64 bytes as no address.
- **Endpoint resolution:** one address per endpoint, IPv4 first, in config order.
- **What the leak test counts:** frames the VM *sends* on its physical adapter. pktmon captures
  before the firewall, so inbound packets appear whether or not they are let in; they are listed
  for information only.
- **Management client (step 15):** notifications that arrive while a command waits for its
  answer are queued, never dropped or mistaken for the answer. A port that does not ask for the
  management password is refused as not ours. An error from a command names only the command's
  first word, since the rest can be a credential. Credentials are double-quoted with `\` and `"`
  escaped; line breaks are already refused at the IPC layer.
- **OpenVPN driver (step 16):**
  - OpenVPN tunnels are never adopted after a restart: the job kills `openvpn.exe` with the
    service, so none can still be up. `reconcile` only clears what a killed one left: the
    host route to its server, any manually added routes on its adapter, and its files.
  - Routes on the adapter are cleared only while the adapter is down. One that is up again
    carries another OpenVPN, perhaps the user's own.
  - `--log <logs>\openvpn-<tunnel>.log` is added to SPEC §4.3's command line, so a failure
    can be read in OpenVPN's own words.
  - Only the `Auth` realm is answered; a private-key passphrase prompt fails the connect.

Still open:
1. **A code-signing certificate** for `th-service.exe` and the installer (SmartScreen,
   Defender). Until there is one, builds are unsigned.

## Progress
- **2026-10-06:** steps 1–9 merged (PR #1). The Proton allowlist merged (PR #2). Steps 24–25
  core merged (PRs #3, #4). 103 tests pass on Linux and Windows CI.
- **2026-10-07:** the Windows PC is set up (Rust 1.99, Build Tools, Hyper-V access without
  elevation). Step 10's code and step 26 landed on `feature/service-shell`; 130 tests pass
  on Windows. The `th-test` VM doesn't exist yet, so step 10's VM check is pending.
- **2026-10-07:** step 10 merged (PR #6, VM check still pending). Step 11 merged (PR #7).
- **2026-10-07:** the lab is up: `th-test` (Windows 11 Home, WireGuard 1.1.1, OpenVPN 2.7.7,
  checkpoint `clean`) and the test servers in Docker Desktop. Step 10's VM check and step 12's
  live test pass on `feature/wireguard-driver`; 163 tests pass on the PC.
- **2026-10-07:** step 12 merged (PR #8). Step 13, the kill-switch, landed on
  `feature/killswitch` and passes in `th-test`, `dev-safety` build included.
- **2026-10-07:** step 13 merged (PR #9). Step 14 landed on `feature/orchestrator`: TunnelHop
  rotates end to end in `th-test`, and the WireGuard leak test passes.
- **2026-10-07:** step 14 merged (PR #10). Step 15, the OpenVPN management client, landed on
  `feature/ovpn-management`, checked against transcripts and against the real OpenVPN in
  `th-test`.
- **2026-10-07:** step 15 merged (PR #11). Step 16, the OpenVPN driver, landed on
  `feature/openvpn-driver`: OpenVPN rotates end to end in `th-test`, and the mixed-protocol
  leak test passes. Also fixed: service stop (a step-14 bug).
- **Next:** step 17, the Tauri app shell.

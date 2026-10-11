# Testing TunnelHop

How each acceptance criterion in `PLAN.md` is checked, and what the last check found. Read
`DEVELOPMENT.md` first: it describes the PC, the `th-test` VM and the test servers.

**Everything that connects a tunnel or engages the kill-switch runs in `th-test`, never on the
PC.** Take a checkpoint first (`scripts\vm.ps1 checkpoint <name>`).

## 1. What runs where

| Check | Where | Command |
|---|---|---|
| Unit and integration tests | the PC (and Linux in CI for th-core) | `cargo test --workspace` |
| Formatting, lints, the app's types | the PC, before every push | `.githooks/pre-push` |
| Release build has no dev safety timer | CI (Windows), when service code changes | the `Release gate` workflow |
| Live tunnel, kill-switch and installer checks | `th-test` only | this document, sections 3 and 4 |

Ten tests are marked `#[ignore]`: they start real tunnels or block real traffic, and run only in
the VM (`scripts\vm.ps1 test <name>`, DEVELOPMENT.md §5).

## 2. Building what gets tested

```powershell
cd app; npm run installer; cd ..
# -> target\release\bundle\nsis\TunnelHop_<version>_x64-setup.exe
```

The installer carries the app (`th-app.exe`) and the service (`th-service.exe`, built first by
the same command with default features, so without the dev safety timer). It installs per
machine and asks for elevation; the app itself never runs elevated.

### Seeing the app in another language

The app follows Windows' display language. To see one without changing Windows, set
`TUNNELHOP_LANG` to a file name under `app\locales` (`uk`, `de`, `ja`, ...) for the user and
start the app again:

```powershell
setx TUNNELHOP_LANG uk        # then start the app; `setx TUNNELHOP_LANG ""` and
                              # `reg delete HKCU\Environment /v TUNNELHOP_LANG /f` undo it
```

Service messages stay English in every language (SPEC §5).

## 3. The installer pass (plan step 19)

From the `clean` checkpoint (Windows with WireGuard and OpenVPN, no TunnelHop):

```powershell
Restore-VMCheckpoint -VMName th-test -Name clean -Confirm:$false
# copy the installer and test-infra\out\* into the VM (C:\lab below), then, in the VM:
Start-Process C:\lab\setup.exe -ArgumentList '/S' -Wait        # or run it by hand
# the lab only: point the IP probe at the lab's endpoint, since every lab server
# shares the PC's real public address
Set-Content C:\ProgramData\TunnelHop\settings.json '{ "probe_urls": ["http://198.18.77.2:8080/"] }' -Encoding ASCII
Restart-Service TunnelHop
$ctl = 'C:\Program Files\TunnelHop\th-service.exe'
& $ctl ctl import C:\lab\bundle-all.zip      # prints the bundle id
& $ctl ctl select <bundle id>
& $ctl ctl connect; & $ctl ctl rotate
```

Then check, after each of **install**, **install again over it** (an upgrade) and **uninstall**:

| After | Expect |
|---|---|
| Install | `Get-Service TunnelHop` is Running, Automatic, LocalSystem, at `C:\Program Files\TunnelHop\th-service.exe run`; `ctl status` answers |
| Upgrade (installer run again while connected) | exit code 0; the service is Running; bundles, the active bundle and `settings.json` are kept; the tunnel is down and the block lifted until you connect again |
| Uninstall (`uninstall.exe /S`, or Settings → Apps) | `sc query TunnelHop` says the service is not installed; `C:\Program Files\TunnelHop` and `C:\ProgramData\TunnelHop` are gone; no `WireGuardTunnel$th-*` service and no `openvpn.exe`; `netsh wfp show filters` lists nothing named TunnelHop; the Start-menu and desktop shortcuts and the autostart entry are gone; the internet works |

The installer takes its language from Windows, in the app's eleven languages. To see another, the
VM needs that display language: `Install-Language uk-UA` (about half an hour),
`Set-WinUILanguageOverride -Language uk-UA`, then sign out and in. `Alt+Tab` brings the
installer to the front for the keyboard.

The WFP check, in the VM: `netsh wfp show filters file=f.xml; Select-String f.xml -Pattern TunnelHop -SimpleMatch`.

**Last run: 2026-10-10, in `th-test`, installer built from `feature/installer`.** All rows as
expected. Uninstall was run three ways, each from a connected tunnel:

- with the service running: clean;
- straight after killing `th-service.exe` (service stopped, one tunnel service and 29 filters
  left behind): clean. The uninstall starts the service so that it takes them over, then stops it;
- with the app open: clean, after a fix. The first attempt removed the service but left the app
  and its files; the installer now closes the app before touching the service.

Left after an uninstall, by Tauri's design unless "delete application data" is ticked:
`%LOCALAPPDATA%\app.tunnelhop` and `%APPDATA%\app.tunnelhop` (the app's log and a marker that
autostart was already offered). A silent uninstall never ticks it.

**Not checked in that run:** the installer's interactive pages (everything ran with `/S`),
launching the app from the Start menu (it was started by a scheduled task with a limited token,
DEVELOPMENT.md §5), and an upgrade from an older *version* (the same version was installed over
itself).

### The final pass on the PC

The last line of plan step 19, for a person to do, not a script: install on the PC, import a
real bundle, connect, rotate, Exit, uninstall. Before starting, keep an administrator prompt
open; if the internet is ever blocked with no way out through the app:

```
"C:\Program Files\TunnelHop\th-service.exe" disengage
```

removes every TunnelHop filter whatever state the service is in.

## 4. The acceptance criteria

Each criterion from `PLAN.md`, how to check it, and the last result. "Live" means in `th-test`
against the installed release build on 2026-10-10 unless another date is given.

| # | Criterion | How it is checked | Last result |
|---|---|---|---|
| 1 | `cargo test -p th-core` passes on Linux and Windows | CI job `th-core (platform-independent)`, on a Linux runner; on Windows by the pre-push hook on the PC. CI's Windows job has run only the release gate since 2026-10-10, to save minutes | Pass (CI on PR #16; hook on every push) |
| 2 | Hostile zips are refused and leave nothing behind | `crates/th-core/tests/bundle.rs` | Pass |
| 3 | Script, plugin and file directives are refused with file and line; so are WireGuard hooks | `crates/th-core/tests/sanitize.rs` | Pass |
| 4 | No plaintext `auth.txt` on disk; the store is unreadable by standard users | Live: import `bundle-authfile.zip`, search `%ProgramData%\TunnelHop` for `auth.txt` and for the password, `icacls` on `bundles`, `tunnels`, `secrets`, then connect | Pass: no `auth.txt`, no file holds the username or password, one sealed `.cred` file that even an administrator cannot read; `bundles` and `tunnels` grant SYSTEM and Administrators only, `secrets` grants Administrators list-and-delete only; the profile connected with the sealed credentials |
| 5 | The UI runs without elevation, with no UAC prompt | Live: read `TokenElevation` of `th-app.exe`; watch for prompts while connecting | Pass: `th-app.exe` not elevated (session 1), `th-service.exe` elevated (session 0). No UAC prompt appeared in any UI session of steps 17, 18, 27 and 28. The app was started by a limited scheduled task, not from the Start menu |
| 6 | Two bundles with the same config file name each connect under their own tunnel service | Live: import `bundle-wg.zip` and `bundle-all.zip` (both hold `wireguard/wg-a.conf`), connect `wg-a` in each | Pass: `WireGuardTunnel$th-59e9c3b5d28e` and `WireGuardTunnel$th-6ef80b5c5d91`; never two at once |
| 7 | 20 rotations leak nothing, across WireGuard and OpenVPN | Live: `scripts\leak-test.ps1 -Service 'C:\Program Files\TunnelHop\th-service.exe'` with `bundle-all.zip` active | Pass: 20 rotations over all three servers (7, 7 and 6 times), 11 032 frames captured, 0 sent by the VM outside the tunnel |
| 8 | The service killed mid-rotation: traffic stays blocked; after the restart it reconnects or offers "Disconnect & unblock"; nothing stray is left | Live: from WireGuard, `ctl connect ovpn` in the background, `Stop-Process th-service -Force` a moment later, then inspect before and after the automatic restart | Pass, two runs. Killed 2.2 s in, between the two tunnels: no WireGuard tunnel service, no `openvpn.exe` (it dies with the service), 26 filters in place, `1.1.1.1:443` unreachable; after the restart the state was `blocked` with the kill-switch on and still no traffic out, and Disconnect lifted it. Killed 0.7 s in, before the old tunnel was down: the 29 filters stayed, and the restarted service adopted the tunnel and reported Connected |
| 9 | The OpenVPN management port refuses a client without the password, and differs between connects | `crates/th-service/tests/ovpn_mgmt.rs` (`a_wrong_management_password_is_reported`, `a_port_that_does_not_ask_for_the_password_is_not_ours`); live: read the listener's port on two connects | Pass: ports 65151 then 65159. Live, a second client got no greeting at all while the service held the connection, so the wrong-password answer rests on the tests |
| 10 | A one-profile bundle rotates by reconnecting that profile | `crates/th-core/tests/rotation.rs` | Pass |
| 11 | When every candidate fails: `Blocked`, an alert, and no traffic leaves | Live: stop the three VPN containers on the PC, `ctl rotate`, read the state, try `1.1.1.1:443`, read `ctl watch` | Pass: state `blocked`, kill-switch on, `1.1.1.1:443` unreachable, and the alert "Every attempt failed; traffic is blocked." reached the pipe's clients. The tray's notification itself was not looked at in this run |
| 12 | A bare `auth-user-pass` profile imports as needing credentials, is never picked until they are set, then connects | `rotation.rs` (`profiles_missing_credentials_are_never_picked`), `orchestrator.rs`; live through the window in step 18 | Pass (step 18, 2026-10-10: sign-in prompt, saved, connected at 172.31.77.13) |
| 13 | A rotation that keeps the public IP tries another candidate; if none changes it, it stays connected and says so | `rotation.rs` (`a_rotation_that_keeps_the_ip_tries_another_candidate`, `when_no_candidate_changes_the_ip_it_stays_connected_and_says_so`) | Pass in tests. Not reproduced live: every lab server has its own address |
| 14 | An interval set by the user survives a service restart and overrides the bundle's | `orchestrator.rs` (`the_users_choices_and_the_bundle_survive_a_restart`, whose bundle sets 15); live: `ctl interval 30`, `Restart-Service TunnelHop`, `ctl status` | Pass: 30 after the restart (the lab bundle sets none, so the override itself rests on the test) |
| 15 | Exit and the uninstall both leave no tunnel services, no filters and working internet | Live: section 3 for the uninstall; Exit through the window | Pass: uninstall as in section 3; Exit in step 18 (disconnected, block lifted, no `openvpn.exe` left) |
| 16 | A runtime below the minimum shows as "too old" and its protocol is disabled | `crates/th-service/src/runtimes.rs` tests for the version rules; the window marks such servers *Unavailable* with the reason | Pass in tests. Not run live: it needs an old WireGuard or OpenVPN installed |
| 17 | The release build contains no dev safety timer | CI's `Release gate`; and `grep -c TUNNELHOP_DEV_SAFETY_TIMER target/release/th-service.exe` on the file the installer packs | Pass: CI on PR #16; 0 matches in the packed file |

## 5. Editing a bundle under the capture (plan step 28)

`scripts\leak-test.ps1 -Watch <seconds>` captures without rotating while you add or remove
servers by hand or through `ctl add` / `ctl remove`. Connect first. Expect 0 frames sent
outside the tunnel and 0 samples with the kill-switch off. Last run: 2026-10-10, two captures
(100 s through `ctl`, 75 s through the window), both 0 and 0.

## 6. What has no automated test

- The window's components and wiring (`app/src`): `npm test` covers the logic kept in plain
  modules (`install.ts`, `i18n.ts`, `ipc.ts`), and `actions.rs` what the controls send; the
  rest is checked by hand in the VM, by keyboard through `Msvm_Keyboard` (DEVELOPMENT.md §5).
- Anything that needs a mouse in the VM: drag-and-drop of bundles and config files, and closing
  a dialog by clicking outside it.
- The installer's hooks: checked by the pass in section 3. The service's own install and
  uninstall have `svc_live`, but not their error branches (a stop that times out, a start that
  fails).
- A management port taken by another process between the driver picking it and OpenVPN
  binding it: only `port_owner`'s unit test.
- `create` failing to make a new bundle active: only a disk failure between writing the
  bundle and reading it back reaches it.

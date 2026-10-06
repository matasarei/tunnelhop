# TunnelHop — Technical Specification

## 1. Summary

TunnelHop is a desktop manager and rotation orchestrator for **WireGuard** and **OpenVPN** on
**Windows 10/11**. It does not implement tunnels itself: it drives the installed native runtimes
(`wireguard.exe`, `openvpn.exe`) and adds what they lack — importing a bundle of profiles,
rotating between them on a schedule, and keeping traffic blocked by its own kill-switch while
it switches.

Configuration arrives as a single `.zip` bundle holding native configs and a manifest. Every
bundle is treated as **untrusted input**.

Supported runtimes: **WireGuard for Windows ≥ 0.5.3**, **OpenVPN ≥ 2.6.0**. Older versions are
reported in the UI and that protocol is disabled.

macOS and Linux are out of scope for v1. The core crate has no platform code, so a later port
replaces the service and drivers, not the logic.

---

## 2. Architecture

### 2.1 Stack

| Part | Choice |
|---|---|
| Core logic | Rust crate `th-core` — manifest, bundle import, config sanitizers, rotation engine, IPC types. Platform-independent. |
| Privileged service | Rust binary `th-service.exe`, a Windows service running as LocalSystem. |
| UI | Tauri v2 + Svelte 5, running **unelevated** as the logged-in user. |
| IPC | Named pipe `\\.\pipe\tunnelhop`, JSON lines, versioned protocol. |
| Installer | Tauri NSIS bundle with hooks that install and remove the service. |

### 2.2 Processes

```
┌────────────────────────────────────────┐
│ tunnelhop.exe  (user, unelevated)      │
│  Tray menu · Dashboard · Bundle import │
└──────────────────┬─────────────────────┘
                   │ \\.\pipe\tunnelhop  (SDDL: SYSTEM, Administrators, Interactive users)
┌──────────────────▼─────────────────────┐
│ th-service.exe  (LocalSystem)          │
│  Bundle store · Rotation engine        │
│  Kill-switch (WFP) · IP probe          │
│  ┌───────────────┐ ┌────────────────┐  │
│  │WireguardDriver│ │ OpenvpnDriver  │  │
│  └──────┬────────┘ └───────┬────────┘  │
└─────────┼──────────────────┼───────────┘
          ▼                  ▼
  WireGuardTunnel$th-…   openvpn.exe (Job Object child,
  (SCM service)          authenticated management socket)
```

- The UI never elevates and never needs to. All privileged work happens in the service.
- The UI sends bundle **bytes** over the pipe (≤ 20 MB). The service never opens a path chosen
  by the user, so a SYSTEM process cannot be tricked into reading a file the user cannot.
- The service owns every tunnel. A UI crash changes nothing; a service crash leaves the
  kill-switch in place (fail closed) and the next start reconciles.

### 2.3 Storage

| Path | Contents | Access |
|---|---|---|
| `%ProgramData%\TunnelHop\bundles\<bundle_id>\` | Extracted, sanitized bundle | SYSTEM, Administrators |
| `%ProgramData%\TunnelHop\tunnels\` | WireGuard configs under generated tunnel names | SYSTEM, Administrators |
| `%ProgramData%\TunnelHop\secrets\` | DPAPI blobs (OpenVPN credentials), short-lived management passwords | SYSTEM |
| `%ProgramData%\TunnelHop\settings.json` | Active bundle, selected profile, user overrides | SYSTEM, Administrators |
| `%ProgramData%\TunnelHop\logs\` | Service logs | SYSTEM, Administrators |
| `%LocalAppData%\TunnelHop\` | UI-only preferences (window, theme) | user |

---

## 3. Configuration bundle

### 3.1 Layout

```text
bundle.zip
├── manifest.json
├── wireguard/
│   ├── ua-kyiv-01.conf
│   └── ua-kyiv-02.conf
└── openvpn/
    ├── ua-kyiv-01.ovpn
    └── auth.txt          # optional, referenced by `auth-user-pass auth.txt`
```

Only `manifest.json`, `wireguard/**` and `openvpn/**` are accepted.

### 3.2 Manifest

Schema: [`manifest.schema.json`](manifest.schema.json).

```json
{
  "schema_version": "1",
  "bundle_name": "Ukraine Server Cluster",
  "protocols": ["wireguard", "openvpn"],
  "rotation": {
    "enabled": true,
    "strategy": "random",
    "interval_minutes": 15,
    "on_disconnect": "reconnect_random",
    "reconnect_grace_seconds": 20
  },
  "profiles": [
    { "id": "ua-wg-01",   "name": "Kyiv #1 (WG)",      "protocol": "wireguard", "file": "wireguard/ua-kyiv-01.conf" },
    { "id": "ua-wg-02",   "name": "Kyiv #2 (WG)",      "protocol": "wireguard", "file": "wireguard/ua-kyiv-02.conf" },
    { "id": "ua-ovpn-01", "name": "Kyiv #1 (OpenVPN)", "protocol": "openvpn",   "file": "openvpn/ua-kyiv-01.ovpn" }
  ]
}
```

| Field | Rule |
|---|---|
| `schema_version` | Must be `"1"`. |
| `bundle_name` | 1–100 characters. |
| `protocols` | Optional; the protocols rotation may pick from. Default: every protocol a profile uses. Must not be empty. |
| `rotation` | Optional; every field has a default. |
| `rotation.enabled` | Default `true`. |
| `rotation.strategy` | `"random"` (the only value in v1). |
| `rotation.interval_minutes` | 1–1440, default 15. |
| `rotation.on_disconnect` | `"reconnect_same"`, `"reconnect_random"` (default) or `"stay_down"`. |
| `rotation.reconnect_grace_seconds` | 5–300, default 20. How long a tunnel may stay in a reconnecting state before it counts as down. |
| `profiles` | 1–500 entries. |
| `profiles[].id` | `^[a-z0-9-]{1,40}$`, unique within the bundle. |
| `profiles[].name` | 1–100 characters. |
| `profiles[].protocol` | `"wireguard"` or `"openvpn"`. |
| `profiles[].file` | Relative path, `/`-separated, under `wireguard/` for WireGuard and `openvpn/` for OpenVPN; must exist in the archive. |

Unknown fields anywhere are an error. `bundle_id` is not part of the manifest: it is the first
16 hex characters of the SHA-256 of the zip bytes.

### 3.3 Import

1. **Archive limits.** ≤ 20 MB compressed, ≤ 500 entries, ≤ 50 MB uncompressed, each entry's
   compression ratio ≤ 100.
2. **Entry names.** Rejected: absolute paths, drive letters, `..` segments, backslashes,
   empty segments, symlinks, names outside the allowed top-level entries, and names that
   collide after case-folding (Windows file systems are case-insensitive).
3. **Staging.** Extract to `bundles\.staging-<random>\`, validate and sanitize everything, then
   rename to `bundles\<bundle_id>\`. On any error the staging directory is deleted and the user
   sees every error found, each with its file and line.
4. **Re-import** of the same zip is a no-op that returns the existing `bundle_id`.

### 3.4 Config sanitizing

Configs run with LocalSystem privileges, so anything in them that can execute code or touch
files is refused.

**OpenVPN** — an allowlist. A directive not on it is an error with its line number. The
tokenizer follows OpenVPN's own quoting and backslash rules, so `"up" "cmd"` is recognised as
`up`.

Allowed: `client`, `dev`, `dev-type`, `proto`, `remote`, `remote-random`, `resolv-retry`,
`nobind`, `persist-key`, `persist-tun`, `remote-cert-tls`, `cipher`, `data-ciphers`,
`data-ciphers-fallback`, `auth`, `tls-version-min`, `tls-cipher`, `tls-ciphersuites`, `tls-auth`
(inline only), `tls-crypt` (inline only), `tls-crypt-v2` (inline only), `key-direction`, `verb`,
`mute`, `auth-user-pass`, `auth-nocache`, `allow-compression`, `comp-lzo` (only `no`),
`pull-filter`, `redirect-gateway`, `block-ipv6`, `ping`, `ping-restart`, `connect-retry`,
`connect-retry-max`, `server-poll-timeout`, `float`, `mssfix`, `tun-mtu`, `fragment`, `sndbuf`,
`rcvbuf`, `verify-x509-name`, `explicit-exit-notify`, `port`, `lport`, `windows-driver`,
`reneg-sec` (one number), `setenv opt block-outside-dns` and `setenv CLIENT_CERT 0` (those exact
forms only), `block-outside-dns`.

Inline blocks allowed: `<ca>`, `<cert>`, `<key>`, `<tls-auth>`, `<tls-crypt>`, `<tls-crypt-v2>`.
Any other inline block is an error.

`auth-user-pass <file>` must name a file inside the bundle, resolved relative to the config's
own directory. At import its two lines (username, password) are moved into a DPAPI secret, the
plaintext file is deleted, and the directive is rewritten to bare `auth-user-pass`; credentials
are then supplied over the management interface.

Each config must contain at least one `remote`.

**WireGuard** — `[Interface]` and `[Peer]` sections only. `PreUp`, `PostUp`, `PreDown`,
`PostDown` and `Table` are errors. Each `[Peer]` needs an `Endpoint`.

**Endpoints** of every profile (`Endpoint` for WireGuard; `remote host [port] [proto]` for
OpenVPN, with `port` and `proto` defaults applied) are extracted at import for the kill-switch.

---

## 4. Behaviour

### 4.1 Driver interface

```rust
#[async_trait]
pub trait VpnDriver: Send + Sync {
    async fn connect(&self, profile: &ResolvedProfile) -> Result<TunnelHandle, DriverError>;
    /// Returns only after teardown is confirmed.
    async fn disconnect(&self, handle: &TunnelHandle) -> Result<(), DriverError>;
    async fn status(&self, handle: &TunnelHandle) -> Result<TunnelStatus, DriverError>;
    fn events(&self) -> broadcast::Receiver<TunnelEvent>;
    /// Find tunnels left by a previous run of the service.
    async fn reconcile(&self) -> Result<Vec<TunnelHandle>, DriverError>;
}
```

`ResolvedProfile` carries the absolute config path, the generated tunnel name and the
endpoints resolved to IP addresses.

### 4.2 WireGuard driver

- `wireguard.exe` is found under `%ProgramFiles%\WireGuard\`; its version comes from the file's
  version resource.
- **Tunnel name:** `th-` followed by the first 12 hex characters of
  SHA-256(`bundle_id` + `/` + `profile_id`). WireGuard derives the service name from the file
  name and requires `^[a-zA-Z0-9_=+.-]{1,32}$`; a generated name also keeps two bundles that both
  ship `ua-kyiv-01.conf` from colliding. The config is copied to `tunnels\<name>.conf` with
  `Endpoint` rewritten to the resolved IP.
- **Connect:** `wireguard.exe /installtunnelservice <absolute path>`, then poll the service
  `WireGuardTunnel$<name>` until `SERVICE_RUNNING` (15 s timeout).
- **Disconnect:** `wireguard.exe /uninstalltunnelservice <name>`, then poll until the service no
  longer exists (15 s timeout).
- **Reconcile:** enumerate services matching `WireGuardTunnel$th-*`. Tunnels without the `th-`
  prefix belong to the user and are never touched.
- `HKLM\Software\WireGuard\DangerousScriptExecution` is never set.

### 4.3 OpenVPN driver

- `openvpn.exe` is found via `HKLM\SOFTWARE\OpenVPN\exe_path`, falling back to
  `%ProgramFiles%\OpenVPN\bin\`. `openvpn --version` must report ≥ 2.6.0.
- **Connect:** spawn, suspended, inside a Job Object with `KILL_ON_JOB_CLOSE`:

  ```
  openvpn.exe --config <abs> --management 127.0.0.1 <random free port> <password file>
              --management-hold --management-query-passwords --auth-nocache
              --connect-retry-max 3
  ```

  The password file holds 32 random bytes (hex), lives in `secrets\`, and is deleted once the
  management client has authenticated. The client then sends `state on` and `hold release`,
  answers `>PASSWORD:Need 'Auth'` from the stored credentials, and maps `>STATE:` lines:
  `CONNECTED` → up, `RECONNECTING` → reconnecting, `EXITING` → down.
- **Disconnect:** `signal SIGTERM` over management; if the process has not exited after 10 s,
  terminate the job.

### 4.4 Kill-switch

The service keeps its own WFP provider and sublayer. All filters are persistent, so they
survive a service crash.

- **Engage(endpoints):** permit loopback, DHCPv4 (UDP 67/68), DHCPv6 (UDP 546/547), ICMPv6
  neighbour discovery (types 133–137) and each VPN endpoint (IP, port, protocol); block all other
  IPv4 and IPv6 traffic, LAN included.
- **Allow interface(LUID):** permit traffic on a tunnel interface. **Revoke** removes it.
- **Disengage:** remove every filter in the sublayer.
- **On service start:** if filters exist and no tunnel is up, they stay and the state is
  `Blocked`. The user can choose "Disconnect & unblock".

### 4.5 Rotation

Triggered by the interval timer, by "Rotate Now", or by the current tunnel being down (or
reconnecting longer than `reconnect_grace_seconds`) when `on_disconnect` is not `stay_down`.

1. **Candidates:** profiles whose protocol is in `protocols` and whose runtime is available,
   minus the current profile. If none are left, reconnect the current profile.
   `reconnect_same` always picks the current profile.
2. Pick one uniformly at random.
3. Resolve its endpoints while the old tunnel is still up.
4. Engage the kill-switch with the old and new endpoints; revoke the old interface; disconnect
   the old tunnel.
5. Connect the new tunnel; allow its interface.
6. **Probe** the public IP: `https://api.ipify.org`, then `https://ipinfo.io/ip`, 5 s timeout
   each. The result must parse as an IP and differ from the ISP address (recorded once, before
   the first connection).
7. If connect or probe fails, try another candidate — at most 3 attempts in total. After that
   the state is `Blocked`: the kill-switch stays engaged and the UI shows an alert.
8. Publish the new status (profile, IP, state, kill-switch) to the UI.

The user's choices in the tray — auto-rotation on/off and the interval — are stored in
`settings.json` and take precedence over the manifest's defaults.

---

## 5. UI

- **Window:** closing it hides it to the tray.
- **Dashboard:** bundle, profile, protocol, public IP, state, kill-switch state; bundle import
  by drag-and-drop or file picker, with the list of import errors.
- **Tray menu:**
  - Status: `Connected: <profile> (<IP>)`, `Blocked`, or `Disconnected`
  - Rotate Now
  - Auto-Rotation (checkbox)
  - Interval ▸ 5 min · 15 min · 30 min · 1 hour
  - Disconnect & unblock
  - Open Dashboard
  - Exit — disconnects, disengages the kill-switch, and quits the UI
- **Autostart:** per-user `Run` key; possible because the UI is not elevated.
- Runtimes below the minimum version are shown as "runtime too old" and their profiles are
  disabled.

---

## 6. Lifecycle and security

- **Service stop or system shutdown** (`SERVICE_CONTROL_STOP`, `SERVICE_CONTROL_PRESHUTDOWN`):
  disconnect, disengage, exit.
- **Service crash:** the kill-switch stays (fail closed); the next start reconciles WireGuard
  tunnel services and the OpenVPN job.
- **Uninstall:** disconnect, disengage, remove the service and `%ProgramData%\TunnelHop`.
- **Pipe access** is limited by its security descriptor; every request is validated and size
  limited.
- **No code from configs is ever executed** (§3.4).

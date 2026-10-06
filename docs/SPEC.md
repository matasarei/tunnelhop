# Technical Specification: Desktop VPN Orchestrator Client

## 1. Executive Summary

A lightweight desktop application acting as a high-level manager and automation orchestrator for **WireGuard** and **OpenVPN**. Target platform is **Windows 10/11** as primary, designed with an abstraction layer for future **macOS** and **Linux** support.

The application does not implement low-level cryptographic tunnel drivers in-process. Instead, it delegates all tunnel lifecycles to installed native VPN runtimes via their official **Command-Line Interfaces (CLI)** and management sockets. Configuration is distributed and imported via a single `.zip` archive containing native configuration files and an orchestration manifest.

---

## 2. Architecture & Design Principles

### 2.1 Technology Stack Recommendations

* **Framework:** Tauri v2 (Rust backend + Svelte / Vue / Vanilla TypeScript frontend) or Go with Wails.
* **Storage:** Local file system (`%LocalAppData%\VpnOrchestrator\` on Windows).
* **Communication:** IPC between UI and backend; TCP socket for OpenVPN management; CLI/Service API for WireGuard.

### 2.2 Architectural Layers

```
┌────────────────────────────────────────────────────────┐
│ UI Layer (Frontend: WebView / System Tray)             │
│ - Dashboard: active profile, public IP, ping, status   │
│ - Tray context menu: quick switch, pause, rotate now   │
│ - Bundle import: drag-and-drop .zip ingestion          │
└──────────────────────────┬─────────────────────────────┘
                           │ IPC Commands
┌──────────────────────────▼─────────────────────────────┐
│ Application Core (Rust / Go Orchestrator)              │
│ - Manifest parser & profile store                      │
│ - Rotation scheduler & randomizer engine               │
│ - IP & connectivity verifier                           │
└──────────────────────────┬─────────────────────────────┘
                           │ VpnDriver Abstraction (Trait / Interface)
          ┌────────────────┴────────────────┐
          ▼                                 ▼
┌──────────────────┐              ┌──────────────────┐
│ WireguardDriver  │              │  OpenvpnDriver   │
│ (CLI / Services) │              │ (CLI + Mgmt TCP) │
└──────────────────┘              └──────────────────┘

```

---

## 3. Configuration Bundle Structure

Configurations are delivered via a single `.zip` archive holding native configs and a central manifest.

### 3.1 Directory Layout Inside Archive

```text
vpn_bundle.zip
├── manifest.json            # Orchestrator settings and profile catalog
├── wireguard/
│   ├── ua-kyiv-01.conf     # Standard WireGuard config
│   ├── ua-kyiv-02.conf
│   └── ...
└── openvpn/
    ├── ua-kyiv-01.ovpn     # Standard OpenVPN config
    ├── ua-kyiv-02.ovpn
    └── auth.txt            # Optional credentials referenced by .ovpn

```

### 3.2 Manifest Schema (`manifest.json`)

```json
{
  "version": "1.0",
  "bundle_name": "Ukraine Server Cluster",
  "default_protocol": "wireguard",
  "rotation": {
    "enabled": true,
    "strategy": "random",
    "interval_minutes": 15,
    "on_disconnect": "reconnect_random",
    "cooldown_seconds": 2
  },
  "profiles": [
    {
      "id": "ua-wg-01",
      "name": "Kyiv #1 (WG)",
      "protocol": "wireguard",
      "file": "wireguard/ua-kyiv-01.conf"
    },
    {
      "id": "ua-wg-02",
      "name": "Kyiv #2 (WG)",
      "protocol": "wireguard",
      "file": "wireguard/ua-kyiv-02.conf"
    },
    {
      "id": "ua-ovpn-01",
      "name": "Kyiv #1 (OpenVPN)",
      "protocol": "openvpn",
      "file": "openvpn/ua-kyiv-01.ovpn"
    }
  ]
}

```

---

## 4. Functional Specifications

### 4.1 Bundle Ingestion

1. Accept `.zip` archive via drag-and-drop or file picker.
2. Unpack contents into `%LocalAppData%\VpnOrchestrator\bundles\<bundle_id>\`.
3. Validate `manifest.json` against schema.
4. Verify that each file path listed in `profiles` exists on disk.
5. Store parsed state in application memory and persist current selection in settings.

### 4.2 Driver Abstraction Layer (`VpnDriver`)

All platform-specific actions must be implemented behind a unified interface:

```rust
pub trait VpnDriver: Send + Sync {
    fn connect(&self, profile_path: &Path) -> Result<(), DriverError>;
    fn disconnect(&self, profile_name: &str) -> Result<(), DriverError>;
    fn status(&self) -> Result<TunnelStatus, DriverError>;
}

```

#### WireGuard Windows Implementation:

* **Connect:**
```cmd
wireguard.exe /installtunnelservice "<Absolute_Path>\profile.conf"

```


Poll Windows Service status `WireGuardTunnel$<TunnelName>` until status is `SERVICE_RUNNING`.
* **Disconnect:**
```cmd
wireguard.exe /uninstalltunnelservice "<TunnelName>"

```


Wait until the service entry is unregistered.

#### OpenVPN Windows Implementation:

* **Connect:**
Spawn `openvpn.exe` as a supervised child process:
```cmd
openvpn.exe --config "<Absolute_Path>\profile.ovpn" --management 127.0.0.1 7505 --management-query-passwords

```


Connect a TCP client to `127.0.0.1:7505` to read state events (`CONNECTED`, `ASSIGN_IP`, `RECONNECTING`).
* **Disconnect:**
Send command `signal SIGTERM` over the management TCP connection or gracefully terminate child process.

### 4.3 Randomized Rotation Engine

* **Execution Flow:**
1. Trigger fired by timer or user action ("Rotate Now").
2. Select candidate profile randomly from the active bundle:

$$\text{Candidate} \in \text{Profiles} \setminus \{\text{CurrentActive}\}$$


3. Invoke `disconnect()` on current active profile.
4. Wait for `cooldown_seconds` (default: 2s) to allow OS route table flush.
5. Invoke `connect()` with the newly selected profile.
6. Execute an external HTTPS probe (`[https://api.ipify.org](https://api.ipify.org)` or `[https://ipinfo.io/json](https://ipinfo.io/json)`) to verify new public IP.
7. Update UI and tray tooltip with active profile name and new external IP.



---

## 5. UI & System Tray Requirements

* **Window Minimization:** Closing the window minimizes the application to the system notification area (tray).
* **Tray Context Menu:**
* **Status:** Displays `Connected: [Profile Name] ([IP])` or `Disconnected`.
* **Rotate Now:** Immediately switches to a random profile.
* **Auto-Rotation:** Toggle checkbox (Enabled/Disabled).
* **Interval:** Submenu to select rotation period (5 min, 15 min, 30 min, 1 hour).
* **Open Dashboard:** Restores main window.
* **Exit:** Cleanly terminates any running tunnel before process shutdown.



---

## 6. Permissions & Security Constraints

* **Privileges:** Network adapter configuration and service registration require Administrator rights on Windows. The app must ship with an application manifest requesting `requireAdministrator` or invoke a dedicated elevated helper process.
* **Leak Prevention:** The client must verify that DNS configuration provided in native configs is strictly enforced by the native runtime.
* **Clean Exit Guarantee:** Register process termination hooks (`SIGINT`, `SIGTERM`, `ExitProcess`) to ensure `/uninstalltunnelservice` or `signal SIGTERM` is executed so no orphaned routing rules remain.

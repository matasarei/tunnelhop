# Developing TunnelHop on Windows

This covers the Windows phase of [the plan](PLAN.md) (steps 10–19): the machine, the test VMs,
how builds get into the test VM, and the rules that keep a kill-switch bug from taking the
development machine offline.

## The setup, in one picture

```
Windows 11 Pro PC (x64)                      ← you and Claude Code work here; never firewalled
├── repo, Rust, Node, Claude Code
├── Docker Desktop  test-infra/ containers  ← WireGuard + OpenVPN test servers, "what's my IP" endpoint
├── Hyper-V
│   └── th-test     Windows 11 x64 VM        ← service, drivers, kill-switch and leak tests run here
│                   WireGuard + OpenVPN installed; checkpoint "clean"
└── th-lab          internal switch + NAT, 192.168.77.0/24; the PC is .1 (servers), plus 198.18.77.2 (probe)
```

**Why a VM, when the PC is already Windows?** The kill-switch blocks all traffic with WFP
filters that survive crashes and reboots on purpose. A bug there takes the machine it runs on
offline, and that includes Claude Code's connection to its API. Inside `th-test` that costs
nothing: Claude keeps control through **PowerShell Direct**, which talks to the VM over Hyper-V
itself rather than the network. So it can still run `th-service disengage`, read logs, or
restore a checkpoint while the VM's network is fully blocked.

**What runs where:**

| Work | Where |
|---|---|
| Editing, `cargo build`, unit tests, clippy | the PC |
| Installing the service, the pipe, WireGuard connect/disconnect (steps 10–12) | `th-test`; the PC is tolerable but avoid it |
| Kill-switch, leak tests, OpenVPN live tests (steps 13–16) | **`th-test` only** |
| UI and installer (steps 17–19) | `th-test` from the clean checkpoint, then one final pass on the PC |
| VPN test servers | Docker Desktop on the PC, reachable from `th-test` only (§4) |

## Requirements

- **Windows 11 Pro, Enterprise or Education.** Hyper-V isn't available on Home. If the PC is
  Home, see [If the PC is Windows Home](#if-the-pc-is-windows-home) before starting.
- **RAM:** 16 GB or more (6 GB for `th-test`, about 1 GB for the test server containers).
- **Disk:** about 100 GB free.
- **CPU virtualization** enabled in the firmware (VT-x/AMD-V). It usually is already.

## 1. The PC

In an **administrator** PowerShell:

```powershell
winget install --id Git.Git -e
winget install --id Rustlang.Rustup -e
winget install --id Microsoft.VisualStudio.2022.BuildTools -e --override "--quiet --wait --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
winget install --id OpenJS.NodeJS.LTS -e          # needed from step 17
winget install --id WiresharkFoundation.Wireshark -e   # reads leak-test captures
winget install --id Docker.DockerDesktop -e       # runs the test servers (WSL2 backend)
Enable-WindowsOptionalFeature -Online -FeatureName Microsoft-Hyper-V -All   # then reboot
```

After the reboot, in a normal PowerShell:

```powershell
git clone git@github.com:matasarei/tunnelhop.git
cd tunnelhop
git config core.hooksPath .githooks   # the full check before every push
cd app; npm ci; npm run build; cd ..  # the app embeds its built frontend
cargo test --workspace     # rustup installs the pinned toolchain (rust-toolchain.toml) on first use
```

Every test should pass. (Windows runs one fewer than Linux: a symlink test in `th-core` is
Unix-only.) Then install Claude Code and the `gku` plugin, and open it in the repo.

**Checks run locally, not in CI.** `.githooks/pre-push` runs fmt, the app's type check, clippy
and every test before each push; it takes about a minute, against ~16 minutes for a cold
Windows CI build billed at twice the Linux rate. CI keeps only what a developer's machine
cannot vouch for: `th-core` on Linux (about a minute) and, when service code changes, the
Windows release gate (the release binary must not contain the dev safety timer). Both have
hard timeouts (5 and 10 minutes), and a new push cancels the run of the previous one. A
version tag also builds the installer for a draft release ("Cutting a release" below).

If the Windows display language isn't English, local group names are translated. Refer to
groups by SID instead (`S-1-5-32-578` is Hyper-V Administrators), and add your account to that
group so Claude Code can drive the VMs without running elevated:

```powershell
Add-LocalGroupMember -SID S-1-5-32-578 -Member "$env:USERDOMAIN\$env:USERNAME"   # then sign out and in
```

## 2. The lab network

A private switch with NAT, so the VM keeps the same address across reboots (Hyper-V's
"Default Switch" changes its subnet). Administrator PowerShell:

```powershell
New-VMSwitch -Name th-lab -SwitchType Internal
New-NetIPAddress -IPAddress 192.168.77.1 -PrefixLength 24 -InterfaceAlias "vEthernet (th-lab)"
New-NetIPAddress -IPAddress 198.18.77.2 -PrefixLength 32 -InterfaceAlias "vEthernet (th-lab)"
New-NetNat -Name th-lab -InternalIPInterfaceAddressPrefix 192.168.77.0/24
# Let th-test, and only the lab subnet, reach the test servers and the probe.
New-NetFirewallRule -DisplayName "TunnelHop test servers (lab only)" -Direction Inbound `
    -Action Allow -Protocol UDP -LocalPort 41820,41821,1194 -RemoteAddress 192.168.77.0/24
New-NetFirewallRule -DisplayName "TunnelHop probe (lab only)" -Direction Inbound `
    -Action Allow -Protocol TCP -LocalPort 8080 -RemoteAddress 192.168.77.0/24
```

| Address | Host |
|---|---|
| 192.168.77.1 | the PC: gateway, NAT to the internet, and the VPN test servers |
| 198.18.77.2 | the PC: the "what's my IP" probe, deliberately off the lab subnet |
| 192.168.77.20 | `th-test` |

The switch has no DHCP, so the VM gets a static address, gateway 192.168.77.1, and DNS
1.1.1.1.

## 3. The test VM (`th-test`)

1. Download a Windows 11 x64 ISO. Any edition works; Home is a good choice, since most users
   run it (the VM used so far is Home). Keep ISOs out of git: `*.iso` in `.git/info/exclude`.
2. Create the VM, at first with no network, which lets Setup create a local account. Being in
   Hyper-V Administrators is enough, no elevation needed; adjust the ISO path:

   ```powershell
   New-VM -Name th-test -Generation 2 -MemoryStartupBytes 4GB -NoVHD -Path C:\Hyper-V
   New-VHD -Path C:\Hyper-V\th-test\th-test.vhdx -SizeBytes 64GB -Dynamic
   Add-VMHardDiskDrive -VMName th-test -Path C:\Hyper-V\th-test\th-test.vhdx
   # Dynamic memory: Windows Setup needs 4 GB to start; 6 GB at most leaves room for the PC.
   Set-VMMemory -VMName th-test -DynamicMemoryEnabled $true -StartupBytes 4GB -MinimumBytes 2GB -MaximumBytes 6GB
   Set-VMProcessor -VMName th-test -Count 4
   Set-VMKeyProtector -VMName th-test -NewLocalKeyProtector
   Enable-VMTPM -VMName th-test                      # Windows 11 requires a TPM
   Add-VMDvdDrive -VMName th-test -Path C:\path\to\Win11.iso
   Set-VMFirmware -VMName th-test -FirstBootDevice (Get-VMDvdDrive -VMName th-test)
   Set-VM -Name th-test -CheckpointType Standard -AutomaticCheckpointsEnabled $false
   vmconnect localhost th-test; Start-VM th-test
   ```

   The DVD only boots if a key is pressed at "Press any key to boot from CD or DVD", within
   about 5 seconds. If the window misses it, send the key through Hyper-V while restarting:
   `Msvm_Keyboard.TypeKey(0x20)` on the VM in `root\virtualization\v2`.
3. Install Windows with a **local administrator** account. Home insists on a network during
   setup: at that screen press **Shift+F10** and run `start ms-cxh:localonly`.
4. Back on the PC, store the VM's credential once. It's encrypted for your Windows user and
   kept outside the repo:

   ```powershell
   New-Item -ItemType Directory "$env:USERPROFILE\.tunnelhop" -Force | Out-Null
   Get-Credential | Export-Clixml "$env:USERPROFILE\.tunnelhop\th-test.cred"
   ```
5. Remove the DVD, connect the VM to `th-lab` and give it its address, all over PowerShell Direct:

   ```powershell
   Get-VMDvdDrive -VMName th-test | Remove-VMDvdDrive
   Connect-VMNetworkAdapter -VMName th-test -SwitchName th-lab
   $cred = Import-Clixml "$env:USERPROFILE\.tunnelhop\th-test.cred"
   Invoke-Command -VMName th-test -Credential $cred {
       $nic = Get-NetAdapter -Physical | Select-Object -First 1
       New-NetIPAddress -InterfaceIndex $nic.ifIndex -IPAddress 192.168.77.20 -PrefixLength 24 -DefaultGateway 192.168.77.1
       # Some networks drop plain DNS to 1.1.1.1; 8.8.8.8 first.
       Set-DnsClientServerAddress -InterfaceIndex $nic.ifIndex -ServerAddresses 8.8.8.8, 1.1.1.1
       Rename-Computer -NewName th-test -Force
   }
   Restart-VM th-test -Force
   ```
6. Install WireGuard and OpenVPN in the VM from their signed MSIs. winget can't initialise its
   sources in a PowerShell Direct session, so download the installers inside the VM, check
   `Get-AuthenticodeSignature` (WireGuard LLC, OpenVPN Inc.), and run `msiexec /i … /qn`. The
   first `scripts\vm.ps1` session creates `C:\th` and locks it to SYSTEM and Administrators.
7. Take the clean checkpoint: `scripts\vm.ps1 checkpoint clean`.

## 4. The test servers (Docker Desktop on the PC)

The servers are containers, not another VM: Docker Desktop's WSL2 kernel has WireGuard and TUN,
and running them on the PC saves a VM's RAM and an OS install. They publish their ports on the
PC's lab addresses only (192.168.77.1 and 198.18.77.2), so they are reachable from `th-test` and from
nothing else. They hold no kill-switch and touch none of the PC's own networking. In
`test-infra/` (see its [README](../test-infra/README.md)):

```sh
docker compose run --rm gen     # keys, server configs (data/), client configs (out/)
docker compose up -d --build    # needs th-lab (§2)
```

They provide:

- **two WireGuard servers and one OpenVPN server**, each with its own tunnel subnet and each
  NATing out through its own address, so a rotation between them really changes the address
  the outside sees;
- **one "what's my IP" endpoint, `http://198.18.77.2:8080/`**, answering with the caller's
  source address;
- **a generator** that writes fresh keys and matching client configs into `test-infra/out/`
  (gitignored), to be copied into `th-test` and zipped into test bundles.

**Why the endpoint matters.** The VM reaches the internet through the PC's NAT, so any public
"what's my IP" service would see the same address with or without the tunnel. The engine's
check that the IP changed (and differs from the ISP address) would then always fail. In
tests the service's probe URLs point at this endpoint instead. With no tunnel it sees Docker's
port proxy (`172.31.77.1`), and through each tunnel it sees that server's own address
(`172.31.77.11`, `.12`, `.13`). Step 14 makes the probe URLs configurable for exactly this.

**Why the probe has an address of its own, off the lab subnet.** It must reach the probe
through whichever tunnel is up, and two things would route it around the tunnel instead:
OpenVPN's `redirect-gateway` exempts the server's own address (so not 192.168.77.1), and any
address on the lab subnet is on-link, sent straight over the switch, which WireGuard's own
kill-switch then blocks (so not 192.168.77.x at all). 198.18.77.2 comes from 198.18.0.0/15,
reserved for network testing and never routed on the internet. The VM reaches it through its
gateway, which is the PC holding it.

If Docker Desktop is not an option, a small Linux VM on `th-lab` running the same compose file
works too: give it an address, then set `SERVER_ADDRESS` and `PROBE_ADDRESS` to it (the probe
then needs a second address on that VM for the same reason).

## 5. Day-to-day workflow

The PC builds; `th-test` runs. A session in PowerShell on the PC:

```powershell
$cred = Import-Clixml "$env:USERPROFILE\.tunnelhop\th-test.cred"
$vm   = New-PSSession -VMName th-test -Credential $cred

cargo build -p th-service --release
Copy-Item -ToSession $vm target\release\th-service.exe -Destination C:\th\ -Force

# Live tests: build the test executable on the PC, copy it, run it in the VM.
cargo test -p th-service --test wg_live --no-run      # prints the path of the test .exe
Copy-Item -ToSession $vm <that path> -Destination C:\th\wg_live.exe -Force
Invoke-Command -Session $vm { C:\th\wg_live.exe --ignored --nocapture }
```

`scripts/vm.ps1` wraps this so it's one command each. It also creates `C:\th` in the VM and
locks it to SYSTEM and Administrators:

```powershell
scripts\vm.ps1 checkpoint before-wg                 # before every risky run
scripts\vm.ps1 copy                                 # release th-service.exe to C:\th
scripts\vm.ps1 configs <path to test-infra\out>     # client configs to C:\th\configs
scripts\vm.ps1 test wg_live -Env @{ TH_WG_CONFIG = 'C:\th\configs\wg-a.conf' }
scripts\vm.ps1 test lib -AsSystem                   # the crate's own VM-only tests, as SYSTEM
scripts\vm.ps1 checkpoint before-killswitch         # always, before the next one
scripts\vm.ps1 test killswitch_live -Env @{ TH_WG_CONFIG = 'C:\th\configs\wg-a.conf' }
scripts\vm.ps1 test killswitch_live -Features dev-safety -Env @{ TH_DEV_SAFETY_SECONDS = '3' }
scripts\vm.ps1 run 'C:\th\th-service.exe disengage'
scripts\vm.ps1 restore before-wg
scripts\vm.ps1 test ovpn_live                       # the OpenVPN driver (step 16), about 1 min
scripts\vm.ps1 test svc_live                        # install, crash, uninstall; TunnelHop not installed
```

- **Take the checkpoint after `configs`**, not before: restoring it otherwise brings back the
  previous configs, and after `gen.sh` has run again their certificates no longer match the
  server. The failure is only a TLS timeout.
- **`copy` fails while the service runs**: the service runs straight from `C:\th\th-service.exe`
  and holds it open. Stopping is not enough, because its recovery actions may start it again.
  Disable it for the copy:
  `scripts\vm.ps1 run 'Set-Service TunnelHop -StartupType Disabled; Stop-Service TunnelHop'`,
  then `copy`, then `Set-Service TunnelHop -StartupType Automatic; Start-Service TunnelHop`.
- `svc_live` drives the `th-service.exe` that `copy` put in `C:\th` and stops at once when
  TunnelHop is already installed: run it from a checkpoint without it.
- `ovpn_live` keeps each test's files under `C:\th\ovpn-live\<test>\`, including OpenVPN's own
  log in `logs\`, so a failure can be read afterwards.

**Checkpoints:**

```powershell
Checkpoint-VM -Name th-test -SnapshotName before-killswitch      # before any risky run
Restore-VMCheckpoint -VMName th-test -Name before-killswitch -Confirm:$false
Restore-VMCheckpoint -VMName th-test -Name clean -Confirm:$false  # back to a fresh Windows
```

**Shutting down when you're done:** `Stop-VM th-test`, and `docker compose stop` in
`test-infra/`. When the VM is off, Hyper-V uses no CPU or memory for it.

### The app in the VM (step 17 on)

```powershell
# On the PC: build with the Tauri CLI. A plain `cargo build` points the window at the dev
# server (localhost:1420), and the window shows "connection refused".
cd app; npx tauri build --no-bundle; cd ..          # -> target\release\th-app.exe
# The installer (app + service), for the installer pass in TESTING.md §3. The first build
# downloads NSIS into %LOCALAPPDATA%\tauri.
# It builds th-service.exe first; the service is listed as a resource only in
# tauri.installer.conf.json, so that the app compiles before the service ever has.
cd app; npm run installer; cd ..  # -> target\release\bundle\nsis\TunnelHop_<version>_x64-setup.exe
```

- **Run it from `C:\Program Files\TunnelHop\`**, not from `C:\th`. `C:\th` is locked to
  SYSTEM and Administrators, so an unelevated app cannot even read its own exe there (the
  launch fails with 0x80070005).
- **Start it unelevated on the VM's desktop:** someone must be signed in to the console
  (`user`/`user`). Then register a task once, with
  `New-ScheduledTaskPrincipal -UserId "$env:COMPUTERNAME\user" -LogonType Interactive
  -RunLevel Limited` and the exe as its action, and start it with `Start-ScheduledTask`. That
  runs the app in the user's session with the normal, non-admin token, as a real launch would.
  Run `Stop-ScheduledTask` first when the app was closed with Exit: Windows may still count the
  old instance as running and skip the start. Give the task
  `-Settings (New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries)`:
  the VM sees the PC's battery, and by default a task stays `Queued` while the PC is unplugged.
- **Bundles for the app to import go under the user's profile** (for example
  `C:\Users\user\Downloads\bundles`): the unelevated app cannot read `C:\th\configs`.
  `test-infra\bundles.py` builds them, including `bundle-auth.zip` (asks for credentials; the
  lab user is `th-test-user`, its password is in `gen.sh`) and `bundle-hostile.zip` (must be
  refused). Rebuild them without touching the keys with
  `docker compose run --rm --entrypoint python3 gen /infra/bundles.py /infra/out` (from Git
  Bash, prefix `MSYS_NO_PATHCONV=1`).
- **The app can be driven by keyboard** through `Msvm_Keyboard` (`TypeKey` for Tab and Enter,
  `TypeText` for a path in the file picker), with the screen capture to see where focus is.
- **To see the VM's screen without the Hyper-V window:** `GetVirtualSystemThumbnailImage` on
  `Msvm_VirtualSystemManagementService` returns the display as RGB565, with 4 bytes of padding
  at the end. A black image means the display is asleep: send a key through `Msvm_Keyboard`.
- The "Config files not found" pop-up at sign-in is OpenVPN GUI, installed with OpenVPN. It is
  not TunnelHop's; disable it under Task Manager → Startup apps.

### Cutting a release

Releases are built by CI (`.github/workflows/release.yml`), not on the PC, so what ships is
built from the tagged commit and nothing else.

1. Pass the installer pass in TESTING.md §3 with the commit you mean to release.
2. Set the new version in all three places: `Cargo.toml` (`[workspace.package]`),
   `app/src-tauri/tauri.conf.json` and `app/package.json`. Run `cargo build` once so that
   `Cargo.lock` follows. Merge that change through a pull request like any other.
3. Tag the merge commit on `main` and push the tag:
   ```powershell
   git switch main; git pull
   git tag -a v0.2.0 -m "TunnelHop 0.2.0"; git push origin v0.2.0
   ```
4. The workflow checks that the tag matches the three versions, runs `cargo test --workspace`,
   builds the installer, checks the service has no dev safety timer, and creates a **draft**
   release with
   `TunnelHop_<version>_x64-setup.exe` and `SHA256SUMS.txt`.
5. Read the draft on GitHub (Releases): edit the notes, and install the attached installer in
   `th-test` if this commit wasn't the one tested in step 1. Then publish it.

A failed run creates no release. Fix the cause, delete the tag
(`git push origin :refs/tags/v0.2.0`, `git tag -d v0.2.0`), and tag again. If the run got as
far as a draft, delete that draft too before tagging again, so only one is left. The installer is
not code-signed, so Windows SmartScreen warns on first run; the user guide says how to get past
that.

## 6. The leak test (steps 14 and 16)

With the service installed in `th-test`, a test bundle imported and selected, the test servers
up, and Docker Desktop running on the PC:

```powershell
# In the VM, once: install, point the probe at the lab, import and select a bundle.
# Separate calls: right after `install` the service is still starting and refuses a stop.
scripts\vm.ps1 run 'C:\th\th-service.exe install'
scripts\vm.ps1 run 'Set-Content C:\ProgramData\TunnelHop\settings.json ''{ "probe_urls": ["http://198.18.77.2:8080/"] }'' -Encoding ASCII; Restart-Service TunnelHop'
# bundle-all.zip rotates across WireGuard and OpenVPN (step 16); bundle-wg.zip, WireGuard only.
scripts\vm.ps1 run 'C:\th\th-service.exe ctl import C:\th\configs\bundle-all.zip'  # prints the bundle id
scripts\vm.ps1 run 'C:\th\th-service.exe ctl select <bundle id>'

scripts\leak-test.ps1          # 20 rotations through `th-service ctl rotate`, then the analysis
# Against an installed build instead of the dev copy in C:\th:
scripts\leak-test.ps1 -Service 'C:\Program Files\TunnelHop\th-service.exe'
```

The script captures with `pktmon` during the rotations, copies the capture to the PC, and reads
it with `tshark` in a throwaway `nicolaka/netshoot` container (no Wireshark install needed).
**"sent by the VM, not allowed" must be 0.** Three things it gets right that a quick capture
gets wrong:

- **Only the physical adapter** (the `Microsoft NetVsc Nic` group in `pktmon list`). With
  `--comp nics`, pktmon also records the tunnel adapters, whose packets are the traffic *inside*
  the tunnel, written as if they were Ethernet, so they decode as garbage.
- **Only frames the VM sends count.** pktmon captures at the adapter, before the firewall, so a
  packet arriving from outside appears whether or not it is let in. The script lists those
  separately. In practice they are the odd TCP reset from a server Windows talked to before the
  tunnel came up, sent to the VM's real address: noise, not a leak.
- **The VPN permit names ports, not the whole address**: 192.168.77.1 is also the VM's gateway,
  so anything else sent to it is a leak.

**Watching instead of rotating:** `scripts\leak-test.ps1 -Watch 180` captures for three minutes
without rotating, for checking something done by hand or through `ctl` meanwhile (editing a
bundle's servers, step 28). Connect first: it refuses to start without a tunnel, samples the
service's state every two seconds, and reports any sample with the kill-switch off. It leaves
the tunnel as it finds it.

## Safety rules

- **Kill-switch and live tests run only in `th-test`.** Not on the PC, not "just once".
- **Take a checkpoint before every kill-switch run.**
- **The dev safety timer** (step 13, Cargo feature `dev-safety`) lifts an unconfirmed engage
  after 120 s. It's a backstop, not a reason to test on the PC. Release builds never contain
  it, and CI checks that.
- **Never commit real VPN configs or credentials.** Keep provider files (such as ProtonVPN
  downloads) outside the repo, for example in `%USERPROFILE%\vpn-configs`. `.gitignore`
  refuses `*.conf`, `*.ovpn`, `auth.txt` and `*.zip` outside test fixtures, but don't rely on
  that alone.
- **Never touch tunnels without the `th-` prefix.** They belong to the user.

## Recovery

| Situation | Fix |
|---|---|
| `th-test` lost its network | `Invoke-Command -VMName th-test -Credential $cred { C:\th\th-service.exe disengage }`, or restore the last checkpoint |
| The service won't uninstall in `th-test` | restore the `clean` checkpoint |
| The kill-switch ended up on the **PC** anyway | at the PC: run `th-service.exe disengage` as administrator. If the binary is broken: `netsh wfp show filters file=wfp.xml`, find our provider GUID (`killswitch.rs`), and remove the filters with an administrator PowerShell script built on `FwpmFilterDeleteByKey0`. Rebooting does **not** help: the filters are persistent. |
| Leftover `WireGuardTunnel$th-*` services | `wireguard.exe /uninstalltunnelservice <name>` for each one (`sc query type= service state= all \| findstr th-`) |

## If the PC is Windows Home

Hyper-V and PowerShell Direct aren't available. Tell Claude at the start of the session. The
fallback:
- **VirtualBox** on the PC replaces Hyper-V. Its `VBoxManage snapshot take/restore` work while
  the VM runs.
- **Control goes over SSH:** turn on the OpenSSH server in the VM.
- **Step 13 gains a dev-only WFP permit for that SSH connection,** so a lockout doesn't cut
  control. A leak test then ignores exactly that one flow, and CI checks that release builds
  don't contain the permit.

## Starting the session

1. On the PC, open Claude Code in the repo.
2. Run `/gku:implement docs/PLAN.md --continue`. It starts at step 10, the first unticked step.
3. Progress, rulings and notes are written into `docs/PLAN.md` and committed with each step,
   so the plan stays the single place that says where the work stands.

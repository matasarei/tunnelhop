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
└── th-lab          internal switch + NAT, 192.168.77.0/24; the PC is .1 (servers) and .2 (probe)
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
cargo test --workspace     # rustup installs the pinned toolchain (rust-toolchain.toml) on first use
```

Every test should pass, the same as CI. (Windows runs one fewer than Linux: a symlink test in
`th-core` is Unix-only.) Then install Claude Code and the `gku` plugin, and open it in the repo.

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
New-NetIPAddress -IPAddress 192.168.77.2 -PrefixLength 24 -InterfaceAlias "vEthernet (th-lab)"
New-NetNat -Name th-lab -InternalIPInterfaceAddressPrefix 192.168.77.0/24
# Let th-test, and only the lab subnet, reach the test servers and the probe.
New-NetFirewallRule -DisplayName "TunnelHop test servers (lab only)" -Direction Inbound `
    -Action Allow -Protocol UDP -LocalPort 51820,51821,1194 -RemoteAddress 192.168.77.0/24
New-NetFirewallRule -DisplayName "TunnelHop probe (lab only)" -Direction Inbound `
    -Action Allow -Protocol TCP -LocalPort 8080 -RemoteAddress 192.168.77.0/24
```

| Address | Host |
|---|---|
| 192.168.77.1 | the PC: gateway, NAT to the internet, and the VPN test servers |
| 192.168.77.2 | the PC: the "what's my IP" probe |
| 192.168.77.20 | `th-test` |

The switch has no DHCP, so the VM gets a static address, gateway 192.168.77.1, and DNS
1.1.1.1.

## 3. The test VM (`th-test`)

1. Download a Windows 11 x64 ISO. The 90-day evaluation from Microsoft's Evaluation Center
   is fine for testing.
2. Create the VM (administrator PowerShell; adjust the paths):

   ```powershell
   New-VM -Name th-test -Generation 2 -MemoryStartupBytes 6GB -SwitchName th-lab `
          -NewVHDPath "C:\Hyper-V\th-test.vhdx" -NewVHDSizeBytes 64GB
   Set-VMProcessor -VMName th-test -Count 4
   Set-VMKeyProtector -VMName th-test -NewLocalKeyProtector
   Enable-VMTPM -VMName th-test                      # Windows 11 requires a TPM
   Add-VMDvdDrive -VMName th-test -Path "C:\ISO\Win11.iso"
   Set-VMFirmware -VMName th-test -FirstBootDevice (Get-VMDvdDrive -VMName th-test)
   Set-VM -Name th-test -CheckpointType Standard     # checkpoints keep the running state
   Start-VM th-test; vmconnect localhost th-test
   ```
3. Install Windows with a **local administrator** account (for example `thadmin`). Give it
   192.168.77.20/24, gateway 192.168.77.1, DNS 1.1.1.1.
4. Inside the VM, in an administrator PowerShell:

   ```powershell
   winget install --id WireGuard.WireGuard -e
   winget install --id OpenVPNTechnologies.OpenVPN -e
   New-Item -ItemType Directory C:\th | Out-Null
   # The service runs as LocalSystem from here: standard users must not be able to replace it.
   icacls C:\th /inheritance:r /grant:r "*S-1-5-18:(OI)(CI)F" "*S-1-5-32-544:(OI)(CI)F"
   ```
5. Back on the PC, store the VM's credential once. It's encrypted for your Windows user and
   kept outside the repo:

   ```powershell
   New-Item -ItemType Directory "$env:USERPROFILE\.tunnelhop" -Force | Out-Null
   Get-Credential thadmin | Export-Clixml "$env:USERPROFILE\.tunnelhop\th-test.cred"
   ```
6. Check that PowerShell Direct works, then take the clean checkpoint:

   ```powershell
   $cred = Import-Clixml "$env:USERPROFILE\.tunnelhop\th-test.cred"
   Invoke-Command -VMName th-test -Credential $cred { hostname; wg.exe --version }
   Checkpoint-VM -Name th-test -SnapshotName clean
   ```

## 4. The test servers (Docker Desktop on the PC)

The servers are containers, not another VM: Docker Desktop's WSL2 kernel has WireGuard and TUN,
and running them on the PC saves a VM's RAM and an OS install. They publish their ports on the
PC's lab addresses only (192.168.77.1 and .2), so they are reachable from `th-test` and from
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
- **one "what's my IP" endpoint, `http://192.168.77.2:8080/`**, answering with the caller's
  source address;
- **a generator** that writes fresh keys and matching client configs into `test-infra/out/`
  (gitignored), to be copied into `th-test` and zipped into test bundles.

**Why the endpoint matters.** The VM reaches the internet through the PC's NAT, so any public
"what's my IP" service would see the same address with or without the tunnel. The engine's
check that the IP changed (and differs from the ISP address) would then always fail. In
tests the service's probe URLs point at this endpoint instead. With no tunnel it sees Docker's
port proxy (`172.31.77.1`), and through each tunnel it sees that server's own address
(`172.31.77.11`, `.12`, `.13`). Step 14 makes the probe URLs configurable for exactly this.

**Why a second address.** OpenVPN's `redirect-gateway` routes the server's own address around
the tunnel, so a probe on 192.168.77.1 would never see the OpenVPN exit.

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
scripts\vm.ps1 run 'C:\th\th-service.exe disengage'
scripts\vm.ps1 restore before-wg
```

**Checkpoints:**

```powershell
Checkpoint-VM -Name th-test -SnapshotName before-killswitch      # before any risky run
Restore-VMCheckpoint -VMName th-test -Name before-killswitch -Confirm:$false
Restore-VMCheckpoint -VMName th-test -Name clean -Confirm:$false  # back to a fresh Windows
```

**Shutting down when you're done:** `Stop-VM th-test`, and `docker compose stop` in
`test-infra/`. When the VM is off, Hyper-V uses no CPU or memory for it.

## 6. The leak test (steps 14 and 16)

Inside `th-test`, with the service installed and a test bundle imported:

```powershell
pktmon start --capture --comp nics --file-name C:\th\leak.etl
# 20 × "Rotate Now" (through the pipe; step 14 adds a small CLI for this)
pktmon stop
pktmon etl2pcap C:\th\leak.etl --out C:\th\leak.pcapng
```

Copy `leak.pcapng` to the PC (`Copy-Item -FromSession $vm …`) and filter out the allowed
traffic:

```powershell
& "C:\Program Files\Wireshark\tshark.exe" -r leak.pcapng `
  -Y "not (ip.addr==192.168.77.1 and udp.port in {51820 51821 1194}) and not dhcp and not dhcpv6 and not (icmpv6.type >= 133 and icmpv6.type <= 137)"
```

The expected output is **empty**. Anything listed is a leak. The filter allows only the VPN
ports on 192.168.77.1, not the whole address: that address is also the VM's gateway, so
anything else sent to it is a leak too.

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

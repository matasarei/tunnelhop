<#
.SYNOPSIS
    The leak test (docs/DEVELOPMENT.md section 6): capture the th-test VM's physical network
    adapter during N rotations driven through `th-service ctl`, then list every frame the VM sent
    that was not VPN traffic, DHCP, IPv6 neighbour discovery or ARP. The expected list is empty.

.DESCRIPTION
    Needs the service installed in the VM with a bundle selected, the test servers up, and
    Docker Desktop on the PC (tshark runs in a throwaway nicolaka/netshoot container).

    Only the physical adapter is captured: the tunnel adapters carry the traffic *inside* the
    tunnel, which is not a leak. Only frames the VM sends count: pktmon captures at the adapter,
    before the firewall, so a packet arriving from outside shows up whether or not it is let in.
    Those are listed separately, for information.

.EXAMPLE
    scripts\leak-test.ps1                 # 20 rotations
    scripts\leak-test.ps1 -Rotations 5
    scripts\leak-test.ps1 -Watch 180      # capture for 3 minutes while you use the app or ctl

.NOTES
    -Watch does no rotating of its own: it expects a tunnel already up, samples the service's
    state every two seconds, and leaves the tunnel as it finds it. Use it to check that
    something done by hand (editing a bundle's servers, say) never opens a gap.
#>
[CmdletBinding()]
param(
    [int]$Rotations = 20,
    # Seconds to capture without rotating; 0 runs the rotations instead.
    [int]$Watch = 0,
    # UDP ports of the VPN servers on 192.168.77.1 (test-infra: wg-a, wg-b, ovpn).
    [int[]]$VpnPorts = @(41820, 41821, 1194),
    # The service whose `ctl` drives the test: the dev copy, or the installed one
    # (C:\Program Files\TunnelHop\th-service.exe).
    [string]$Service = 'C:\th\th-service.exe',
    [string]$VM = 'th-test'
)

$ErrorActionPreference = 'Stop'
$credential = Import-Clixml (Join-Path $env:USERPROFILE '.tunnelhop\th-test.cred')
$session = New-PSSession -VMName $VM -Credential $credential
try {
    $result = Invoke-Command -Session $session -ArgumentList $Rotations, $Watch, $Service {
        param($count, $watch, $service)
        # The capture is written to C:\th whichever service is under test.
        New-Item -ItemType Directory -Force C:\th | Out-Null
        function Ctl {
            $text = & $service ctl @args 2>$null | Out-String
            [pscustomobject]@{ Code = $LASTEXITCODE; Json = $(try { $text | ConvertFrom-Json } catch { $null }); Text = $text.Trim() }
        }
        $group = (pktmon list --json | Out-String | ConvertFrom-Json) |
            Where-Object { $_.Group -eq 'Microsoft NetVsc Nic' } | Select-Object -First 1
        $ids = @($group.Components.Id)
        if (-not $ids) { throw 'the physical adapter (Microsoft NetVsc Nic) was not found' }
        if ($watch -gt 0) {
            if ((Ctl status).Json.report.state -ne 'connected') {
                throw 'connect first: a watch expects a tunnel up from its first second'
            }
        } else {
            $connect = Ctl connect
            if ($connect.Code) { throw "connect failed: $($connect.Text)" }
        }
        foreach ($f in 'C:\th\leak.etl', 'C:\th\leak.pcapng') { if (Test-Path $f) { [IO.File]::Delete($f) } }
        pktmon start --capture --comp ($ids -join ',') --pkt-size 0 --file-name C:\th\leak.etl | Out-Null
        $states = @()
        $unblocked = 0
        if ($watch -gt 0) {
            $ips = @()
            $until = (Get-Date).AddSeconds($watch)
            while ((Get-Date) -lt $until) {
                $report = (Ctl status).Json.report
                $states += [string]$report.state
                if (-not $report.killswitch_engaged) { $unblocked++ }
                Start-Sleep -Seconds 2
            }
        } else {
            $ips = foreach ($i in 1..$count) {
                $r = Ctl rotate
                if ($r.Code) { "failed: $($r.Text)" } else { $r.Json.report.public_ip }
            }
        }
        pktmon stop | Out-Null
        pktmon etl2pcap C:\th\leak.etl --out C:\th\leak.pcapng | Out-Null
        if ($watch -eq 0) { $null = Ctl disconnect }
        # Who "the VM" is on the wire, read here rather than assumed: a rebuilt VM has a new MAC.
        $adapter = Get-NetAdapter -Physical | Where-Object Status -eq 'Up' | Select-Object -First 1
        $address = (Get-NetIPAddress -InterfaceIndex $adapter.ifIndex -AddressFamily IPv4).IPAddress
        [pscustomobject]@{
            Ips = $ips
            States = $states
            Unblocked = $unblocked
            Mac = $adapter.MacAddress.Replace('-', ':').ToLower()
            Address = $address
        }
    }
    $folder = Join-Path $env:TEMP 'th-leak'
    New-Item -ItemType Directory -Force $folder | Out-Null
    Copy-Item -FromSession $session C:\th\leak.pcapng -Destination (Join-Path $folder 'leak.pcapng') -Force
} finally {
    Remove-PSSession $session
}

if ($Watch -gt 0) {
    "watched $Watch s; states seen: " + (($result.States | Group-Object | ForEach-Object { "$($_.Name) x$($_.Count)" }) -join ', ')
    "samples with the kill-switch off: $($result.Unblocked)   <- must be 0"
} else {
    "$Rotations rotations; public IPs: " + (($result.Ips | Group-Object | ForEach-Object { "$($_.Name) x$($_.Count)" }) -join ', ')
}
if ($result.Mac -notmatch '^([0-9a-f]{2}:){5}[0-9a-f]{2}$') { throw "unexpected MAC from the VM: $($result.Mac)" }
"the VM on the wire: $($result.Mac), $($result.Address)"

$ports = ($VpnPorts | ForEach-Object { "udp.port==$_" }) -join ' || '
$vpn = "ip.addr==192.168.77.1 && ($ports)"
$allowed = "($vpn) || dhcp || dhcpv6 || (icmpv6.type>=133 && icmpv6.type<=137) || arp"
$fromVm = "eth.src==$($result.Mac) || ip.src==$($result.Address)"
$analysis = @"
f=/c/leak.pcapng
echo "frames captured:              `$(tshark -r `$f 2>&1 | wc -l)"
echo "VPN traffic:                  `$(tshark -r `$f -Y '$vpn' 2>&1 | wc -l)"
echo "sent by the VM, not allowed:  `$(tshark -r `$f -Y '($fromVm) && !($allowed)' 2>&1 | wc -l)   <- must be 0"
tshark -r `$f -Y '($fromVm) && !($allowed)' -T fields -E separator=' ' -e _ws.col.Protocol -e ip.dst -e ipv6.dst -e udp.dstport -e tcp.dstport 2>&1 | sort | uniq -c | sort -rn | head -20
echo "arrived from outside, not allowed (for information): `$(tshark -r `$f -Y '!($fromVm) && !($allowed)' 2>&1 | wc -l)"
tshark -r `$f -Y '!($fromVm) && !($allowed)' -T fields -E separator=' ' -e _ws.col.Protocol -e ip.src -e tcp.flags.str 2>&1 | sort | uniq -c | sort -rn | head -10
"@
# Through a file: Windows PowerShell 5.1 mangles a multi-line argument with quotes in it.
[IO.File]::WriteAllText((Join-Path $folder 'analyse.sh'), ($analysis -replace "`r`n", "`n"))
docker run --rm -v "${folder}:/c:ro" nicolaka/netshoot sh /c/analyse.sh

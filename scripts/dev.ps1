<#
.SYNOPSIS
    Start, stop and remove the lab on the PC: the th-lab network, the test servers in Docker
    Desktop, and the th-test VM. See docs/DEVELOPMENT.md, "The lab in one command".

    `start` and `stop` are for every day. `stop` frees the memory and deletes nothing; `start`
    brings up whatever is missing, the VM from its export included. `purge` is for a longer
    break: it removes the VM with every checkpoint, the containers and their images, the
    network and the build output. It keeps one thing, an export of the VM's clean checkpoint
    (about 19 GB), which is what lets `start` bring the VM back without installing Windows
    again. `purge -All` removes that too.

.EXAMPLE
    scripts\dev.ps1 status        # what exists, what runs, and how much disk it takes
    scripts\dev.ps1 start         # network, Docker Desktop, servers, VM: created if missing
    scripts\dev.ps1 stop          # save the VM, stop the servers, quit Docker Desktop
    scripts\dev.ps1 purge         # remove the lab from the PC, keeping the VM's export
    scripts\dev.ps1 purge -All    # remove the export too: the VM is then built by hand again
    scripts\dev.ps1 archive       # only write the export, and remove nothing
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory, Position = 0)]
    [ValidateSet('status', 'start', 'stop', 'purge', 'archive', 'network-up', 'network-down')]
    [string]$Action,

    # Where the VM's export is kept. Remembered between runs, so `start` finds what `purge` wrote.
    [string]$ArchivePath,

    # The checkpoint that `archive` and `purge` keep. Every other one goes with the VM.
    [string]$Checkpoint = 'clean',

    # `purge` also removes the VM's export, and so the way back by `start`.
    [switch]$All,

    # `purge` without the question, and `archive` over an existing export (`purge` always
    # replaces it, once the new one is complete).
    [switch]$Force,

    [string]$VM = 'th-test'
)

$ErrorActionPreference = 'Stop'
$repo = Split-Path $PSScriptRoot -Parent
$stateDirectory = Join-Path $env:USERPROFILE '.tunnelhop'
$stateFile = Join-Path $stateDirectory 'dev-env.json'
$credentialFile = Join-Path $stateDirectory 'th-test.cred'
$compose = Join-Path $repo 'test-infra\docker-compose.yml'

# The lab network, as DEVELOPMENT.md section 2 describes it.
$labSwitch = 'th-lab'
$labAlias = "vEthernet ($labSwitch)"
$subnet = '192.168.77.0/24'
$addresses = @(
    @{ Address = '192.168.77.1'; Prefix = 24 },   # the VM's gateway, and where the servers listen
    @{ Address = '198.18.77.2'; Prefix = 32 }     # the IP probe, off the lab subnet on purpose
)
$firewallRules = @(
    @{ Name = 'TunnelHop test servers (lab only)'; Protocol = 'UDP'; Ports = 41820, 41821, 1194 },
    @{ Name = 'TunnelHop probe (lab only)'; Protocol = 'TCP'; Ports = 8080 }
)

function Write-Step([string]$text) { Write-Host "== $text" }

function Test-Administrator {
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    ([Security.Principal.WindowsPrincipal]$identity).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
}

# What `purge` wrote and `start` reads: where the export is, and where the VM lived.
function Get-State {
    $state = @{ ArchivePath = 'C:\Hyper-V\th-test-export'; VmRoot = 'C:\Hyper-V' }
    if (Test-Path $stateFile) {
        $saved = Get-Content $stateFile -Raw | ConvertFrom-Json
        foreach ($key in @($state.Keys)) { if ($saved.$key) { $state[$key] = $saved.$key } }
    }
    if ($ArchivePath) { $state.ArchivePath = $ArchivePath }
    $state
}

function Save-State($state) {
    New-Item -ItemType Directory -Force $stateDirectory | Out-Null
    $state | ConvertTo-Json | Set-Content $stateFile -Encoding utf8
}

function Get-LabVM { Get-VM -Name $VM -ErrorAction SilentlyContinue }

function Get-SizeGB([string]$path) {
    if (-not (Test-Path $path)) { return 0 }
    $sum = (Get-ChildItem $path -Recurse -Force -File -ErrorAction SilentlyContinue | Measure-Object Length -Sum).Sum
    [Math]::Round($sum / 1GB, 1)
}

# --- the network ---------------------------------------------------------------------------

function Test-LabNetwork {
    if (-not (Get-VMSwitch -Name $labSwitch -ErrorAction SilentlyContinue)) { return $false }
    foreach ($a in $addresses) {
        if (-not (Get-NetIPAddress -InterfaceAlias $labAlias -IPAddress $a.Address -ErrorAction SilentlyContinue)) { return $false }
    }
    if (-not (Get-NetNat -Name $labSwitch -ErrorAction SilentlyContinue)) { return $false }
    foreach ($rule in $firewallRules) {
        if (-not (Get-NetFirewallRule -DisplayName $rule.Name -ErrorAction SilentlyContinue)) { return $false }
    }
    $true
}

# Creates whatever part of the network is missing. Needs an administrator.
function New-LabNetwork {
    if (-not (Get-VMSwitch -Name $labSwitch -ErrorAction SilentlyContinue)) {
        New-VMSwitch -Name $labSwitch -SwitchType Internal | Out-Null
    }
    foreach ($a in $addresses) {
        if (-not (Get-NetIPAddress -InterfaceAlias $labAlias -IPAddress $a.Address -ErrorAction SilentlyContinue)) {
            New-NetIPAddress -IPAddress $a.Address -PrefixLength $a.Prefix -InterfaceAlias $labAlias | Out-Null
        }
    }
    if (-not (Get-NetNat -Name $labSwitch -ErrorAction SilentlyContinue)) {
        New-NetNat -Name $labSwitch -InternalIPInterfaceAddressPrefix $subnet | Out-Null
    }
    # The servers answer the lab subnet and nothing else.
    foreach ($rule in $firewallRules) {
        if (-not (Get-NetFirewallRule -DisplayName $rule.Name -ErrorAction SilentlyContinue)) {
            New-NetFirewallRule -DisplayName $rule.Name -Direction Inbound -Action Allow `
                -Protocol $rule.Protocol -LocalPort $rule.Ports -RemoteAddress $subnet | Out-Null
        }
    }
}

# Removes the network. The switch takes its adapter and both addresses with it.
function Remove-LabNetwork {
    foreach ($rule in $firewallRules) {
        Get-NetFirewallRule -DisplayName $rule.Name -ErrorAction SilentlyContinue | Remove-NetFirewallRule
    }
    Get-NetNat -Name $labSwitch -ErrorAction SilentlyContinue | Remove-NetNat -Confirm:$false
    Get-VMSwitch -Name $labSwitch -ErrorAction SilentlyContinue | Remove-VMSwitch -Force
}

# Addresses, NAT and firewall rules need an administrator, and the rest of this script does not
# (Hyper-V Administrators is enough for the VM). So only this part asks for elevation, in a
# window of its own, and what it printed is shown afterwards.
function Invoke-NetworkAction([string]$networkAction) {
    if (Test-Administrator) {
        if ($networkAction -eq 'network-up') { New-LabNetwork } else { Remove-LabNetwork }
        return
    }
    Write-Host "   asking for an administrator (a Windows prompt) to change the lab network"
    $log = Join-Path $env:TEMP "tunnelhop-$networkAction.log"
    Remove-Item $log -ErrorAction SilentlyContinue
    $command = "try { & '$PSCommandPath' $networkAction *> '$log'; exit 0 } catch { `$_ | Out-File '$log' -Append; exit 1 }"
    $process = Start-Process powershell.exe -Verb RunAs -Wait -PassThru -WindowStyle Hidden `
        -ArgumentList '-NoProfile', '-ExecutionPolicy', 'Bypass', '-Command', $command
    if (Test-Path $log) { Get-Content $log | ForEach-Object { Write-Host "   $_" } }
    if ($process.ExitCode -ne 0) { throw "The lab network could not be changed (exit code $($process.ExitCode))." }
}

# --- Docker Desktop and the test servers -----------------------------------------------------

# Docker and compose write their progress to stderr, which Windows PowerShell 5.1 turns into
# errors under 'Stop' once redirected. So they run through cmd, and only the exit code decides.
function Invoke-Docker([string]$arguments, [switch]$Quiet) {
    $redirect = ''
    if ($Quiet) { $redirect = ' >nul 2>&1' }
    cmd.exe /c "docker $arguments$redirect"
    $LASTEXITCODE -eq 0
}

function Test-Docker { Invoke-Docker 'info' -Quiet }

function Start-Docker {
    if (Test-Docker) { return }
    Write-Step 'Starting Docker Desktop'
    if (-not (Invoke-Docker 'desktop start' -Quiet)) {
        $exe = Join-Path $env:ProgramFiles 'Docker\Docker\Docker Desktop.exe'
        if (-not (Test-Path $exe)) { throw 'Docker Desktop is not installed (DEVELOPMENT.md section 1).' }
        Start-Process $exe
    }
    for ($i = 0; $i -lt 60; $i++) {
        if (Test-Docker) { return }
        Start-Sleep -Seconds 5
    }
    throw 'Docker Desktop did not answer within five minutes.'
}

function Start-Servers {
    # First run on this clone: the keys and the configs that go with them.
    if (-not (Test-Path (Join-Path $repo 'test-infra\data'))) {
        Write-Step 'Generating the test servers'' keys and configs'
        if (-not (Invoke-Docker "compose -f `"$compose`" run --rm gen")) { throw 'gen failed.' }
    }
    Write-Step 'Starting the test servers'
    if (-not (Invoke-Docker "compose -f `"$compose`" up -d" -Quiet)) {
        # Shown this time, so the reason is on screen.
        Invoke-Docker "compose -f `"$compose`" up -d" | Out-Null
        throw 'The test servers did not start. They need the th-lab network.'
    }
}

# Containers that are not the test servers: somebody else's work, which this script leaves alone.
function Get-OtherContainers {
    $ours = @(cmd.exe /c "docker compose -f `"$compose`" ps -q 2>nul")
    @(cmd.exe /c 'docker ps -q --no-trunc 2>nul') | Where-Object { $_ -and ($ours -notcontains $_) }
}

function Stop-Servers([switch]$Remove) {
    if (-not (Test-Docker)) { return }
    $verb = 'stop'
    # With the images the servers were built into, and their volumes.
    if ($Remove) { $verb = 'down --rmi local --volumes' }
    Write-Step "Stopping the test servers (compose $verb)"
    Invoke-Docker "compose -f `"$compose`" $verb" -Quiet | Out-Null
    $others = @(Get-OtherContainers)
    if ($others.Count -gt 0) {
        Write-Host "   Docker Desktop stays up: $($others.Count) other container(s) are running."
        return
    }
    Write-Step 'Stopping Docker Desktop'
    if (-not (Invoke-Docker 'desktop stop' -Quiet)) {
        Write-Host '   it did not stop by itself: quit it from its tray icon.'
    }
}

# --- the VM ------------------------------------------------------------------------------

function Start-LabVM {
    $machine = Get-LabVM
    if (-not $machine) { throw "There is no $VM VM." }
    if ($machine.State -ne 'Running') {
        Write-Step "Starting $VM"
        Start-VM -VM $machine
    }
    if (-not (Test-Path $credentialFile)) {
        Write-Host "   no credential at $credentialFile (DEVELOPMENT.md section 3, step 4): not waiting for the VM."
        return
    }
    $credential = Import-Clixml $credentialFile
    for ($i = 0; $i -lt 60; $i++) {
        try {
            Invoke-Command -VMName $VM -Credential $credential -ErrorAction Stop { $env:COMPUTERNAME } | Out-Null
            return
        } catch { Start-Sleep -Seconds 5 }
    }
    throw "$VM did not answer over PowerShell Direct within five minutes."
}

function Stop-LabVM {
    $machine = Get-LabVM
    if ($machine -and $machine.State -eq 'Running') {
        Write-Step "Saving $VM"
        Stop-VM -VM $machine -Save
    }
}

# Exports one checkpoint as a VM of its own: the disk as it was then, without the other
# checkpoints. Written beside the old export and swapped in only once it is complete.
function Export-LabVM($state, [switch]$Replace) {
    if (-not (Get-LabVM)) { throw "There is no $VM VM to export." }
    if (-not (Get-VMCheckpoint -VMName $VM -Name $Checkpoint -ErrorAction SilentlyContinue)) {
        throw "$VM has no checkpoint named '$Checkpoint'."
    }
    $archive = $state.ArchivePath
    if ((Test-Path $archive) -and -not ($Force -or $Replace)) {
        throw "There is already an export at $archive. -Force replaces it."
    }
    $fresh = "$archive.new"
    if (Test-Path $fresh) { Remove-Item $fresh -Recurse -Force }
    Write-Step "Exporting $VM's '$Checkpoint' checkpoint to $archive (several minutes)"
    Export-VMSnapshot -VMName $VM -Name $Checkpoint -Path $fresh
    if (-not (Test-LabArchive $fresh)) {
        throw "The export at $fresh is incomplete: nothing was removed."
    }
    if (Test-Path $archive) { Remove-Item $archive -Recurse -Force }
    Rename-Item $fresh (Split-Path $archive -Leaf)
    $state.VmRoot = Split-Path (Get-LabVM).Path -Parent
    Save-State $state
    Write-Host "   $(Get-SizeGB $archive) GB"
}

# An export that can be imported has the VM's configuration and at least one disk.
function Test-LabArchive([string]$path) {
    if (-not (Test-Path $path)) { return $false }
    $configuration = @(Get-ChildItem $path -Recurse -Filter *.vmcx -ErrorAction SilentlyContinue)
    $disks = @(Get-ChildItem $path -Recurse -Include *.vhdx, *.avhdx -ErrorAction SilentlyContinue)
    ($configuration.Count -ge 1) -and ($disks.Count -ge 1)
}

function Remove-LabVM {
    $machine = Get-LabVM
    if (-not $machine) { return }
    $folder = $machine.Path
    # The folder is deleted whole, so it has to be the VM's own and nothing wider.
    if ((Split-Path $folder -Leaf) -ne $VM) { throw "$VM lives in $folder, which is not a folder of its own: remove it by hand." }
    Write-Step "Removing $VM and its checkpoints"
    if ($machine.State -ne 'Off') { Stop-VM -VM $machine -TurnOff -Force }
    Remove-VM -VM $machine -Force
    Remove-Item $folder -Recurse -Force
}

# Copies the export back to where the VM lived, and takes the checkpoint the tests restore.
function Import-LabVM($state) {
    $archive = $state.ArchivePath
    if (-not (Test-LabArchive $archive)) {
        throw "No VM export at $archive. Build the VM as DEVELOPMENT.md section 3 describes, or pass -ArchivePath."
    }
    $folder = Join-Path $state.VmRoot $VM
    Write-Step "Importing $VM from $archive (several minutes)"
    $configuration = Get-ChildItem $archive -Recurse -Filter *.vmcx | Select-Object -First 1
    $machine = Import-VM -Path $configuration.FullName -Copy -VirtualMachinePath $folder `
        -VhdDestinationPath $folder -SnapshotFilePath $folder -SmartPagingFilePath $folder
    if ($machine.Name -ne $VM) { Rename-VM -VM $machine -NewName $VM }
    Set-VM -Name $VM -CheckpointType Standard -AutomaticCheckpointsEnabled $false
    Checkpoint-VM -Name $VM -SnapshotName $Checkpoint
}

# --- what is needed on the PC ----------------------------------------------------------------

function Test-Tools {
    $missing = @()
    foreach ($tool in 'git', 'cargo', 'node', 'npm', 'docker') {
        if (-not (Get-Command $tool -ErrorAction SilentlyContinue)) { $missing += $tool }
    }
    if (-not (Get-Command Get-VM -ErrorAction SilentlyContinue)) { $missing += 'Hyper-V' }
    if ($missing.Count -gt 0) {
        throw "Missing on this PC: $($missing -join ', '). DEVELOPMENT.md section 1 has the commands."
    }
}

# --- the actions ---------------------------------------------------------------------------

function Show-Status {
    $state = Get-State
    $machine = Get-LabVM
    $network = 'missing'
    if (Test-LabNetwork) { $network = 'present' }
    Write-Host "Network  $labSwitch : $network"
    if (Test-Docker) {
        $running = @(cmd.exe /c "docker compose -f `"$compose`" ps -q 2>nul") | Where-Object { $_ }
        Write-Host "Docker   running, $(@($running).Count) test server container(s) up"
    } else {
        Write-Host 'Docker   not running'
    }
    if ($machine) {
        $checkpoints = @(Get-VMCheckpoint -VMName $VM).Count
        $memory = [Math]::Round($machine.MemoryAssigned / 1GB, 1)
        Write-Host "VM       $VM : $($machine.State), $memory GB of memory, $checkpoints checkpoint(s), $(Get-SizeGB $machine.Path) GB in $($machine.Path)"
    } else {
        Write-Host "VM       $VM : not on this PC"
    }
    if (Test-LabArchive $state.ArchivePath) {
        Write-Host "Export   $(Get-SizeGB $state.ArchivePath) GB in $($state.ArchivePath)"
    } else {
        Write-Host "Export   none at $($state.ArchivePath)"
    }
    Write-Host "Build    $(Get-SizeGB (Join-Path $repo 'target')) GB in target\"
}

function Start-Lab {
    Test-Tools
    if (-not (Test-LabNetwork)) {
        Write-Step 'Creating the lab network'
        Invoke-NetworkAction 'network-up'
    }
    # After a purge the VM comes from its export. Its adapter names th-lab, so the network
    # is there first.
    if (-not (Get-LabVM)) { Import-LabVM (Get-State) }
    Start-Docker
    Start-Servers
    Start-LabVM
    Write-Host "The lab is up. 'scripts\vm.ps1 copy' and 'scripts\vm.ps1 configs test-infra\out' put a build and the configs into the VM."
}

function Stop-Lab {
    Stop-LabVM
    Stop-Servers
    Write-Host 'The lab is stopped. Nothing was deleted.'
}

function Remove-Lab {
    $state = Get-State
    $machine = Get-LabVM
    if (-not $Force) {
        Write-Host 'This removes from the PC:'
        if ($machine) {
            Write-Host "  - the $VM VM with all $(@(Get-VMCheckpoint -VMName $VM).Count) of its checkpoints, and whatever is in it since '$Checkpoint'"
        }
        Write-Host "  - the test server containers and their images, and the $labSwitch network with its firewall rules"
        Write-Host '  - the build output in target\'
        if ($All) {
            Write-Host "  - the VM's export in $($state.ArchivePath): after this the VM is built by hand again (DEVELOPMENT.md section 3)"
        } else {
            Write-Host "It keeps an export of the VM's '$Checkpoint' checkpoint in $($state.ArchivePath), for 'start' to bring the VM back."
        }
        if ((Read-Host 'Type "purge" to go on') -ne 'purge') { Write-Host 'Nothing was changed.'; return }
    }
    if ($machine) {
        # A fresh export each time: the old one is replaced only once the new one is complete,
        # and the VM goes only once the export is.
        if (-not $All) { Export-LabVM $state -Replace }
        Remove-LabVM
    }
    if ($All -and (Test-Path $state.ArchivePath)) {
        Write-Step "Removing the VM's export"
        Remove-Item $state.ArchivePath -Recurse -Force
    }
    # Docker Desktop has to be up to remove what it holds.
    if (Get-Command docker -ErrorAction SilentlyContinue) {
        Start-Docker
        Stop-Servers -Remove
    }
    if (Get-VMSwitch -Name $labSwitch -ErrorAction SilentlyContinue) {
        Write-Step 'Removing the lab network'
        Invoke-NetworkAction 'network-down'
    }
    if (Test-Path (Join-Path $repo 'target')) {
        Write-Step 'Removing the build output'
        Push-Location $repo
        try { cmd.exe /c 'cargo clean 2>&1' } finally { Pop-Location }
    }
    if (Test-LabArchive $state.ArchivePath) {
        Write-Host "The lab is gone from the PC. 'scripts\dev.ps1 start' brings it back from the export."
    } else {
        Write-Host 'The lab is gone from the PC, and there is no export of the VM: DEVELOPMENT.md section 3 builds it again.'
    }
}

switch ($Action) {
    'status' { Show-Status }
    'start' { Start-Lab }
    'stop' { Stop-Lab }
    'purge' { Remove-Lab }
    'archive' { Export-LabVM (Get-State) }
    # The two an elevated copy of this script runs (Invoke-NetworkAction).
    'network-up' { New-LabNetwork }
    'network-down' { Remove-LabNetwork }
}

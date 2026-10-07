<#
.SYNOPSIS
    Build on the PC, run in the th-test VM. Everything goes over PowerShell Direct, which keeps
    working while the VM's network is blocked by the kill-switch. See docs/DEVELOPMENT.md section 5.

.EXAMPLE
    scripts\vm.ps1 checkpoint before-wg        # always before a risky run
    scripts\vm.ps1 copy                        # release th-service.exe to C:\th
    scripts\vm.ps1 configs ..\test-infra\out   # client configs to C:\th\configs
    scripts\vm.ps1 test wg_live -Env @{ TH_WG_CONFIG = 'C:\th\configs\wg-a.conf' }
    scripts\vm.ps1 run 'C:\th\th-service.exe disengage'
    scripts\vm.ps1 restore before-wg
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory, Position = 0)]
    [ValidateSet('copy', 'configs', 'test', 'run', 'checkpoint', 'restore')]
    [string]$Action,

    # The test target, the command, the checkpoint name, or the configs directory.
    [Parameter(Position = 1)]
    [string]$Name,

    # Environment variables for `test`.
    [hashtable]$Env = @{},

    [string]$VM = 'th-test'
)

$ErrorActionPreference = 'Stop'
$repo = Split-Path $PSScriptRoot -Parent
$credentialFile = Join-Path $env:USERPROFILE '.tunnelhop\th-test.cred'

function Require-Name([string]$what) {
    if (-not $Name) { throw "$Action needs $what" }
}

function Open-Session {
    if (-not (Test-Path $credentialFile)) {
        throw "No VM credential at $credentialFile (DEVELOPMENT.md section 3, step 5)."
    }
    $credential = Import-Clixml $credentialFile
    $session = New-PSSession -VMName $VM -Credential $credential
    # C:\th holds a binary that runs as LocalSystem: only SYSTEM and Administrators may write it.
    Invoke-Command -Session $session {
        New-Item -ItemType Directory -Force C:\th, C:\th\configs, C:\th\tunnels | Out-Null
        icacls C:\th /inheritance:r /grant:r '*S-1-5-18:(OI)(CI)F' '*S-1-5-32-544:(OI)(CI)F' | Out-Null
    }
    $session
}

# The path of the test executable cargo builds for `--test <target>`.
function Build-Test([string]$target) {
    Push-Location $repo
    try {
        $lines = cargo test -p th-service --test $target --no-run --message-format=json
        if ($LASTEXITCODE -ne 0) { throw "cargo could not build the $target test" }
    } finally {
        Pop-Location
    }
    $artifact = $lines |
        ForEach-Object { try { $_ | ConvertFrom-Json } catch { $null } } |
        Where-Object { $_.reason -eq 'compiler-artifact' -and $_.executable -and $_.target.name -eq $target } |
        Select-Object -Last 1
    if (-not $artifact) { throw "no test executable named $target" }
    $artifact.executable
}

switch ($Action) {
    'checkpoint' {
        Require-Name 'a checkpoint name'
        Checkpoint-VM -Name $VM -SnapshotName $Name
        "Checkpoint '$Name' taken."
    }
    'restore' {
        Require-Name 'a checkpoint name'
        Restore-VMCheckpoint -VMName $VM -Name $Name -Confirm:$false
        "Restored '$Name'."
    }
    'copy' {
        Push-Location $repo
        try {
            cargo build -p th-service --release
            if ($LASTEXITCODE -ne 0) { throw 'cargo build failed' }
        } finally {
            Pop-Location
        }
        $session = Open-Session
        try {
            Copy-Item -ToSession $session (Join-Path $repo 'target\release\th-service.exe') -Destination C:\th\ -Force
            'Copied th-service.exe to C:\th.'
        } finally {
            Remove-PSSession $session
        }
    }
    'configs' {
        Require-Name 'the directory with the generated client configs (test-infra/out)'
        $files = Get-ChildItem -File $Name -Include *.conf, *.ovpn -Recurse
        if (-not $files) { throw "no .conf or .ovpn files in $Name" }
        $session = Open-Session
        try {
            foreach ($file in $files) {
                Copy-Item -ToSession $session $file.FullName -Destination C:\th\configs\ -Force
            }
            "Copied $($files.Count) configs to C:\th\configs."
        } finally {
            Remove-PSSession $session
        }
    }
    'test' {
        Require-Name 'a test target (wg_live, ...)'
        $executable = Build-Test $Name
        $session = Open-Session
        try {
            $remote = "C:\th\$Name.exe"
            Copy-Item -ToSession $session $executable -Destination $remote -Force
            $code = Invoke-Command -Session $session -ArgumentList $remote, $Env {
                param($exe, $variables)
                foreach ($key in $variables.Keys) { Set-Item "env:$key" $variables[$key] }
                & $exe --ignored --nocapture --test-threads=1 | Out-Host
                $LASTEXITCODE
            }
            if ($code -ne 0) { throw "$Name failed in $VM (exit $code)" }
        } finally {
            Remove-PSSession $session
        }
    }
    'run' {
        Require-Name 'a command'
        $session = Open-Session
        try {
            Invoke-Command -Session $session -ScriptBlock ([scriptblock]::Create($Name))
        } finally {
            Remove-PSSession $session
        }
    }
}

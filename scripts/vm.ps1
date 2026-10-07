<#
.SYNOPSIS
    Build on the PC, run in the th-test VM. Everything goes over PowerShell Direct, which keeps
    working while the VM's network is blocked by the kill-switch. See docs/DEVELOPMENT.md section 5.

.EXAMPLE
    scripts\vm.ps1 checkpoint before-wg        # always before a risky run
    scripts\vm.ps1 copy                        # release th-service.exe to C:\th
    scripts\vm.ps1 configs ..\test-infra\out   # client configs to C:\th\configs
    scripts\vm.ps1 test wg_live -Env @{ TH_WG_CONFIG = 'C:\th\configs\wg-a.conf' }
    scripts\vm.ps1 test lib -AsSystem          # the crate's VM-only unit tests, as SYSTEM
    scripts\vm.ps1 test killswitch_live -Features dev-safety -Env @{ TH_DEV_SAFETY_SECONDS = '3' }
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

    # Run the test as LocalSystem, as the service runs, rather than as the VM's administrator
    # (some paths, such as secrets\, are SYSTEM's alone).
    [switch]$AsSystem,

    # Cargo features for `test` and `copy`, such as dev-safety.
    [string]$Features = '',

    [string]$VM = 'th-test'
)

$ErrorActionPreference = 'Stop'
$repo = Split-Path $PSScriptRoot -Parent
$credentialFile = Join-Path $env:USERPROFILE '.tunnelhop\th-test.cred'

function Assert-Name([string]$what) {
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

# Run cargo in the repository and return what it prints on stdout. Its progress goes to stderr,
# which Windows PowerShell 5.1 turns into terminating errors under 'Stop' once redirected, so
# stderr is shown as text and only the exit code decides.
function Invoke-Cargo([string[]]$arguments) {
    Push-Location $repo
    $previous = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try {
        & cargo @arguments 2>&1 | ForEach-Object {
            if ($_ -is [System.Management.Automation.ErrorRecord]) { Write-Host $_.ToString() } else { $_ }
        }
        if ($LASTEXITCODE -ne 0) { throw "cargo $($arguments -join ' ') failed (exit $LASTEXITCODE)" }
    } finally {
        $ErrorActionPreference = $previous
        Pop-Location
    }
}

# The path of the test executable cargo builds for `--test <target>`; `lib` means the
# crate's own unit tests.
function Get-TestExecutable([string]$target) {
    if ($target -eq 'lib') {
        $selector = @('--lib')
        $artifactName = 'th_service'
    } else {
        $selector = @('--test', $target)
        $artifactName = $target
    }
    $featureArguments = if ($Features) { @('--features', $Features) } else { @() }
    $lines = Invoke-Cargo (@('test', '-p', 'th-service') + $selector + $featureArguments + @('--no-run', '--message-format=json'))
    $artifact = $lines |
        ForEach-Object { try { $_ | ConvertFrom-Json } catch { $null } } |
        Where-Object { $_.reason -eq 'compiler-artifact' -and $_.executable -and $_.target.name -eq $artifactName } |
        Select-Object -Last 1
    if (-not $artifact) { throw "no test executable named $target" }
    $artifact.executable
}

switch ($Action) {
    'checkpoint' {
        Assert-Name 'a checkpoint name'
        Checkpoint-VM -Name $VM -SnapshotName $Name
        "Checkpoint '$Name' taken."
    }
    'restore' {
        Assert-Name 'a checkpoint name'
        Restore-VMCheckpoint -VMName $VM -Name $Name -Confirm:$false
        "Restored '$Name'."
    }
    'copy' {
        $featureArguments = if ($Features) { @('--features', $Features) } else { @() }
        Invoke-Cargo (@('build', '-p', 'th-service', '--release') + $featureArguments) | Out-Host
        $session = Open-Session
        try {
            Copy-Item -ToSession $session (Join-Path $repo 'target\release\th-service.exe') -Destination C:\th\ -Force
            'Copied th-service.exe to C:\th.'
        } finally {
            Remove-PSSession $session
        }
    }
    'configs' {
        Assert-Name 'the directory with the generated client configs (test-infra/out)'
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
        Assert-Name 'a test target (wg_live, lib, ...)'
        $executable = Get-TestExecutable $Name
        $session = Open-Session
        try {
            $remote = "C:\th\$Name.exe"
            Copy-Item -ToSession $session $executable -Destination $remote -Force
            # Live tests that could cut a machine off refuse to run without this.
            $variables = @{ TH_TEST_VM = '1' } + $Env
            $code = Invoke-Command -Session $session -ArgumentList $remote, $variables, $AsSystem.IsPresent {
                param($exe, $variables, $asSystem)
                $arguments = '--ignored --nocapture --test-threads=1'
                if (-not $asSystem) {
                    foreach ($key in $variables.Keys) { Set-Item "env:$key" $variables[$key] }
                    # stderr as plain text: the test's own output, not an error. The exit code decides.
                    & $exe $arguments.Split(' ') 2>&1 | ForEach-Object { "$_" } | Out-Host
                    return $LASTEXITCODE
                }
                # As SYSTEM: a one-off scheduled task runs a script that sets the environment,
                # runs the test with its output in a log, and records the exit code.
                $log = "$exe.log"; $exit = "$exe.exit"; $script = "$exe.cmd"
                Remove-Item $log, $exit -ErrorAction SilentlyContinue
                $lines = @('@echo off')
                foreach ($key in $variables.Keys) {
                    # The values go into a .cmd file verbatim: refuse what cmd would read as syntax.
                    if ("$key$($variables[$key])" -match '["%^!&|<>\r\n]') {
                        throw "-Env $key holds a character cmd would misread; use a plain value"
                    }
                    $lines += "set `"$key=$($variables[$key])`""
                }
                $lines += "`"$exe`" $arguments > `"$log`" 2>&1"
                $lines += "echo %ERRORLEVEL% > `"$exit`""
                Set-Content -Path $script -Value $lines -Encoding ASCII
                $task = 'TunnelHop-test'
                Unregister-ScheduledTask -TaskName $task -Confirm:$false -ErrorAction SilentlyContinue
                $action = New-ScheduledTaskAction -Execute 'cmd.exe' -Argument "/c `"$script`""
                Register-ScheduledTask -TaskName $task -Action $action -User 'SYSTEM' -RunLevel Highest | Out-Null
                Start-ScheduledTask -TaskName $task
                $deadline = (Get-Date).AddMinutes(10)
                while (-not (Test-Path $exit) -and (Get-Date) -lt $deadline) { Start-Sleep -Seconds 1 }
                Unregister-ScheduledTask -TaskName $task -Confirm:$false
                if (Test-Path $log) { Get-Content $log | Out-Host }
                if (-not (Test-Path $exit)) { return 'timed out' }
                [int](Get-Content $exit).Trim()
            }
            if ($code -ne 0) { throw "$Name failed in $VM (exit $code)" }
        } finally {
            Remove-PSSession $session
        }
    }
    'run' {
        Assert-Name 'a command'
        $session = Open-Session
        try {
            # A command's own errors are reported, not turned into a stop of this script.
            Invoke-Command -Session $session -ScriptBlock ([scriptblock]::Create($Name)) -ErrorAction Continue
        } finally {
            Remove-PSSession $session
        }
    }
}

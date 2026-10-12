# The lab on the PC: short names for scripts\dev.ps1 (docs/DEVELOPMENT.md section 5).
# `just` alone lists the recipes.

set windows-shell := ["powershell.exe", "-NoProfile", "-ExecutionPolicy", "Bypass", "-Command"]

[private]
default:
    @just --list --unsorted

# What exists, what runs, and how much disk it takes
status:
    @& ./scripts/dev.ps1 status

# Bring up the network, Docker Desktop, the test servers and the VM, creating what is missing
start:
    @& ./scripts/dev.ps1 start

# Save the VM, stop the test servers, quit Docker Desktop; deletes nothing
stop:
    @& ./scripts/dev.ps1 stop

# Remove the lab from the PC, keeping the VM's export; `just purge -All` removes that too
purge *args:
    @& ./scripts/dev.ps1 purge {{ args }}

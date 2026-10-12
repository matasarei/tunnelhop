# Security policy

TunnelHop runs a Windows service as LocalSystem, takes configuration files from outside, and
blocks the network with a firewall kill-switch. A flaw in any of these can hurt the people who
use it, so please report one privately and give a fix a chance to ship first.

## Reporting a vulnerability

Use GitHub's private vulnerability reporting:
**[Report a vulnerability](https://github.com/matasarei/tunnelhop/security/advisories/new)**
(the repository's *Security* tab → *Report a vulnerability*).

Please do **not** open a public issue, pull request or discussion about it.

A useful report has:

- what an attacker can do, and what they need to start with (a standard user account on the
  PC, a crafted bundle the user imports, a position on the network…);
- the TunnelHop version (*Settings → Apps*, or the installer's file name) and Windows version;
- the steps or the file that shows it. Use made-up configs and keys, never real ones.

You can expect an acknowledgement within a week, and an update at least every two weeks until
it's resolved. Once a fix is released, the advisory is published with credit to you, unless you
would rather not be named.

## Supported versions

TunnelHop is before 1.0. Only the latest release gets security fixes; please check that what
you found is still there in it.

## In scope

- **The service** (`th-service.exe`): anything that lets a standard user, another process or a
  remote party run code as LocalSystem, read or change what it protects, or stop it from
  protecting the user.
- **The named pipe** between the app and the service (`\\.\pipe\tunnelhop`): requests that get
  past its validation, impersonating the service to the app, or exhausting it.
- **Bundle import**: a `.zip` or config file that escapes its checks, writes outside the
  bundle store, runs a program or script, or carries a directive the sanitizer should have
  refused (SPEC §3.3–3.4).
- **The kill-switch**: traffic that leaves the PC outside the tunnel while TunnelHop says it is
  blocked, or a block that cannot be lifted.
- **Stored sign-ins**: OpenVPN usernames and passwords readable by anyone but the service, or
  left in plain text on disk.
- **The installer and uninstaller**: anything that leaves the PC less safe than before.

## Not in scope

- The WireGuard and OpenVPN programs themselves, and your VPN provider's servers or configs:
  report those to their own projects or to the provider.
- **Exporting bundles as any signed-in user.** Every interactive user of the PC can export the
  installed bundles, keys and sign-ins included. This is by design (SPEC §2.3 and §3.6): the
  window runs unelevated. On a shared PC, the keys are only as private as its accounts.
- Attacks that need administrator rights already: an administrator controls the PC anyway.
- The installer being unsigned, and the SmartScreen warning that follows.

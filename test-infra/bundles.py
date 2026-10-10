#!/usr/bin/env python3
"""Build the lab's test bundles from the client configs in out/.

Run by gen.sh after it writes the configs, and on its own to rebuild only the bundles (the
keys, and so the servers, stay as they are):

    docker compose run --rm --entrypoint python3 gen /infra/bundles.py /infra/out

A bundle's id is the hash of its zip, so a rebuilt bundle imports as a new one.

  bundle-wg.zip       WireGuard A and B: the WireGuard-only leak test.
  bundle-all.zip      WireGuard A and B plus OpenVPN: the mixed-protocol leak test.
  bundle-auth.zip     WireGuard A plus an OpenVPN profile with a bare auth-user-pass, as
                      ProtonVPN ships them: the import reports it as needing credentials.
                      The lab server takes th-test-user with the password in gen.sh.
  bundle-authfile.zip WireGuard A plus an OpenVPN profile whose credentials come in the
                      archive's auth.txt: the import must seal them and leave no plaintext
                      copy on disk.
  bundle-hostile.zip  Must be refused: a WireGuard config with PostUp and an OpenVPN config
                      with an up script and script-security. Nothing in it may ever run.
"""
import json
import sys
import zipfile

out = sys.argv[1]


def read(name):
    with open(f"{out}/{name}", encoding="utf-8") as config:
        return config.read()


def profile(profile_id, name, protocol, file, text=None):
    """A manifest entry, and the config text when it is not simply the file in out/."""
    entry = {"id": profile_id, "name": name, "protocol": protocol, "file": f"{protocol}/{file}"}
    return entry, text if text is not None else read(file)


wg_a = profile("wg-a", "Lab WG A", "wireguard", "wg-a.conf")
wg_b = profile("wg-b", "Lab WG B", "wireguard", "wg-b.conf")
ovpn = profile("ovpn", "Lab OpenVPN", "openvpn", "ovpn.ovpn")
ovpn_auth = profile("ovpn-auth", "Lab OpenVPN (sign-in)", "openvpn", "ovpn-auth.ovpn")
evil_wg = profile(
    "evil-wg",
    "Runs a command (WireGuard)",
    "wireguard",
    "evil.conf",
    read("wg-a.conf").replace("[Interface]\n", "[Interface]\nPostUp = calc.exe\n", 1),
)
evil_ovpn = profile(
    "evil-ovpn",
    "Runs a command (OpenVPN)",
    "openvpn",
    "evil.ovpn",
    'script-security 2\nup "C:\\\\Windows\\\\System32\\\\calc.exe"\n' + read("ovpn.ovpn"),
)

ovpn_file = profile(
    "ovpn-file",
    "Lab OpenVPN (auth.txt)",
    "openvpn",
    "with-file.ovpn",
    read("ovpn.ovpn") + "auth-user-pass auth.txt\n",
)
# The lab's fixed, obviously fake credentials (gen.sh), as a provider's auth.txt holds them.
AUTH_TXT = 'th-test-user\nth-test "pass\\word\n'

# name: (bundle name, profiles, other files in the archive)
bundles = {
    "bundle-wg.zip": ("TunnelHop lab", [wg_a, wg_b], {}),
    "bundle-all.zip": ("TunnelHop lab", [wg_a, wg_b, ovpn], {}),
    "bundle-auth.zip": ("TunnelHop lab (sign-in)", [wg_a, ovpn_auth], {}),
    "bundle-authfile.zip": (
        "TunnelHop lab (auth.txt)",
        [wg_a, ovpn_file],
        {"openvpn/auth.txt": AUTH_TXT},
    ),
    "bundle-hostile.zip": ("Hostile bundle (must be refused)", [evil_wg, evil_ovpn], {}),
}
for name, (bundle_name, profiles, extras) in bundles.items():
    manifest = {
        "schema_version": "1",
        "bundle_name": bundle_name,
        "profiles": [entry for entry, _ in profiles],
    }
    with zipfile.ZipFile(f"{out}/{name}", "w", zipfile.ZIP_DEFLATED) as bundle:
        bundle.writestr("manifest.json", json.dumps(manifest, indent=2))
        for entry, text in profiles:
            bundle.writestr(entry["file"], text)
        for path, text in extras.items():
            bundle.writestr(path, text)
print("bundles:", ", ".join(bundles))

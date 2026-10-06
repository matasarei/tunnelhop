//! Helpers shared by the integration tests: building zip bundles in memory.
#![allow(dead_code)]

use std::io::{Cursor, Write};

use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

pub const MANIFEST: &str = r#"{
  "schema_version": "1",
  "bundle_name": "Test",
  "profiles": [
    { "id": "wg-1", "name": "WG 1", "protocol": "wireguard", "file": "wireguard/one.conf" },
    { "id": "ovpn-1", "name": "OVPN 1", "protocol": "openvpn", "file": "openvpn/one.ovpn" }
  ]
}"#;

pub const WG_CONF: &str = "[Interface]\nPrivateKey = cGxhY2Vob2xkZXJwbGFjZWhvbGRlcnBsYWNlaG9sZGU=\nAddress = 10.0.0.2/32\nDNS = 10.0.0.1\n\n[Peer]\nPublicKey = cGxhY2Vob2xkZXJwbGFjZWhvbGRlcnBsYWNlaG9sZGU=\nAllowedIPs = 0.0.0.0/0, ::/0\nEndpoint = 198.51.100.7:51820\n";

pub const OVPN_CONF: &str = "client\ndev tun\nproto udp\nremote 198.51.100.8 1194\nnobind\npersist-key\npersist-tun\nremote-cert-tls server\n<ca>\n-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----\n</ca>\n";

pub enum Entry<'a> {
    File(&'a str, Vec<u8>),
    Stored(&'a str, Vec<u8>),
    Dir(&'a str),
    Symlink(&'a str, &'a str),
}

pub fn zip(entries: &[Entry]) -> Vec<u8> {
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    let deflated = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    let stored = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    for entry in entries {
        match entry {
            Entry::File(name, content) => {
                writer.start_file(*name, deflated).unwrap();
                writer.write_all(content).unwrap();
            }
            Entry::Stored(name, content) => {
                writer.start_file(*name, stored).unwrap();
                writer.write_all(content).unwrap();
            }
            Entry::Dir(name) => writer.add_directory(*name, deflated).unwrap(),
            Entry::Symlink(name, target) => writer.add_symlink(*name, *target, deflated).unwrap(),
        }
    }
    writer.finish().unwrap().into_inner()
}

/// A bundle that imports cleanly, plus `extra` entries.
pub fn valid_bundle(extra: Vec<Entry>) -> Vec<u8> {
    let mut entries = vec![
        Entry::File("manifest.json", MANIFEST.as_bytes().to_vec()),
        Entry::File("wireguard/one.conf", WG_CONF.as_bytes().to_vec()),
        Entry::File("openvpn/one.ovpn", OVPN_CONF.as_bytes().to_vec()),
    ];
    entries.extend(extra);
    zip(&entries)
}

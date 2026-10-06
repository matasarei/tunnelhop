//! Names for WireGuard tunnels — SPEC §4.2.
//!
//! WireGuard for Windows names the tunnel service after the config file, so the name must pass
//! its rules. Deriving it from the bundle and profile ids keeps two bundles that ship the same
//! file name from colliding, and the `th-` prefix marks the tunnels this app owns.

use sha2::{Digest, Sha256};

use crate::bundle::BundleId;

pub const PREFIX: &str = "th-";
const HASH_CHARS: usize = 12;

pub fn tunnel_name(bundle: &BundleId, profile_id: &str) -> String {
    let digest = Sha256::digest(format!("{bundle}/{profile_id}").as_bytes());
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    format!("{PREFIX}{}", &hex[..HASH_CHARS])
}

/// Whether this app created the tunnel; tunnels without the prefix belong to the user.
pub fn is_ours(name: &str) -> bool {
    name.starts_with(PREFIX) && is_valid_wireguard_name(name)
}

/// WireGuard for Windows' rule: `^[a-zA-Z0-9_=+.-]{1,32}$`, no reserved DOS device name
/// before the first dot, and no leading or trailing dot.
pub fn is_valid_wireguard_name(name: &str) -> bool {
    let charset_ok = !name.is_empty()
        && name.len() <= 32
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_=+.-".contains(&b));
    charset_ok && !name.starts_with('.') && !name.ends_with('.') && !is_dos_device_name(name)
}

/// Whether Windows treats a file name as a device (`CON`, `NUL`, `CONIN$`, `COM1`, `LPT²`,
/// `lpt9.txt`, …): the part before the first dot, ignoring case and trailing spaces, is a
/// reserved device name. Windows also counts 0 and the superscript digits ¹ ² ³ for COM and LPT.
pub(crate) fn is_dos_device_name(name: &str) -> bool {
    const RESERVED: &[&str] = &["con", "prn", "aux", "nul", "conin$", "conout$"];
    let stem = name
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end_matches(' ')
        .to_ascii_lowercase();
    let numbered = ["com", "lpt"].iter().any(|prefix| {
        let mut digit = stem.strip_prefix(prefix).unwrap_or("").chars();
        matches!(
            (digit.next(), digit.next()),
            (Some('0'..='9' | '¹' | '²' | '³'), None)
        )
    });
    RESERVED.contains(&stem.as_str()) || numbered
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn id(s: &str) -> BundleId {
        BundleId::of(s.as_bytes())
    }

    #[test]
    fn name_is_stable_and_prefixed() {
        let a = tunnel_name(&id("zip"), "ua-wg-01");
        assert_eq!(a, tunnel_name(&id("zip"), "ua-wg-01"));
        assert!(a.starts_with("th-"));
        assert_eq!(a.len(), 15);
        assert!(is_ours(&a));
    }

    #[test]
    fn same_profile_in_two_bundles_gets_two_names() {
        assert_ne!(
            tunnel_name(&id("bundle-a"), "ua-kyiv-01"),
            tunnel_name(&id("bundle-b"), "ua-kyiv-01")
        );
    }

    #[test]
    fn wireguard_rules_are_applied() {
        for ok in ["wg0", "th-0123456789ab", "a_b=c+d.e-f", &"x".repeat(32)] {
            assert!(is_valid_wireguard_name(ok), "{ok}");
        }
        for bad in [
            "",
            "has space",
            "dollar$",
            "slash/",
            ".lead",
            "trail.",
            "CON",
            "nul.conf",
            "Com1",
            "LPT9.x",
            &"x".repeat(33),
        ] {
            assert!(!is_valid_wireguard_name(bad), "{bad}");
        }
        assert!(is_valid_wireguard_name("com10"));
        for device in [
            "CONIN$",
            "conout$.txt",
            "COM\u{b9}",
            "lpt\u{b3}.ovpn",
            "COM0",
            "Aux .x",
        ] {
            assert!(is_dos_device_name(device), "{device}");
        }
        for fine in ["com10", "com", "lpt\u{b9}0", "conin", "console"] {
            assert!(!is_dos_device_name(fine), "{fine}");
        }
        assert!(!is_ours("wg0"), "user tunnels are never ours");
    }

    proptest! {
        #[test]
        fn every_generated_name_is_valid(bundle in ".*", profile in "[a-z0-9-]{1,40}") {
            let name = tunnel_name(&id(&bundle), &profile);
            prop_assert!(is_valid_wireguard_name(&name));
            prop_assert!(name.len() <= 32);
        }

        #[test]
        fn names_differ_across_bundles(a in ".+", b in ".+", profile in "[a-z0-9-]{1,40}") {
            prop_assume!(a != b);
            prop_assert_ne!(tunnel_name(&id(&a), &profile), tunnel_name(&id(&b), &profile));
        }
    }
}

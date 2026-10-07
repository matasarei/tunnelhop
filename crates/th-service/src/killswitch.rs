//! The kill-switch — SPEC §4.4.
//!
//! TunnelHop's own WFP provider and sublayer, holding persistent filters: a crashed service
//! leaves traffic blocked rather than leaking it, and the next start reconciles. While engaged,
//! only loopback, DHCP, IPv6 neighbour discovery and the VPN endpoints get through, plus any
//! tunnel interface explicitly allowed; everything else is blocked, LAN included.
//!
//! The rule set is plain data ([`base_rules`], [`interface_rules`]) so it is tested anywhere.
//! Applying it touches the machine's firewall and is never done on the development PC: only in
//! the `th-test` VM (CLAUDE.md, DEVELOPMENT.md).

use std::net::IpAddr;

use th_core::driver::ResolvedEndpoint;
use th_core::sanitize::Transport;

/// Every filter's name starts with this; interface permits carry their LUID after it.
pub const NAME: &str = "TunnelHop";
const INTERFACE_NAME: &str = "TunnelHop interface";
/// The block filters' name; finding one is how `is_engaged` knows the block is in place.
const BLOCK_NAME: &str = "TunnelHop: block everything else";

const PERMIT_WEIGHT: u8 = 15;
const BLOCK_WEIGHT: u8 = 0;

const UDP: u8 = 17;
const TCP: u8 = 6;
const ICMPV6: u8 = 58;
/// ICMPv6 router solicitation, advertisement, neighbour solicitation, advertisement, redirect.
const NDP_TYPES: std::ops::RangeInclusive<u16> = 133..=137;

/// Where a filter applies: outbound connections and inbound accepts, per address family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Layer {
    ConnectV4,
    ConnectV6,
    AcceptV4,
    AcceptV6,
}

impl Layer {
    pub const ALL: [Layer; 4] = [
        Layer::ConnectV4,
        Layer::ConnectV6,
        Layer::AcceptV4,
        Layer::AcceptV6,
    ];

    fn is_v6(self) -> bool {
        matches!(self, Layer::ConnectV6 | Layer::AcceptV6)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Permit,
    /// Final: no other sublayer's permit can override it.
    Block,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Condition {
    Loopback,
    Protocol(u8),
    RemoteAddress(IpAddr),
    RemotePort(u16),
    /// The local port; for ICMP, WFP puts the ICMP type here.
    LocalPort(u16),
    /// A network interface, by LUID.
    Interface(u64),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    pub name: String,
    pub layer: Layer,
    pub action: Action,
    pub weight: u8,
    pub conditions: Vec<Condition>,
}

fn permit(name: &str, layer: Layer, conditions: Vec<Condition>) -> Rule {
    Rule {
        name: format!("{NAME}: permit {name}"),
        layer,
        action: Action::Permit,
        weight: PERMIT_WEIGHT,
        conditions,
    }
}

/// The filters for an engaged kill-switch with `endpoints` permitted.
pub fn base_rules(endpoints: &[ResolvedEndpoint]) -> Vec<Rule> {
    use Condition::*;
    let mut rules = Vec::new();
    for layer in Layer::ALL {
        rules.push(permit("loopback", layer, vec![Loopback]));
        // DHCP keeps the physical adapter's address while everything else is blocked.
        let (client, server) = if layer.is_v6() { (546, 547) } else { (68, 67) };
        rules.push(permit(
            "DHCP",
            layer,
            vec![Protocol(UDP), LocalPort(client), RemotePort(server)],
        ));
        if layer.is_v6() {
            for kind in NDP_TYPES {
                rules.push(permit(
                    "neighbour discovery",
                    layer,
                    vec![Protocol(ICMPV6), LocalPort(kind)],
                ));
            }
        }
    }
    for endpoint in endpoints {
        let layer = if endpoint.addr.is_ipv6() {
            Layer::ConnectV6
        } else {
            Layer::ConnectV4
        };
        let protocol = match endpoint.transport {
            Transport::Udp => UDP,
            Transport::Tcp => TCP,
        };
        rules.push(permit(
            &format!("VPN endpoint {}", endpoint.addr),
            layer,
            vec![
                Protocol(protocol),
                RemoteAddress(endpoint.addr.ip()),
                RemotePort(endpoint.addr.port()),
            ],
        ));
    }
    for layer in Layer::ALL {
        rules.push(Rule {
            name: BLOCK_NAME.to_string(),
            layer,
            action: Action::Block,
            weight: BLOCK_WEIGHT,
            conditions: Vec::new(),
        });
    }
    rules
}

/// The name every filter permitting `luid` carries.
pub fn interface_name(luid: u64) -> String {
    format!("{INTERFACE_NAME} {luid:#x}")
}

/// Permit all traffic on a tunnel's interface, both directions and both families.
pub fn interface_rules(luid: u64) -> Vec<Rule> {
    Layer::ALL
        .into_iter()
        .map(|layer| Rule {
            name: interface_name(luid),
            layer,
            action: Action::Permit,
            weight: PERMIT_WEIGHT,
            conditions: vec![Condition::Interface(luid)],
        })
        .collect()
}

/// Whether a filter of ours is an interface permit, which `engage` keeps in place.
pub fn is_interface_filter(name: &str) -> bool {
    name.starts_with(INTERFACE_NAME)
}

#[cfg(windows)]
pub use wfp::WfpKillSwitch;

#[cfg(windows)]
mod wfp {
    use std::io;
    use std::net::IpAddr;
    use std::ptr;

    use async_trait::async_trait;
    use th_core::driver::{InterfaceId, ResolvedEndpoint};
    use th_core::rotation::KillSwitch;
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::NetworkManagement::WindowsFilteringPlatform::*;
    use windows_sys::Win32::System::Rpc::RPC_C_AUTHN_DEFAULT;
    use windows_sys::core::GUID;

    use super::{
        Action, BLOCK_NAME, Condition, Layer, NAME, Rule, base_rules, interface_name,
        interface_rules, is_interface_filter,
    };

    /// Fixed, so a restarted service, `th-service disengage` and the uninstaller all find the
    /// same objects. `netsh wfp show filters` lists them under this provider.
    pub const PROVIDER_KEY: GUID = GUID::from_u128(0xc2366378_9f20_4855_b28b_798b3561b418);
    const SUBLAYER_KEY: GUID = GUID::from_u128(0xad2b183b_87a5_49e8_8d2d_dabde26a7b1d);
    /// The highest sublayer weight: ours is evaluated first.
    const SUBLAYER_WEIGHT: u16 = u16::MAX;

    const FWP_E_FILTER_NOT_FOUND: u32 = 0x8032_0003;
    const FWP_E_PROVIDER_NOT_FOUND: u32 = 0x8032_0005;
    const FWP_E_SUBLAYER_NOT_FOUND: u32 = 0x8032_0007;
    const FWP_E_ALREADY_EXISTS: u32 = 0x8032_0009;

    fn check(code: u32, what: &str) -> io::Result<()> {
        if code == 0 {
            Ok(())
        } else {
            Err(io::Error::other(format!("{what} failed: {code:#010x}")))
        }
    }

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(Some(0)).collect()
    }

    /// An open WFP session; closed on drop. Not dynamic, so what it adds outlives it.
    struct Engine(HANDLE);

    impl Engine {
        fn open() -> io::Result<Engine> {
            let mut handle: HANDLE = ptr::null_mut();
            // SAFETY: local engine, default authentication, no session options; a valid out
            // pointer.
            check(
                unsafe {
                    FwpmEngineOpen0(
                        ptr::null(),
                        RPC_C_AUTHN_DEFAULT as u32,
                        ptr::null(),
                        ptr::null(),
                        &mut handle,
                    )
                },
                "opening the filtering engine",
            )?;
            Ok(Engine(handle))
        }

        /// Run `change` in one transaction: all of it lands, or none of it.
        fn transaction(&self, change: impl FnOnce(&Engine) -> io::Result<()>) -> io::Result<()> {
            // SAFETY: an open engine handle.
            check(
                unsafe { FwpmTransactionBegin0(self.0, 0) },
                "starting a transaction",
            )?;
            match change(self) {
                // SAFETY: as above, inside the transaction begun here.
                Ok(()) => check(unsafe { FwpmTransactionCommit0(self.0) }, "committing"),
                Err(e) => {
                    // SAFETY: as above.
                    unsafe { FwpmTransactionAbort0(self.0) };
                    Err(e)
                }
            }
        }

        fn ensure_provider_and_sublayer(&self) -> io::Result<()> {
            let mut name = wide(NAME);
            let mut description = wide("Blocks traffic while TunnelHop switches tunnels");
            let display = FWPM_DISPLAY_DATA0 {
                name: name.as_mut_ptr(),
                description: description.as_mut_ptr(),
            };
            // SAFETY: zero is a valid value for these plain C structures.
            let mut provider: FWPM_PROVIDER0 = unsafe { std::mem::zeroed() };
            provider.providerKey = PROVIDER_KEY;
            provider.displayData = display;
            provider.flags = FWPM_PROVIDER_FLAG_PERSISTENT;
            // SAFETY: the structure and the strings it points to live through the call.
            let code = unsafe { FwpmProviderAdd0(self.0, &provider, ptr::null_mut()) };
            if code != FWP_E_ALREADY_EXISTS {
                check(code, "adding the provider")?;
            }

            let mut provider_key = PROVIDER_KEY;
            // SAFETY: as above.
            let mut sublayer: FWPM_SUBLAYER0 = unsafe { std::mem::zeroed() };
            sublayer.subLayerKey = SUBLAYER_KEY;
            sublayer.displayData = display;
            sublayer.flags = FWPM_SUBLAYER_FLAG_PERSISTENT;
            sublayer.providerKey = &mut provider_key;
            sublayer.weight = SUBLAYER_WEIGHT;
            // SAFETY: as above.
            let code = unsafe { FwpmSubLayerAdd0(self.0, &sublayer, ptr::null_mut()) };
            if code != FWP_E_ALREADY_EXISTS {
                check(code, "adding the sublayer")?;
            }
            Ok(())
        }

        /// Every filter under our provider: its id and name.
        fn our_filters(&self) -> io::Result<Vec<(u64, String)>> {
            let mut handle: HANDLE = ptr::null_mut();
            // SAFETY: an open engine; no template enumerates every filter.
            check(
                unsafe { FwpmFilterCreateEnumHandle0(self.0, ptr::null(), &mut handle) },
                "listing filters",
            )?;
            let mut found = Vec::new();
            let result = loop {
                let mut entries: *mut *mut FWPM_FILTER0 = ptr::null_mut();
                let mut count = 0u32;
                // SAFETY: valid handles and out pointers; the entries are freed below.
                let code =
                    unsafe { FwpmFilterEnum0(self.0, handle, 256, &mut entries, &mut count) };
                if code != 0 {
                    break check(code, "listing filters");
                }
                // The end: WFP returns no list at all, and a slice from a null pointer is
                // undefined behaviour even at length zero.
                if count == 0 || entries.is_null() {
                    break Ok(());
                }
                // SAFETY: the call returned `count` valid filter pointers.
                let filters = unsafe { std::slice::from_raw_parts(entries, count as usize) };
                for &filter in filters {
                    // SAFETY: each pointer is a filter owned by the returned block.
                    let filter = unsafe { &*filter };
                    // SAFETY: a non-null provider key points to a GUID.
                    let ours = !filter.providerKey.is_null()
                        && same_guid(unsafe { &*filter.providerKey }, &PROVIDER_KEY);
                    if ours {
                        let name = if filter.displayData.name.is_null() {
                            String::new()
                        } else {
                            // SAFETY: a NUL-terminated name owned by the returned block.
                            unsafe { from_wide(filter.displayData.name) }
                        };
                        found.push((filter.filterId, name));
                    }
                }
                // SAFETY: memory the engine allocated for this call.
                unsafe { FwpmFreeMemory0((&raw mut entries).cast()) };
            };
            // SAFETY: the handle created above.
            unsafe { FwpmFilterDestroyEnumHandle0(self.0, handle) };
            result.map(|()| found)
        }

        fn delete(&self, id: u64) -> io::Result<()> {
            // SAFETY: an open engine; an unknown id is reported, not undefined.
            let code = unsafe { FwpmFilterDeleteById0(self.0, id) };
            if code == FWP_E_FILTER_NOT_FOUND {
                return Ok(());
            }
            check(code, "deleting a filter")
        }

        fn add(&self, rule: &Rule) -> io::Result<()> {
            let mut storage = ConditionStorage::default();
            let mut conditions: Vec<FWPM_FILTER_CONDITION0> = rule
                .conditions
                .iter()
                .map(|c| storage.condition(c))
                .collect();
            let mut name = wide(&rule.name);
            let mut provider_key = PROVIDER_KEY;
            // SAFETY: zero is a valid value for this plain C structure.
            let mut filter: FWPM_FILTER0 = unsafe { std::mem::zeroed() };
            filter.displayData.name = name.as_mut_ptr();
            filter.flags = FWPM_FILTER_FLAG_PERSISTENT;
            if rule.action == Action::Block {
                filter.flags |= FWPM_FILTER_FLAG_CLEAR_ACTION_RIGHT;
            }
            filter.providerKey = &mut provider_key;
            filter.layerKey = layer_key(rule.layer);
            filter.subLayerKey = SUBLAYER_KEY;
            filter.weight.r#type = FWP_UINT8;
            filter.weight.Anonymous.uint8 = rule.weight;
            filter.numFilterConditions = conditions.len() as u32;
            filter.filterCondition = if conditions.is_empty() {
                ptr::null_mut()
            } else {
                conditions.as_mut_ptr()
            };
            filter.action.r#type = match rule.action {
                Action::Permit => FWP_ACTION_PERMIT,
                Action::Block => FWP_ACTION_BLOCK,
            };
            let mut id = 0u64;
            // SAFETY: the filter, its conditions and the values they point to (in `storage`)
            // all live through the call, which copies them.
            check(
                unsafe { FwpmFilterAdd0(self.0, &filter, ptr::null_mut(), &mut id) },
                &format!("adding \"{}\"", rule.name),
            )
        }
    }

    impl Drop for Engine {
        fn drop(&mut self) {
            // SAFETY: the handle opened in `open`.
            unsafe { FwpmEngineClose0(self.0) };
        }
    }

    /// # Safety
    /// `p` must point at a NUL-terminated UTF-16 string.
    unsafe fn from_wide(p: *const u16) -> String {
        let mut len = 0;
        // SAFETY: the caller guarantees a terminator.
        unsafe {
            while *p.add(len) != 0 {
                len += 1;
            }
            String::from_utf16_lossy(std::slice::from_raw_parts(p, len))
        }
    }

    /// `windows-sys` GUIDs have no `==`.
    fn same_guid(a: &GUID, b: &GUID) -> bool {
        (a.data1, a.data2, a.data3, a.data4) == (b.data1, b.data2, b.data3, b.data4)
    }

    fn layer_key(layer: Layer) -> GUID {
        match layer {
            Layer::ConnectV4 => FWPM_LAYER_ALE_AUTH_CONNECT_V4,
            Layer::ConnectV6 => FWPM_LAYER_ALE_AUTH_CONNECT_V6,
            Layer::AcceptV4 => FWPM_LAYER_ALE_AUTH_RECV_ACCEPT_V4,
            Layer::AcceptV6 => FWPM_LAYER_ALE_AUTH_RECV_ACCEPT_V6,
        }
    }

    /// Values a condition points to, kept alive until the filter has been added. Boxed on
    /// purpose: a condition holds a raw pointer to its value, and a plain `Vec<u64>` would move
    /// its values when it grows, leaving the earlier pointers dangling.
    #[derive(Default)]
    #[allow(clippy::vec_box)]
    struct ConditionStorage {
        luids: Vec<Box<u64>>,
        v6: Vec<Box<FWP_BYTE_ARRAY16>>,
    }

    impl ConditionStorage {
        fn condition(&mut self, condition: &Condition) -> FWPM_FILTER_CONDITION0 {
            // SAFETY: zero is a valid value for this plain C structure.
            let mut out: FWPM_FILTER_CONDITION0 = unsafe { std::mem::zeroed() };
            out.matchType = FWP_MATCH_EQUAL;
            let value = &mut out.conditionValue;
            match *condition {
                Condition::Loopback => {
                    out.fieldKey = FWPM_CONDITION_FLAGS;
                    out.matchType = FWP_MATCH_FLAGS_ALL_SET;
                    value.r#type = FWP_UINT32;
                    value.Anonymous.uint32 = FWP_CONDITION_FLAG_IS_LOOPBACK;
                }
                Condition::Protocol(protocol) => {
                    out.fieldKey = FWPM_CONDITION_IP_PROTOCOL;
                    value.r#type = FWP_UINT8;
                    value.Anonymous.uint8 = protocol;
                }
                Condition::RemotePort(port) => {
                    out.fieldKey = FWPM_CONDITION_IP_REMOTE_PORT;
                    value.r#type = FWP_UINT16;
                    value.Anonymous.uint16 = port;
                }
                Condition::LocalPort(port) => {
                    out.fieldKey = FWPM_CONDITION_IP_LOCAL_PORT;
                    value.r#type = FWP_UINT16;
                    value.Anonymous.uint16 = port;
                }
                Condition::RemoteAddress(IpAddr::V4(address)) => {
                    out.fieldKey = FWPM_CONDITION_IP_REMOTE_ADDRESS;
                    // WFP wants an IPv4 address as a host-order number.
                    value.r#type = FWP_UINT32;
                    value.Anonymous.uint32 = u32::from(address);
                }
                Condition::RemoteAddress(IpAddr::V6(address)) => {
                    out.fieldKey = FWPM_CONDITION_IP_REMOTE_ADDRESS;
                    let mut bytes = Box::new(FWP_BYTE_ARRAY16 {
                        byteArray16: address.octets(),
                    });
                    value.r#type = FWP_BYTE_ARRAY16_TYPE;
                    value.Anonymous.byteArray16 = &mut *bytes;
                    self.v6.push(bytes);
                }
                Condition::Interface(luid) => {
                    out.fieldKey = FWPM_CONDITION_IP_LOCAL_INTERFACE;
                    let mut luid = Box::new(luid);
                    value.r#type = FWP_UINT64;
                    value.Anonymous.uint64 = &mut *luid;
                    self.luids.push(luid);
                }
            }
            out
        }
    }

    /// The kill-switch the rotation engine drives. Every call opens its own engine session and
    /// makes its change in one transaction.
    #[derive(Default)]
    pub struct WfpKillSwitch {
        #[cfg(feature = "dev-safety")]
        safety: super::dev_safety::Timer,
    }

    impl WfpKillSwitch {
        pub fn new() -> Self {
            Self::default()
        }

        /// Block everything but the basics and `endpoints`, keeping any interface permits.
        pub fn engage_now(endpoints: &[ResolvedEndpoint]) -> io::Result<()> {
            let engine = Engine::open()?;
            engine.transaction(|engine| {
                engine.ensure_provider_and_sublayer()?;
                for (id, name) in engine.our_filters()? {
                    if !is_interface_filter(&name) {
                        engine.delete(id)?;
                    }
                }
                base_rules(endpoints)
                    .iter()
                    .try_for_each(|rule| engine.add(rule))
            })
        }

        pub fn allow_interface_now(luid: u64) -> io::Result<()> {
            let engine = Engine::open()?;
            let name = interface_name(luid);
            engine.transaction(|engine| {
                engine.ensure_provider_and_sublayer()?;
                for (id, existing) in engine.our_filters()? {
                    if existing == name {
                        engine.delete(id)?;
                    }
                }
                interface_rules(luid)
                    .iter()
                    .try_for_each(|rule| engine.add(rule))
            })
        }

        pub fn revoke_interface_now(luid: u64) -> io::Result<()> {
            let engine = Engine::open()?;
            let name = interface_name(luid);
            engine.transaction(|engine| {
                for (id, existing) in engine.our_filters()? {
                    if existing == name {
                        engine.delete(id)?;
                    }
                }
                Ok(())
            })
        }

        /// Remove every filter of ours, then the sublayer and the provider: afterwards
        /// `netsh wfp show filters` lists nothing of TunnelHop's.
        pub fn disengage_now() -> io::Result<()> {
            let engine = Engine::open()?;
            engine.transaction(|engine| {
                for (id, _) in engine.our_filters()? {
                    engine.delete(id)?;
                }
                // SAFETY: an open engine and a valid key.
                let code = unsafe { FwpmSubLayerDeleteByKey0(engine.0, &SUBLAYER_KEY) };
                if code != FWP_E_SUBLAYER_NOT_FOUND {
                    check(code, "deleting the sublayer")?;
                }
                // SAFETY: as above.
                let code = unsafe { FwpmProviderDeleteByKey0(engine.0, &PROVIDER_KEY) };
                if code != FWP_E_PROVIDER_NOT_FOUND {
                    check(code, "deleting the provider")?;
                }
                Ok(())
            })
        }

        /// Whether the block is in place: what a starting service checks to report `Blocked`.
        pub fn is_engaged() -> io::Result<bool> {
            Ok(Engine::open()?
                .our_filters()?
                .iter()
                .any(|(_, name)| name == BLOCK_NAME))
        }

        /// How many filters of ours exist, for tests and diagnostics.
        pub fn filter_count() -> io::Result<usize> {
            Ok(Engine::open()?.our_filters()?.len())
        }

        /// Dev builds only: keep the current engage past the safety timer.
        #[cfg(feature = "dev-safety")]
        pub fn confirm(&self) {
            self.safety.confirm();
        }
    }

    fn reason(e: io::Error) -> String {
        e.to_string()
    }

    #[async_trait]
    impl KillSwitch for WfpKillSwitch {
        async fn engage(&self, endpoints: &[ResolvedEndpoint]) -> Result<(), String> {
            Self::engage_now(endpoints).map_err(reason)?;
            #[cfg(feature = "dev-safety")]
            self.safety.arm();
            Ok(())
        }

        async fn allow_interface(&self, interface: InterfaceId) -> Result<(), String> {
            Self::allow_interface_now(interface.0).map_err(reason)?;
            // A tunnel came up through the block: the machine is under control.
            #[cfg(feature = "dev-safety")]
            self.safety.confirm();
            Ok(())
        }

        async fn revoke_interface(&self, interface: InterfaceId) -> Result<(), String> {
            Self::revoke_interface_now(interface.0).map_err(reason)
        }

        async fn disengage(&self) -> Result<(), String> {
            Self::disengage_now().map_err(reason)
        }
    }
}

/// Dev builds only: lift an engage that nobody confirmed, so a bug cannot leave the test VM
/// cut off for good. Release builds never contain this; CI checks for [`MARKER`].
#[cfg(all(windows, feature = "dev-safety"))]
pub mod dev_safety {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    /// Present in a binary exactly when it contains the safety timer.
    pub const MARKER: &str = "TUNNELHOP_DEV_SAFETY_TIMER";

    /// 120 s, or `TH_DEV_SAFETY_SECONDS` (tests shorten it).
    fn delay() -> Duration {
        let seconds = std::env::var("TH_DEV_SAFETY_SECONDS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(120);
        Duration::from_secs(seconds)
    }

    /// Engages count up; confirming records the current one. A timer whose engage was neither
    /// confirmed nor superseded disengages.
    #[derive(Default)]
    pub struct Timer {
        armed: Arc<AtomicU64>,
        confirmed: Arc<AtomicU64>,
    }

    impl Timer {
        pub fn arm(&self) {
            let generation = self.armed.fetch_add(1, Ordering::SeqCst) + 1;
            let (armed, confirmed) = (self.armed.clone(), self.confirmed.clone());
            let delay = delay();
            log::warn!(
                "{MARKER}: lifting this engage in {} s unless confirmed",
                delay.as_secs()
            );
            std::thread::spawn(move || {
                std::thread::sleep(delay);
                let current = armed.load(Ordering::SeqCst) == generation;
                if current && confirmed.load(Ordering::SeqCst) < generation {
                    log::warn!("{MARKER}: engage not confirmed; disengaging");
                    if let Err(e) = super::WfpKillSwitch::disengage_now() {
                        log::error!("{MARKER}: could not disengage: {e}");
                    }
                }
            });
        }

        pub fn confirm(&self) {
            self.confirmed
                .store(self.armed.load(Ordering::SeqCst), Ordering::SeqCst);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;

    use super::*;

    fn endpoint(addr: &str, transport: Transport) -> ResolvedEndpoint {
        ResolvedEndpoint {
            addr: addr.parse::<SocketAddr>().unwrap(),
            transport,
        }
    }

    fn rules_with(rules: &[Rule], condition: Condition) -> Vec<&Rule> {
        rules
            .iter()
            .filter(|r| r.conditions.contains(&condition))
            .collect()
    }

    #[test]
    fn every_layer_ends_in_a_final_block_with_no_conditions() {
        let rules = base_rules(&[]);
        for layer in Layer::ALL {
            let blocks: Vec<_> = rules
                .iter()
                .filter(|r| r.layer == layer && r.action == Action::Block)
                .collect();
            assert_eq!(blocks.len(), 1, "{layer:?}");
            assert!(blocks[0].conditions.is_empty());
            assert_eq!(blocks[0].weight, BLOCK_WEIGHT);
            // is_engaged looks for exactly this name.
            assert_eq!(blocks[0].name, BLOCK_NAME);
        }
    }

    #[test]
    fn every_permit_is_narrow_and_outweighs_the_block() {
        let rules = base_rules(&[endpoint("192.0.2.1:51820", Transport::Udp)]);
        for rule in rules.iter().filter(|r| r.action == Action::Permit) {
            assert!(
                !rule.conditions.is_empty(),
                "{} permits everything",
                rule.name
            );
            assert!(rule.weight > BLOCK_WEIGHT, "{}", rule.name);
            assert!(rule.name.starts_with(NAME));
        }
    }

    #[test]
    fn loopback_dhcp_and_neighbour_discovery_are_permitted() {
        let rules = base_rules(&[]);
        assert_eq!(rules_with(&rules, Condition::Loopback).len(), 4);
        let dhcp_v4 = rules_with(&rules, Condition::LocalPort(68));
        assert_eq!(dhcp_v4.len(), 2);
        assert!(
            dhcp_v4
                .iter()
                .all(|r| r.conditions.contains(&Condition::RemotePort(67)))
        );
        let dhcp_v6 = rules_with(&rules, Condition::LocalPort(546));
        assert_eq!(dhcp_v6.len(), 2);
        assert!(dhcp_v6.iter().all(|r| r.layer.is_v6()));
        for kind in 133..=137 {
            let ndp = rules_with(&rules, Condition::LocalPort(kind));
            assert_eq!(ndp.len(), 2, "ICMPv6 type {kind}");
            assert!(
                ndp.iter()
                    .all(|r| r.conditions.contains(&Condition::Protocol(ICMPV6)))
            );
        }
        // ICMPv6 type 128 (echo) is not neighbour discovery.
        assert!(rules_with(&rules, Condition::LocalPort(128)).is_empty());
    }

    #[test]
    fn each_endpoint_is_permitted_by_address_port_and_protocol_on_its_family() {
        let rules = base_rules(&[
            endpoint("192.0.2.1:51820", Transport::Udp),
            endpoint("[2001:db8::1]:443", Transport::Tcp),
        ]);
        let v4 = rules_with(
            &rules,
            Condition::RemoteAddress("192.0.2.1".parse().unwrap()),
        );
        assert_eq!(v4.len(), 1);
        assert_eq!(v4[0].layer, Layer::ConnectV4);
        assert_eq!(
            v4[0].conditions,
            [
                Condition::Protocol(UDP),
                Condition::RemoteAddress("192.0.2.1".parse().unwrap()),
                Condition::RemotePort(51820)
            ]
        );
        let v6 = rules_with(
            &rules,
            Condition::RemoteAddress("2001:db8::1".parse().unwrap()),
        );
        assert_eq!(v6.len(), 1);
        assert_eq!(v6[0].layer, Layer::ConnectV6);
        assert!(v6[0].conditions.contains(&Condition::Protocol(TCP)));
        assert!(v6[0].conditions.contains(&Condition::RemotePort(443)));
    }

    #[test]
    fn no_endpoints_means_no_remote_permits() {
        let rules = base_rules(&[]);
        assert!(rules.iter().all(|r| {
            !r.conditions
                .iter()
                .any(|c| matches!(c, Condition::RemoteAddress(_)))
        }));
    }

    #[test]
    fn interface_permits_cover_every_layer_and_are_told_apart_from_the_base() {
        let rules = interface_rules(0x2a);
        assert_eq!(rules.len(), 4);
        assert!(
            rules
                .iter()
                .all(|r| r.conditions == [Condition::Interface(0x2a)])
        );
        assert!(rules.iter().all(|r| r.name == interface_name(0x2a)));
        assert!(is_interface_filter(&interface_name(0x2a)));
        assert!(
            base_rules(&[endpoint("192.0.2.1:1", Transport::Udp)])
                .iter()
                .all(|r| !is_interface_filter(&r.name))
        );
        assert_ne!(interface_name(1), interface_name(0x10));
    }
}

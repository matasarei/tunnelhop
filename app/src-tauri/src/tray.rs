//! The tray icon and its menu — SPEC §5. The menu shows the service's state and sends it
//! requests; it never changes what it shows by itself, only from the next status report, so
//! it cannot claim something the service did not do.

use th_core::ipc::StatusReport;
use th_core::rotation::State;

use crate::lang::text;

/// The rotation intervals the menu offers, in minutes.
pub const INTERVALS: [u32; 5] = [15, 30, 60, 120, 240];

/// The menu item ids, which [`Action::from_id`] maps back.
pub mod ids {
    pub const STATUS: &str = "status";
    pub const ROTATE: &str = "rotate";
    pub const AUTO: &str = "auto";
    pub const INTERVAL_PREFIX: &str = "interval-";
    pub const DISCONNECT: &str = "disconnect";
    pub const DASHBOARD: &str = "dashboard";
    pub const EXIT: &str = "exit";
}

/// What a click on a menu item asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Rotate,
    ToggleAuto,
    Interval(u32),
    Disconnect,
    Dashboard,
    Exit,
}

impl Action {
    pub fn from_id(id: &str) -> Option<Action> {
        match id {
            ids::ROTATE => Some(Action::Rotate),
            ids::AUTO => Some(Action::ToggleAuto),
            ids::DISCONNECT => Some(Action::Disconnect),
            ids::DASHBOARD => Some(Action::Dashboard),
            ids::EXIT => Some(Action::Exit),
            other => other
                .strip_prefix(ids::INTERVAL_PREFIX)
                .and_then(|m| m.parse().ok())
                .filter(|m| INTERVALS.contains(m))
                .map(Action::Interval),
        }
    }
}

/// Whole hours as hours, anything else as minutes. Abbreviated, so no language needs a
/// plural form here.
pub fn interval_label(minutes: u32) -> String {
    if minutes.is_multiple_of(60) {
        text("interval.hours", &[("n", &(minutes / 60).to_string())])
    } else {
        text("interval.minutes", &[("n", &minutes.to_string())])
    }
}

/// What the icon says when pointed at.
pub fn tooltip(status: &str) -> String {
    text("tray.tooltip", &[("status", status)])
}

/// A menu item's text. Windows reads `&` in a menu as "underline the next letter", so a real
/// one is doubled; the language files hold the text as it reads.
pub fn menu_text(key: &str) -> String {
    text(key, &[]).replace('&', "&&")
}

/// The status line: `Connected: <profile> (<IP>)`, `Blocked` or `Disconnected` (SPEC §5), or
/// that the service cannot be reached.
pub fn status_text(report: Option<&StatusReport>) -> String {
    let Some(report) = report else {
        return text("tray.status.unreachable", &[]);
    };
    match report.state {
        State::Connected => {
            let profile = report.profile_id.as_deref().unwrap_or("?");
            match report.public_ip {
                Some(ip) => text(
                    "tray.status.connected_ip",
                    &[("profile", profile), ("ip", &ip.to_string())],
                ),
                None => text("tray.status.connected", &[("profile", profile)]),
            }
        }
        State::Blocked => text("tray.status.blocked", &[]),
        State::Disconnected => text("tray.status.disconnected", &[]),
    }
}

#[cfg(windows)]
pub use menu::Tray;

#[cfg(windows)]
mod menu {
    use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};
    use tauri::tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
    use tauri::{AppHandle, Wry};
    use th_core::ipc::StatusReport;
    use th_core::rotation::State;

    use super::{Action, INTERVALS, ids, interval_label, menu_text, status_text, tooltip};

    /// The items whose text or checks follow the service's state.
    pub struct Tray {
        icon: TrayIcon,
        status: MenuItem<Wry>,
        rotate: MenuItem<Wry>,
        auto: CheckMenuItem<Wry>,
        intervals: Vec<(u32, CheckMenuItem<Wry>)>,
        disconnect: MenuItem<Wry>,
    }

    impl Tray {
        /// Build the icon and its menu; clicks go to `on_action`.
        pub fn build(
            app: &AppHandle,
            on_action: impl Fn(&AppHandle, Action) + Send + Sync + 'static,
        ) -> tauri::Result<Tray> {
            let status =
                MenuItem::with_id(app, ids::STATUS, status_text(None), false, None::<&str>)?;
            let rotate = MenuItem::with_id(
                app,
                ids::ROTATE,
                menu_text("tray.rotate"),
                false,
                None::<&str>,
            )?;
            let auto = CheckMenuItem::with_id(
                app,
                ids::AUTO,
                menu_text("tray.auto"),
                false,
                false,
                None::<&str>,
            )?;
            let intervals = INTERVALS
                .iter()
                .map(|&m| {
                    let id = format!("{}{m}", ids::INTERVAL_PREFIX);
                    CheckMenuItem::with_id(app, id, interval_label(m), false, false, None::<&str>)
                        .map(|item| (m, item))
                })
                .collect::<tauri::Result<Vec<_>>>()?;
            let interval_items: Vec<&dyn tauri::menu::IsMenuItem<Wry>> = intervals
                .iter()
                .map(|(_, item)| item as &dyn tauri::menu::IsMenuItem<Wry>)
                .collect();
            let interval =
                Submenu::with_items(app, menu_text("tray.interval"), true, &interval_items)?;
            let disconnect = MenuItem::with_id(
                app,
                ids::DISCONNECT,
                menu_text("tray.disconnect"),
                false,
                None::<&str>,
            )?;
            let dashboard = MenuItem::with_id(
                app,
                ids::DASHBOARD,
                menu_text("tray.dashboard"),
                true,
                None::<&str>,
            )?;
            let exit =
                MenuItem::with_id(app, ids::EXIT, menu_text("tray.exit"), true, None::<&str>)?;
            let menu = Menu::with_items(
                app,
                &[
                    &status,
                    &PredefinedMenuItem::separator(app)?,
                    &rotate,
                    &auto,
                    &interval,
                    &disconnect,
                    &PredefinedMenuItem::separator(app)?,
                    &dashboard,
                    &exit,
                ],
            )?;

            let mut builder = TrayIconBuilder::with_id("main")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .tooltip(tooltip(&status_text(None)))
                .on_menu_event(move |app, event| {
                    if let Some(action) = Action::from_id(event.id().as_ref()) {
                        on_action(app, action);
                    }
                })
                .on_tray_icon_event(|icon, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        crate::show_dashboard(icon.app_handle());
                    }
                });
            if let Some(image) = app.default_window_icon() {
                builder = builder.icon(image.clone());
            }
            let icon = builder.build(app)?;
            Ok(Tray {
                icon,
                status,
                rotate,
                auto,
                intervals,
                disconnect,
            })
        }

        /// Show `report`, or that the service cannot be reached.
        pub fn show(&self, report: Option<&StatusReport>) -> tauri::Result<()> {
            let text = status_text(report);
            self.status.set_text(&text)?;
            self.icon.set_tooltip(Some(tooltip(&text)))?;
            let reachable = report.is_some();
            let has_bundle = report.is_some_and(|r| r.bundle.is_some());
            self.rotate.set_enabled(has_bundle)?;
            self.auto.set_enabled(reachable)?;
            self.auto
                .set_checked(report.is_some_and(|r| r.auto_rotate))?;
            for (minutes, item) in &self.intervals {
                item.set_enabled(reachable)?;
                item.set_checked(report.is_some_and(|r| r.interval_minutes == *minutes))?;
            }
            // Also when blocked with no bundle: "unblock" is the way out of a crash's block.
            let engaged =
                report.is_some_and(|r| r.killswitch_engaged || r.state != State::Disconnected);
            self.disconnect.set_enabled(engaged)?;
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use th_core::ipc::StatusReport;

    fn report(state: State, profile: Option<&str>, ip: Option<&str>) -> StatusReport {
        StatusReport {
            state,
            profile_id: profile.map(Into::into),
            public_ip: ip.map(|ip| ip.parse().unwrap()),
            killswitch_engaged: state != State::Disconnected,
            bundle: None,
            auto_rotate: true,
            interval_minutes: 15,
            runtimes: Vec::new(),
        }
    }

    #[test]
    fn the_status_line_follows_the_spec() {
        assert_eq!(
            status_text(Some(&report(
                State::Connected,
                Some("ua-kyiv-01"),
                Some("203.0.113.7")
            ))),
            "Connected: ua-kyiv-01 (203.0.113.7)"
        );
        assert_eq!(
            status_text(Some(&report(State::Connected, Some("ua-kyiv-01"), None))),
            "Connected: ua-kyiv-01"
        );
        assert_eq!(
            status_text(Some(&report(State::Blocked, None, None))),
            "Blocked"
        );
        assert_eq!(
            status_text(Some(&report(State::Disconnected, None, None))),
            "Disconnected"
        );
        assert_eq!(status_text(None), "Service not running");
    }

    #[test]
    fn a_menu_item_shows_its_ampersand() {
        // Undoubled, Windows would underline the next letter and show no "&" at all.
        assert_eq!(menu_text("tray.disconnect"), "Disconnect && unblock");
        assert_eq!(menu_text("tray.exit"), "Exit");
    }

    #[test]
    fn menu_ids_map_back_to_actions() {
        assert_eq!(Action::from_id(ids::ROTATE), Some(Action::Rotate));
        assert_eq!(Action::from_id(ids::EXIT), Some(Action::Exit));
        for minutes in INTERVALS {
            let id = format!("{}{minutes}", ids::INTERVAL_PREFIX);
            assert_eq!(Action::from_id(&id), Some(Action::Interval(minutes)));
        }
        // The status line is not clickable, and only the offered intervals exist.
        for id in [
            ids::STATUS,
            "interval-5",
            "interval-7",
            "interval-",
            "interval-x",
            "",
        ] {
            assert_eq!(Action::from_id(id), None, "{id}");
        }
    }

    #[test]
    fn intervals_read_as_the_spec_writes_them() {
        let labels: Vec<String> = INTERVALS.iter().map(|&m| interval_label(m)).collect();
        assert_eq!(labels, ["15 min", "30 min", "1 h", "2 h", "4 h"]);
    }
}

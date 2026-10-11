//! What the window's and the tray's controls ask the service for, and which status reply is
//! still worth showing. Kept apart from the Tauri glue so that it is tested without a window.

use std::sync::atomic::{AtomicU64, Ordering};

use th_core::ipc::{Request, StatusReport};

use crate::tray::{self, Action};

/// What the window's controls ask for: the tray's actions, plus Connect. The shape is the
/// page's `Action` type in `ipc.ts`.
#[derive(Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum WindowAction {
    /// A random server, or the one picked in the list.
    Connect {
        #[serde(default)]
        profile_id: Option<String>,
    },
    Rotate,
    Disconnect,
    SetAutoRotate {
        enabled: bool,
    },
    SetInterval {
        minutes: u32,
    },
    Exit,
}

/// The request a window control sends; `None` for Exit, which is not one request.
pub fn window_request(action: WindowAction) -> Option<Request> {
    Some(match action {
        WindowAction::Connect { profile_id } => Request::Connect { profile_id },
        WindowAction::Rotate => Request::RotateNow,
        WindowAction::Disconnect => Request::Disconnect,
        WindowAction::SetAutoRotate { enabled } => Request::SetAutoRotate { enabled },
        WindowAction::SetInterval { minutes } => Request::SetInterval { minutes },
        WindowAction::Exit => return None,
    })
}

/// The request a tray item sends, given the report it was clicked on; `None` for the items
/// that open the window or quit. Auto-Rotation flips what the report says, not what the check
/// mark shows: the menu item has already flipped itself by the time it is clicked.
pub fn tray_request(action: Action, report: Option<&StatusReport>) -> Option<Request> {
    Some(match action {
        Action::Dashboard | Action::Exit => return None,
        Action::Rotate => Request::RotateNow,
        Action::Disconnect => Request::Disconnect,
        Action::Interval(minutes) => Request::SetInterval { minutes },
        Action::ToggleAuto => Request::SetAutoRotate {
            enabled: !report.is_some_and(|r| r.auto_rotate),
        },
    })
}

/// The intervals the window offers, each with its label.
pub fn intervals() -> Vec<(u32, String)> {
    tray::INTERVALS
        .iter()
        .map(|&m| (m, tray::interval_label(m)))
        .collect()
}

/// Orders the status the app asks for against the status the service pushes. A `GetStatus`
/// reply that arrives after a pushed one is older than what is shown, and is dropped.
#[derive(Debug, Default)]
pub struct Freshness(AtomicU64);

/// Taken when the app asks; [`Freshness::is_current`] says whether the reply still is.
#[derive(Debug, Clone, Copy)]
pub struct Asked(u64);

impl Freshness {
    pub fn asking(&self) -> Asked {
        Asked(self.0.load(Ordering::SeqCst))
    }

    /// The service pushed a status.
    pub fn pushed(&self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }

    /// Whether a reply to what was asked at `asked` is still the latest status.
    pub fn is_current(&self, asked: Asked) -> bool {
        self.0.load(Ordering::SeqCst) == asked.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use th_core::rotation::State;

    fn parse(json: &str) -> WindowAction {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn every_window_action_the_page_sends_becomes_its_request() {
        // The JSON is what `ipc.ts` sends for each `Action`.
        let cases = [
            (
                r#"{"type":"connect"}"#,
                Some(Request::Connect { profile_id: None }),
            ),
            (
                r#"{"type":"connect","profile_id":"wg-a"}"#,
                Some(Request::Connect {
                    profile_id: Some("wg-a".into()),
                }),
            ),
            (r#"{"type":"rotate"}"#, Some(Request::RotateNow)),
            (r#"{"type":"disconnect"}"#, Some(Request::Disconnect)),
            (
                r#"{"type":"set_auto_rotate","enabled":false}"#,
                Some(Request::SetAutoRotate { enabled: false }),
            ),
            (
                r#"{"type":"set_interval","minutes":30}"#,
                Some(Request::SetInterval { minutes: 30 }),
            ),
            (r#"{"type":"exit"}"#, None),
        ];
        for (json, request) in cases {
            assert_eq!(window_request(parse(json)), request, "{json}");
        }
    }

    #[test]
    fn what_the_page_cannot_send_is_refused() {
        for json in [
            r#"{"type":"connect","profile":"wg-a"}"#,
            r#"{"type":"set_interval"}"#,
            r#"{"type":"set_interval","minutes":-5}"#,
            r#"{"type":"format_disk"}"#,
        ] {
            assert!(
                serde_json::from_str::<WindowAction>(json).is_err(),
                "{json}"
            );
        }
    }

    fn report(auto_rotate: bool) -> StatusReport {
        StatusReport {
            state: State::Connected,
            profile_id: None,
            public_ip: None,
            killswitch_engaged: false,
            bundle: None,
            auto_rotate,
            interval_minutes: 60,
            rotatable: true,
            runtimes: Vec::new(),
        }
    }

    #[test]
    fn the_tray_toggles_auto_rotation_from_the_report_not_the_check_mark() {
        assert_eq!(
            tray_request(Action::ToggleAuto, Some(&report(true))),
            Some(Request::SetAutoRotate { enabled: false })
        );
        assert_eq!(
            tray_request(Action::ToggleAuto, Some(&report(false))),
            Some(Request::SetAutoRotate { enabled: true })
        );
        // No report yet: auto-rotation counts as off, so the click turns it on.
        assert_eq!(
            tray_request(Action::ToggleAuto, None),
            Some(Request::SetAutoRotate { enabled: true })
        );
        assert_eq!(
            tray_request(Action::Interval(120), None),
            Some(Request::SetInterval { minutes: 120 })
        );
        assert_eq!(tray_request(Action::Rotate, None), Some(Request::RotateNow));
        assert_eq!(
            tray_request(Action::Disconnect, None),
            Some(Request::Disconnect)
        );
        assert_eq!(tray_request(Action::Dashboard, None), None);
        assert_eq!(tray_request(Action::Exit, None), None);
    }

    #[test]
    fn the_window_is_offered_the_trays_intervals_with_labels() {
        let offered = intervals();
        let minutes: Vec<u32> = offered.iter().map(|(m, _)| *m).collect();
        assert_eq!(minutes, tray::INTERVALS);
        assert_eq!(offered[0].1, "15 min");
        assert_eq!(offered[2].1, "1 h");
    }

    #[test]
    fn a_reply_overtaken_by_a_pushed_status_is_stale() {
        let freshness = Freshness::default();
        let asked = freshness.asking();
        assert!(freshness.is_current(asked), "nothing came in between");

        let asked = freshness.asking();
        freshness.pushed();
        assert!(!freshness.is_current(asked), "the pushed status is newer");

        // Asked after the push: current again.
        assert!(freshness.is_current(freshness.asking()));
    }
}

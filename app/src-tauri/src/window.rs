//! How tall the dashboard opens (SPEC §5): tall enough to show the view without servers, in
//! every language, without a scrollbar; but never taller than the screen has room for, as on
//! a 1080p laptop at 150% scaling, where Windows leaves about 670 logical pixels.

/// The height the window asks for, in logical pixels. The same as `tauri.conf.json`, which is
/// what the window gets before the screen is known.
pub const HEIGHT: f64 = 780.0;
/// The window's minimum height (`tauri.conf.json`).
pub const MIN_HEIGHT: f64 = 480.0;
/// The title bar and a margin, which have to fit on the screen as well.
const FRAME: f64 = 40.0;

/// The window's height on a screen whose work area (the screen without the taskbar) is
/// `work_area` logical pixels tall: [`HEIGHT`] where that fits, else as tall as fits, and never
/// below [`MIN_HEIGHT`].
pub fn fitted_height(work_area: f64) -> f64 {
    if !work_area.is_finite() {
        return HEIGHT;
    }
    (work_area - FRAME).clamp(MIN_HEIGHT, HEIGHT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_screen_with_room_gets_the_full_height() {
        // 1080p at 100% and at 125%, less a 48-pixel taskbar.
        assert_eq!(fitted_height(1032.0), HEIGHT);
        assert_eq!(fitted_height(1080.0 / 1.25 - 48.0 / 1.25), HEIGHT);
        assert_eq!(fitted_height(HEIGHT + FRAME), HEIGHT);
    }

    #[test]
    fn a_short_screen_gets_as_much_as_fits() {
        // 1080p at 150%: 720 logical pixels, 688 without the taskbar.
        assert_eq!(fitted_height(688.0), 688.0 - FRAME);
        assert_eq!(fitted_height(HEIGHT + FRAME - 1.0), HEIGHT - 1.0);
    }

    #[test]
    fn the_window_never_gets_below_its_minimum() {
        assert_eq!(fitted_height(400.0), MIN_HEIGHT);
        assert_eq!(fitted_height(0.0), MIN_HEIGHT);
        assert_eq!(fitted_height(-1.0), MIN_HEIGHT);
    }

    #[test]
    fn an_unknown_screen_keeps_the_configured_height() {
        assert_eq!(fitted_height(f64::NAN), HEIGHT);
        assert_eq!(fitted_height(f64::INFINITY), HEIGHT);
    }
}

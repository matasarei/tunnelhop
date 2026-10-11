// The window is a page in WebView2, which brings a browser's own context menu (back, refresh,
// save as, print; in a text field also "import passwords" and the browser's tools) and its
// shortcuts. None of them belong in an app: they are turned off. Cut, copy and paste in a text
// field still work from the keyboard.

/**
 * Ctrl+key shortcuts of the browser: print, save, find, find next, reload, view source. By the
 * physical key: with a Ukrainian or Russian layout, `key` for P is "з".
 */
const BROWSER_CTRL_KEYS = new Set(["KeyP", "KeyS", "KeyF", "KeyG", "KeyR", "KeyU"]);
/** Reload, find next, caret browsing. */
const BROWSER_KEYS = new Set(["F5", "F3", "F7"]);

export function actLikeAnApp() {
  window.addEventListener("contextmenu", (event) => event.preventDefault());
  window.addEventListener("keydown", (event) => {
    const ctrl = event.ctrlKey || event.metaKey;
    if (BROWSER_KEYS.has(event.key) || (ctrl && BROWSER_CTRL_KEYS.has(event.code))) {
      event.preventDefault();
    }
  });
}

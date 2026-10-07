// Fluent 2, Microsoft's design system for Windows 11 and its apps, as web components. Only the
// components the window uses are registered, and the theme follows Windows' light/dark mode.
import "@fluentui/web-components/button/define.js";
import "@fluentui/web-components/dropdown/define.js";
import "@fluentui/web-components/listbox/define.js";
import "@fluentui/web-components/option/define.js";
import "@fluentui/web-components/message-bar/define.js";
import "@fluentui/web-components/spinner/define.js";
import "@fluentui/web-components/switch/define.js";
import { setTheme } from "@fluentui/web-components";
import { webDarkTheme, webLightTheme } from "@fluentui/tokens";

export function followSystemTheme(): () => void {
  const dark = window.matchMedia("(prefers-color-scheme: dark)");
  const apply = () => setTheme(dark.matches ? webDarkTheme : webLightTheme);
  apply();
  dark.addEventListener("change", apply);
  return () => dark.removeEventListener("change", apply);
}

// Tauri doesn't have a Node.js server to do proper SSR
// so we use adapter-static with a fallback to index.html to put the site in SPA mode
// See: https://svelte.dev/docs/kit/single-page-apps
// See: https://v2.tauri.app/start/frontend/sveltekit/ for more info
import { setLanguage } from "$lib/i18n";
import { language } from "$lib/ipc";
import { actLikeAnApp } from "$lib/webview";

export const ssr = false;

// No browser menu or browser shortcuts in the app's window.
actLikeAnApp();

// The language is settled before the page is drawn, so no text is ever shown in two languages.
export const load = async () => {
  // Outside the app (the page opened in a browser) there is nobody to ask: English.
  setLanguage(await language().catch(() => "en"));
};

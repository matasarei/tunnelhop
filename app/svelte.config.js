// Tauri doesn't have a Node.js server to do proper SSR
// so we use adapter-static with a fallback to index.html to put the site in SPA mode
// See: https://svelte.dev/docs/kit/single-page-apps
// See: https://v2.tauri.app/start/frontend/sveltekit/ for more info
import adapter from "@sveltejs/adapter-static";
import { vitePreprocess } from "@sveltejs/vite-plugin-svelte";

/** @type {import('@sveltejs/kit').Config} */
const config = {
  preprocess: vitePreprocess(),
  compilerOptions: {
    // Fluent's buttons are buttons: role, focus and Enter/Space come with the element, which
    // Svelte cannot see. Only these two checks, only on fluent-* elements.
    warningFilter: (warning) =>
      !(
        ["a11y_click_events_have_key_events", "a11y_no_static_element_interactions"].includes(
          warning.code,
        ) && warning.message.includes("<fluent-")
      ),
  },
  kit: {
    adapter: adapter({
      fallback: "index.html",
    }),
  },
};

export default config;

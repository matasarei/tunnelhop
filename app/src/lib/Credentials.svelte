<script lang="ts">
  // The username and password for a bundle's OpenVPN servers (SPEC §3.4): asked once for all
  // of them after an import, or for one server from the list. The password leaves this dialog
  // only towards the service, which seals it; the field is emptied as soon as it is sent.
  import { t, tn } from "$lib/i18n";
  import { setCredentials, type Bundle, type Profile } from "$lib/ipc";

  let {
    bundle,
    profile,
    refused = false,
    onclose,
  }: {
    /** The bundle the credentials are for. */
    bundle: Bundle;
    /** One server, or null for every server of the bundle that needs them. */
    profile: Profile | null;
    /** Asked because the server refused the saved ones, not because there were none. */
    refused?: boolean;
    onclose: () => void;
  } = $props();

  let dialog: (HTMLElement & { show(): void; hide(): void }) | undefined = $state();
  let username = $state("");
  let password = $state("");
  let saving = $state(false);
  let error = $state<string | null>(null);

  const openvpn = $derived(bundle.profiles.filter((p) => p.protocol === "openvpn"));
  // One provider's servers share one sign-in, so a new one is offered for all of them: on
  // unless the user says this server is different.
  let everywhere = $state(true);
  const shared = $derived(profile !== null && openvpn.length > 1);

  const waiting = $derived(bundle.profiles.filter((p) => p.protocol === "openvpn" && !p.has_credentials));

  $effect(() => {
    dialog?.show();
  });

  async function save(event: Event) {
    event.preventDefault();
    // A space picked up with a pasted username or password would be sent, and refused.
    const name = username.trim();
    const secret = password.trim();
    if (saving || !name || !secret) return;
    saving = true;
    error = null;
    try {
      if (shared && everywhere) {
        // As the bundle's default, and over whatever each server had of its own.
        await setCredentials(bundle.bundle_id, [], name, secret);
        await setCredentials(
          bundle.bundle_id,
          openvpn.map((p) => p.id),
          name,
          secret,
        );
      } else {
        await setCredentials(bundle.bundle_id, profile ? [profile.id] : [], name, secret);
      }
      password = "";
      onclose();
    } catch (e) {
      error = String(e);
    } finally {
      saving = false;
    }
  }

  // Fluent's inputs keep the real input element in a shadow root, so the form's own
  // submit-on-Enter never fires: Enter is handled here.
  function onkeydown(event: KeyboardEvent) {
    if (event.key === "Enter") save(event);
  }

  function cancel() {
    password = "";
    onclose();
  }

  // Esc and a click outside close the dialog without either button: that is "Not now" too.
  function ontoggle(event: Event) {
    if ((event as CustomEvent<{ newState: string }>).detail?.newState === "closed") cancel();
  }

  const value = (e: Event) => (e.currentTarget as HTMLInputElement).value;
</script>

<fluent-dialog bind:this={dialog} type="modal" aria-label={t("signin.label")} {ontoggle}>
  <fluent-dialog-body>
    <div slot="title">{t("signin.title", { name: profile ? profile.name : bundle.bundle_name })}</div>
    <form id="credentials" onsubmit={save}>
      {#if profile}
        {#if refused}
          <p><strong>{t("signin.refused")}</strong></p>
        {:else}
          <p>{shared ? t("signin.asks") : t("signin.one_server")}</p>
        {/if}
      {:else}
        <p>{tn("signin.needed", waiting.length)}</p>
      {/if}
      <!-- The account's own sign-in is the obvious guess, and the wrong one. -->
      <p>{t("signin.provider_hint")}</p>
      <fluent-text-input
        value={username}
        oninput={(e: Event) => (username = value(e))}
        {onkeydown}
        required
        autocomplete="off"
        spellcheck="false"
      >{t("signin.username")}</fluent-text-input>
      <fluent-text-input
        type="password"
        value={password}
        oninput={(e: Event) => (password = value(e))}
        {onkeydown}
        required
        autocomplete="off"
      >{t("signin.password")}</fluent-text-input>
      {#if shared}
        <div class="everywhere">
          <fluent-switch
            checked={everywhere}
            aria-label={t("signin.everywhere")}
            onchange={(e: Event) => (everywhere = (e.currentTarget as HTMLInputElement).checked)}
          ></fluent-switch>
          <span>{t("signin.everywhere")}</span>
        </div>
      {/if}
      {#if error}
        <fluent-message-bar intent="error" layout="multiline">{error}</fluent-message-bar>
      {/if}
      <p class="hint">{t("signin.stored")}</p>
    </form>
    <fluent-button
      slot="action"
      appearance="primary"
      disabled={saving || !username.trim() || !password.trim()}
      onclick={save}
    >
      {t("action.save")}
    </fluent-button>
    <fluent-button slot="action" disabled={saving} onclick={cancel}>{t("action.not_now")}</fluent-button>
  </fluent-dialog-body>
</fluent-dialog>

<style>
  .everywhere {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  form {
    display: flex;
    flex-direction: column;
    gap: 12px;
  }
  p {
    margin: 0;
  }
  .hint {
    font-size: 12px;
    color: var(--colorNeutralForeground3, #616161);
  }
</style>

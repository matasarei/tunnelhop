<script lang="ts">
  // The username and password for a bundle's OpenVPN servers (SPEC §3.4): asked once for all
  // of them after an import, or for one server from the list. The password leaves this dialog
  // only towards the service, which seals it; the field is emptied as soon as it is sent.
  import { t, tn } from "$lib/i18n";
  import { setCredentials, type Bundle, type Profile } from "$lib/ipc";

  let {
    bundle,
    profile,
    onclose,
  }: {
    /** The bundle the credentials are for. */
    bundle: Bundle;
    /** One server, or null for every server of the bundle that needs them. */
    profile: Profile | null;
    onclose: () => void;
  } = $props();

  let dialog: (HTMLElement & { show(): void; hide(): void }) | undefined = $state();
  let username = $state("");
  let password = $state("");
  let saving = $state(false);
  let error = $state<string | null>(null);

  const waiting = $derived(bundle.profiles.filter((p) => p.protocol === "openvpn" && !p.has_credentials));

  $effect(() => {
    dialog?.show();
  });

  async function save(event: Event) {
    event.preventDefault();
    if (saving || !username || !password) return;
    saving = true;
    error = null;
    try {
      await setCredentials(bundle.bundle_id, profile ? [profile.id] : [], username, password);
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
        <p>{t("signin.one_server")}</p>
      {:else}
        <p>{tn("signin.needed", waiting.length)}</p>
      {/if}
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
      {#if error}
        <fluent-message-bar intent="error" layout="multiline">{error}</fluent-message-bar>
      {/if}
      <p class="hint">{t("signin.stored")}</p>
    </form>
    <fluent-button
      slot="action"
      appearance="primary"
      disabled={saving || !username || !password}
      onclick={save}
    >
      {t("action.save")}
    </fluent-button>
    <fluent-button slot="action" disabled={saving} onclick={cancel}>{t("action.not_now")}</fluent-button>
  </fluent-dialog-body>
</fluent-dialog>

<style>
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

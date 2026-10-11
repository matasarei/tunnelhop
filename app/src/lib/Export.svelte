<script lang="ts">
  // Export a bundle to a file (SPEC §3.6): with its OpenVPN sign-ins or without, and protected
  // by a password, which is offered and recommended but not required. The file holds the
  // servers' private keys, so the dialog says so before anything is written.
  import { t } from "$lib/i18n";
  import { exportBundle, type Bundle } from "$lib/ipc";

  let {
    bundle,
    onclose,
  }: {
    bundle: Bundle;
    /** `saved` is false when nothing was written: cancelled here or in the save dialog. */
    onclose: (saved: boolean) => void;
  } = $props();

  let dialog: (HTMLElement & { show(): void; hide(): void }) | undefined = $state();
  let password = $state("");
  let repeat = $state("");
  let saving = $state(false);
  let error = $state<string | null>(null);
  /** Closing removes the dialog, which reports itself closed once more: answered once only. */
  let closed = false;

  const signsIn = $derived(bundle.profiles.some((p) => p.protocol === "openvpn"));
  let includeSignIns = $state(true);

  const protectedFile = $derived(password !== "");
  const mismatch = $derived(protectedFile && repeat !== "" && repeat !== password);
  const ready = $derived(!protectedFile || repeat === password);

  $effect(() => {
    dialog?.show();
  });

  async function save(event: Event) {
    event.preventDefault();
    if (saving || !ready) return;
    saving = true;
    error = null;
    try {
      const saved = await exportBundle(bundle.bundle_id, signsIn && includeSignIns, protectedFile ? password : null);
      // Closing the save dialog leaves this one open, to change something or cancel.
      if (!saved) return;
      forget();
      closed = true;
      onclose(true);
    } catch (e) {
      error = String(e);
    } finally {
      saving = false;
    }
  }

  function forget() {
    password = "";
    repeat = "";
  }

  // Fluent's inputs keep the real input element in a shadow root, so the form's own
  // submit-on-Enter never fires: Enter is handled here.
  function onkeydown(event: KeyboardEvent) {
    if (event.key === "Enter") save(event);
  }

  function cancel() {
    forget();
    if (closed) return;
    closed = true;
    onclose(false);
  }

  // Esc and a click outside close the dialog without either button: that is a cancel.
  function ontoggle(event: Event) {
    if ((event as CustomEvent<{ newState: string }>).detail?.newState === "closed") cancel();
  }

  const value = (e: Event) => (e.currentTarget as HTMLInputElement).value;
</script>

<fluent-dialog bind:this={dialog} type="modal" aria-label={t("export.title", { bundle: bundle.bundle_name })} {ontoggle}>
  <fluent-dialog-body>
    <div slot="title">{t("export.title", { bundle: bundle.bundle_name })}</div>
    <form id="export" onsubmit={save}>
      <p>{t("export.keys")}</p>
      {#if signsIn}
        <div class="switch">
          <fluent-switch
            checked={includeSignIns}
            aria-label={t("export.sign_ins")}
            onchange={(e: Event) => (includeSignIns = (e.currentTarget as HTMLInputElement).checked)}
          ></fluent-switch>
          <span>{t("export.sign_ins")}</span>
        </div>
        <p class="hint">{includeSignIns ? t("export.sign_ins.on") : t("export.sign_ins.off")}</p>
      {/if}
      <fluent-text-input
        type="password"
        value={password}
        oninput={(e: Event) => (password = value(e))}
        {onkeydown}
        autocomplete="new-password"
      >{t("export.password")}</fluent-text-input>
      {#if protectedFile}
        <fluent-text-input
          type="password"
          value={repeat}
          oninput={(e: Event) => (repeat = value(e))}
          {onkeydown}
          autocomplete="new-password"
        >{t("export.repeat")}</fluent-text-input>
      {/if}
      {#if mismatch}
        <p class="problem">{t("export.mismatch")}</p>
      {:else if protectedFile}
        <p class="hint">{t("export.password.hint")}</p>
      {:else}
        <fluent-message-bar intent="warning" layout="multiline">{t("export.unprotected")}</fluent-message-bar>
      {/if}
      {#if error}
        <fluent-message-bar intent="error" layout="multiline">{error}</fluent-message-bar>
      {/if}
    </form>
    <fluent-button slot="action" appearance="primary" disabled={saving || !ready} onclick={save}>
      {protectedFile ? t("action.export") : t("action.export_unprotected")}
    </fluent-button>
    <fluent-button slot="action" disabled={saving} onclick={cancel}>{t("action.cancel")}</fluent-button>
  </fluent-dialog-body>
</fluent-dialog>

<style>
  .switch {
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
  .problem {
    font-size: 12px;
    color: var(--colorPaletteRedForeground1, #bc2f32);
  }
</style>

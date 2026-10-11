<script lang="ts">
  // The password of a protected bundle, asked when an import needs it. It goes with the file
  // to the service and is not kept here.
  import { t } from "$lib/i18n";

  let {
    name,
    wrong,
    onunlock,
    oncancel,
  }: {
    /** The file's name, so the user knows which password is meant. */
    name: string;
    /** The last password given did not open it. */
    wrong: boolean;
    onunlock: (password: string) => void;
    oncancel: () => void;
  } = $props();

  let dialog: (HTMLElement & { show(): void; hide(): void }) | undefined = $state();
  let password = $state("");
  let answered = false;

  $effect(() => {
    dialog?.show();
  });

  function unlock(event: Event) {
    event.preventDefault();
    if (!password) return;
    answered = true;
    const given = password;
    password = "";
    onunlock(given);
  }

  // Fluent's inputs keep the real input element in a shadow root, so the form's own
  // submit-on-Enter never fires: Enter is handled here.
  function onkeydown(event: KeyboardEvent) {
    if (event.key === "Enter") unlock(event);
  }

  function cancel() {
    password = "";
    oncancel();
  }

  // Esc and a click outside close the dialog without either button: that is a cancel.
  function ontoggle(event: Event) {
    if ((event as CustomEvent<{ newState: string }>).detail?.newState === "closed" && !answered) cancel();
  }

  const value = (e: Event) => (e.currentTarget as HTMLInputElement).value;
</script>

<fluent-dialog bind:this={dialog} type="modal" aria-label={t("unlock.title")} {ontoggle}>
  <fluent-dialog-body>
    <div slot="title">{t("unlock.title")}</div>
    <form id="unlock" onsubmit={unlock}>
      <p>{t("unlock.text", { name })}</p>
      <fluent-text-input
        type="password"
        value={password}
        oninput={(e: Event) => (password = value(e))}
        {onkeydown}
        required
        autocomplete="off"
      >{t("unlock.password")}</fluent-text-input>
      {#if wrong}
        <fluent-message-bar intent="error" layout="multiline">{t("unlock.wrong")}</fluent-message-bar>
      {/if}
    </form>
    <fluent-button slot="action" appearance="primary" disabled={!password} onclick={unlock}>
      {t("action.import")}
    </fluent-button>
    <fluent-button slot="action" onclick={cancel}>{t("action.cancel")}</fluent-button>
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
    overflow-wrap: anywhere;
  }
</style>

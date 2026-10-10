<script lang="ts">
  import { t } from "$lib/i18n";

  // A yes/no question before something that cannot be undone, or that drops the tunnel.
  let {
    title,
    message,
    action,
    onconfirm,
    oncancel,
  }: {
    title: string;
    message: string;
    /** The label of the button that goes ahead. */
    action: string;
    onconfirm: () => void;
    oncancel: () => void;
  } = $props();

  let dialog: (HTMLElement & { show(): void; hide(): void }) | undefined = $state();
  let answered = false;

  $effect(() => {
    dialog?.show();
  });

  function confirm() {
    answered = true;
    onconfirm();
  }

  // Esc and a click outside close the dialog without either button: that is a "no".
  function ontoggle(event: Event) {
    if ((event as CustomEvent<{ newState: string }>).detail?.newState === "closed" && !answered) oncancel();
  }
</script>

<fluent-dialog bind:this={dialog} type="modal" aria-label={title} {ontoggle}>
  <fluent-dialog-body>
    <div slot="title">{title}</div>
    <p>{message}</p>
    <fluent-button slot="action" appearance="primary" onclick={confirm}>{action}</fluent-button>
    <fluent-button slot="action" onclick={oncancel}>{t("action.cancel")}</fluent-button>
  </fluent-dialog-body>
</fluent-dialog>

<style>
  p {
    margin: 0;
  }
</style>

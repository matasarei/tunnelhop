<script lang="ts">
  // About TunnelHop: its version, its licence, where the third-party notices are, and where it
  // lives on GitHub. As in the runtime help, the app opens no links itself (SPEC §5): each
  // address is shown to copy.
  import { getVersion } from "@tauri-apps/api/app";
  import { t, type Key } from "$lib/i18n";

  let { onclose }: { onclose: () => void } = $props();

  const REPOSITORY = "https://github.com/matasarei/tunnelhop";
  const links: { label: Key; address: string }[] = [
    { label: "about.project", address: REPOSITORY },
    { label: "about.releases", address: `${REPOSITORY}/releases` },
    { label: "about.issues", address: `${REPOSITORY}/issues` },
  ];

  let dialog: (HTMLElement & { show(): void; hide(): void }) | undefined = $state();
  let version = $state("");
  let copied = $state<string | null>(null);
  let closed = false;

  $effect(() => {
    dialog?.show();
  });

  getVersion().then(
    (v) => (version = v),
    // The rest of the dialog is still worth showing without it.
    () => (version = ""),
  );

  async function copy(address: string) {
    try {
      await navigator.clipboard.writeText(address);
      copied = address;
    } catch {
      // The address stays on screen and can be selected by hand.
      copied = null;
    }
  }

  function close() {
    closed = true;
    onclose();
  }

  // Esc and a click outside close the dialog as the button does.
  function ontoggle(event: Event) {
    if ((event as CustomEvent<{ newState: string }>).detail?.newState === "closed" && !closed) close();
  }
</script>

<fluent-dialog bind:this={dialog} type="modal" aria-label={t("about.title")} {ontoggle}>
  <fluent-dialog-body>
    <div slot="title">{t("about.title")}</div>
    <div class="text">
      {#if version}
        <p class="version">{t("about.version", { version })}</p>
      {/if}
      <p>{t("about.description")}</p>
      <p>{t("about.licence")}</p>
      <p class="hint">{t("about.notices")}</p>
      {#each links as link (link.address)}
        <div class="link">
          <span class="label">{t(link.label)}</span>
          <div class="source">
            <code>{link.address}</code>
            <fluent-button size="small" onclick={() => copy(link.address)}>
              {copied === link.address ? t("help.copied") : t("help.copy")}
            </fluent-button>
          </div>
        </div>
      {/each}
    </div>
    <fluent-button slot="action" appearance="primary" onclick={close}>{t("action.close")}</fluent-button>
  </fluent-dialog-body>
</fluent-dialog>

<style>
  .text {
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  p {
    margin: 0;
  }
  .version {
    color: var(--colorNeutralForeground2, #424242);
  }
  .link {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .label {
    font-weight: 600;
  }
  .source {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    padding: 8px 12px;
    border: 1px solid var(--colorNeutralStroke2, #e0e0e0);
    border-radius: var(--borderRadiusMedium, 4px);
    background: var(--colorNeutralBackground3, #f5f5f5);
  }
  code {
    font-family: "Cascadia Mono", Consolas, monospace;
    font-size: 13px;
    overflow-wrap: anywhere;
    user-select: all;
  }
  .hint {
    font-size: 12px;
    color: var(--colorNeutralForeground3, #616161);
  }
</style>

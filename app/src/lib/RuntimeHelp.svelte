<script lang="ts">
  // Why a server is unavailable: its protocol's program is missing or too old on this PC, which
  // one TunnelHop needs, and where it comes from. The app opens no links itself, so the address
  // is shown to copy.
  import { sentences, t, type Key } from "$lib/i18n";
  import { protocolName, runtimeName, type Availability, type Protocol } from "$lib/ipc";

  let {
    protocol,
    availability,
    onclose,
  }: {
    protocol: Protocol;
    /** `missing` or `too_old`; the page closes the dialog once the runtime is available. */
    availability: Availability;
    onclose: () => void;
  } = $props();

  const sources: Record<Protocol, { address: string; pick: Key }> = {
    wireguard: { address: "https://www.wireguard.com/install/", pick: "help.pick.wireguard" },
    openvpn: { address: "https://openvpn.net/community-downloads/", pick: "help.pick.openvpn" },
  };

  let dialog: (HTMLElement & { show(): void; hide(): void }) | undefined = $state();
  let copied = $state(false);
  let closed = false;

  $effect(() => {
    dialog?.show();
  });

  const name = $derived(runtimeName[protocol]);
  const source = $derived(sources[protocol]);
  const title = $derived(
    availability.status === "too_old"
      ? t(availability.found === "unknown" ? "help.title.unchecked" : "help.title.too_old", {
          name,
          found: availability.found,
        })
      : t("runtime.missing", { name }),
  );

  async function copy() {
    try {
      await navigator.clipboard.writeText(source.address);
      copied = true;
    } catch {
      // The address stays on screen and can be selected by hand.
      copied = false;
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

<fluent-dialog bind:this={dialog} type="modal" aria-label={title} {ontoggle}>
  <fluent-dialog-body>
    <div slot="title">{title}</div>
    <div class="text">
      {#if availability.status === "too_old"}
        <p>
          {sentences(
            t(availability.found === "unknown" ? "help.unreadable" : "help.installed", {
              name,
              found: availability.found,
            }),
            t("help.needs", { required: availability.required, protocol: protocolName[protocol] }),
          )}
        </p>
      {:else}
        <p>{t("help.missing", { protocol: protocolName[protocol], name })}</p>
      {/if}
      {#if protocol === "openvpn" && availability.status === "missing"}
        <p>
          <strong>{t("help.connect.strong")}</strong>
          {t("help.connect.rest")}
        </p>
      {/if}
      <p>{t("help.download")}</p>
      <div class="source">
        <code>{source.address}</code>
        <fluent-button size="small" onclick={copy}>{copied ? t("help.copied") : t("help.copy")}</fluent-button>
      </div>
      <p>{sentences(t(source.pick), t("help.defaults"))}</p>
      <p class="hint">{t("help.closes")}</p>
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

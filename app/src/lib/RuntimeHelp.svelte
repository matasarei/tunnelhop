<script lang="ts">
  // Why a server is unavailable: its protocol's program is missing or too old on this PC, which
  // one TunnelHop needs, and where it comes from. The app opens no links itself, so the address
  // is shown to copy.
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

  const sources: Record<Protocol, { address: string; pick: string }> = {
    wireguard: {
      address: "https://www.wireguard.com/install/",
      pick: "Choose the Windows installer.",
    },
    openvpn: {
      address: "https://openvpn.net/community-downloads/",
      pick: "Choose the Windows 64-bit MSI installer; its file name starts with OpenVPN-2.",
    },
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
      ? availability.found === "unknown"
        ? `${name} could not be checked`
        : `${name} ${availability.found} is too old`
      : `${name} is not installed`,
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
          {#if availability.found === "unknown"}
            {name} is installed, but its version could not be read.
          {:else}
            {name} {availability.found} is installed.
          {/if}
          TunnelHop needs {availability.required} or newer for {protocolName[protocol]} servers. Install the
          current release over it.
        </p>
      {:else}
        <p>
          TunnelHop starts and controls {protocolName[protocol]} tunnels itself, and for that it needs
          {name} installed on this PC. It was not found.
        </p>
      {/if}
      {#if protocol === "openvpn"}
        <p>
          <strong>OpenVPN Connect is a different program and cannot be used.</strong> It is what openvpn.net
          and the Microsoft Store offer first. TunnelHop needs the Community edition (version 2.x), which can
          be installed next to it.
        </p>
      {/if}
      <p>Download it from:</p>
      <div class="source">
        <code>{source.address}</code>
        <fluent-button size="small" onclick={copy}>{copied ? "Copied" : "Copy link"}</fluent-button>
      </div>
      <p>{source.pick} Keep the default options.</p>
      <p class="hint">
        TunnelHop notices the new program a few seconds after its installer finishes; this message then
        closes by itself.
      </p>
    </div>
    <fluent-button slot="action" appearance="primary" onclick={close}>Close</fluent-button>
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

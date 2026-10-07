<script lang="ts">
  // The window: the service's state and the same controls as the tray, in Fluent 2 (the
  // design system of Windows 11). Every control sends a request; what the window shows changes
  // only with the service's next report, so it never shows something the service did not do.
  // The profile list, import and bundle management arrive with steps 18, 27 and 28.
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import { onMount } from "svelte";
  import { followSystemTheme } from "$lib/fluent";

  type Report = {
    state: "connected" | "blocked" | "disconnected";
    profile_id: string | null;
    public_ip: string | null;
    killswitch_engaged: boolean;
    bundle: { bundle_name: string; profiles: { id: string; name: string; protocol: string }[] } | null;
    auto_rotate: boolean;
    interval_minutes: number;
  };
  type Action =
    | { type: "connect" }
    | { type: "rotate" }
    | { type: "disconnect" }
    | { type: "set_auto_rotate"; enabled: boolean }
    | { type: "set_interval"; minutes: number }
    | { type: "exit" };

  let report = $state<Report | null>(null);
  let intervals = $state<[number, string][]>([]);
  let busy = $state(false);
  let error = $state<string | null>(null);
  let alert = $state<string | null>(null);

  onMount(() => {
    const stopTheme = followSystemTheme();
    invoke<Report | null>("status").then((r) => (report = r));
    invoke<[number, string][]>("intervals").then((list) => (intervals = list));
    const unlisten = [
      listen<Report | null>("status", (e) => {
        report = e.payload;
        // A tunnel up again answers the last alert.
        if (e.payload?.state === "connected") alert = null;
      }),
      listen<string>("alert", (e) => (alert = e.payload)),
    ];
    return () => {
      stopTheme();
      unlisten.forEach((p) => p.then((stop) => stop()));
    };
  });

  async function act(action: Action) {
    busy = true;
    error = null;
    try {
      await invoke("act", { action });
    } catch (e) {
      error = String(e);
    } finally {
      busy = false;
    }
  }

  const reachable = $derived(report !== null);
  const connected = $derived(report?.state === "connected");
  const blocked = $derived(report?.state === "blocked");
  const engaged = $derived(!!report && (report.killswitch_engaged || report.state !== "disconnected"));
  const profileName = $derived(
    report?.bundle?.profiles.find((p) => p.id === report?.profile_id)?.name ?? report?.profile_id ?? null,
  );
  const title = $derived(
    !report ? "Service not running" : connected ? "Connected" : blocked ? "Blocked" : "Disconnected",
  );
  const subtitle = $derived(
    !report
      ? "Start the TunnelHop service to connect."
      : connected
        ? [profileName, report.public_ip].filter(Boolean).join(" · ")
        : blocked
          ? "Traffic is blocked until a tunnel is up, or you unblock."
          : report.bundle
            ? "Not protected. Connect to pick a server."
            : "Import a bundle to get started.",
  );
  // A bundle may set an interval the menu does not offer; show it rather than a blank choice.
  const intervalChoices = $derived(
    report && !intervals.some(([m]) => m === report!.interval_minutes)
      ? [...intervals, [report.interval_minutes, `${report.interval_minutes} min (from the bundle)`] as [number, string]]
      : intervals,
  );
</script>

<main>
  {#if !reachable}
    <fluent-message-bar intent="warning" layout="multiline">
      The TunnelHop service is not running. The app reconnects to it on its own.
    </fluent-message-bar>
  {/if}
  {#if alert}
    <fluent-message-bar intent="error" layout="multiline">
      {alert}
      <fluent-button slot="dismiss" appearance="transparent" size="small" aria-label="Dismiss" onclick={() => (alert = null)}>✕</fluent-button>
    </fluent-message-bar>
  {/if}
  {#if error}
    <fluent-message-bar intent="error" layout="multiline">
      {error}
      <fluent-button slot="dismiss" appearance="transparent" size="small" aria-label="Dismiss" onclick={() => (error = null)}>✕</fluent-button>
    </fluent-message-bar>
  {/if}

  <section class="card hero">
    <div class="badge" class:connected class:blocked class:off={!connected && !blocked} aria-hidden="true">
      {#if connected}
        <svg viewBox="0 0 24 24"><path d="M12 2 4 5v6c0 5 3.4 9.7 8 11 4.6-1.3 8-6 8-11V5l-8-3Zm-1.3 14.2-3.9-3.9 1.4-1.4 2.5 2.5 5.4-5.4 1.4 1.4-6.8 6.8Z" /></svg>
      {:else if blocked}
        <svg viewBox="0 0 24 24"><path d="M12 2 4 5v6c0 5 3.4 9.7 8 11 4.6-1.3 8-6 8-11V5l-8-3Zm-1 5h2v6h-2V7Zm0 8h2v2h-2v-2Z" /></svg>
      {:else}
        <svg viewBox="0 0 24 24"><path d="M12 2 4 5v6c0 5 3.4 9.7 8 11 4.6-1.3 8-6 8-11V5l-8-3Zm6 9c0 3.9-2.6 7.6-6 8.9-3.4-1.3-6-5-6-8.9V6.4l6-2.3 6 2.3V11Z" /></svg>
      {/if}
    </div>
    <div class="hero-text">
      <div class="title">{title}</div>
      <div class="subtitle">{subtitle}</div>
    </div>
  </section>

  <div class="actions">
    {#if connected || blocked}
      <fluent-button appearance="primary" disabled={!report?.bundle || busy} onclick={() => act({ type: "rotate" })}>
        Rotate now
      </fluent-button>
    {:else}
      <fluent-button appearance="primary" disabled={!report?.bundle || busy} onclick={() => act({ type: "connect" })}>
        Connect
      </fluent-button>
    {/if}
    <fluent-button disabled={!engaged || busy} onclick={() => act({ type: "disconnect" })}>
      Disconnect &amp; unblock
    </fluent-button>
    {#if busy}<fluent-spinner size="tiny" aria-label="Working"></fluent-spinner>{/if}
  </div>

  <h2>Details</h2>
  <section class="card rows">
    <div class="row"><span class="label">Bundle</span><span>{report?.bundle?.bundle_name ?? "None selected"}</span></div>
    <div class="row"><span class="label">Server</span><span>{profileName ?? "—"}</span></div>
    <div class="row"><span class="label">Public IP</span><span>{report?.public_ip ?? "—"}</span></div>
    <div class="row">
      <span class="label">Kill-switch</span>
      <span>{report?.killswitch_engaged ? "On: traffic outside the tunnel is blocked" : "Off"}</span>
    </div>
  </section>

  <h2>Rotation</h2>
  <section class="card rows">
    <div class="row">
      <div>
        <div class="label-strong">Auto-rotation</div>
        <div class="hint">Hop to another server on a timer</div>
      </div>
      <fluent-switch
        checked={report?.auto_rotate ?? false}
        disabled={!reachable || busy}
        aria-label="Auto-rotation"
        onchange={(e: Event) => act({ type: "set_auto_rotate", enabled: (e.currentTarget as HTMLInputElement).checked })}
      ></fluent-switch>
    </div>
    <div class="row">
      <div>
        <div class="label-strong">Interval</div>
        <div class="hint">How often to hop while auto-rotation is on</div>
      </div>
      <fluent-dropdown
        aria-label="Interval"
        disabled={!reachable || busy}
        onchange={(e: Event) => act({ type: "set_interval", minutes: Number((e.currentTarget as HTMLSelectElement).value) })}
      >
        <fluent-listbox>
          {#each intervalChoices as [minutes, label] (minutes)}
            <fluent-option value={String(minutes)} selected={report?.interval_minutes === minutes}>{label}</fluent-option>
          {/each}
        </fluent-listbox>
      </fluent-dropdown>
    </div>
  </section>

  <footer>
    <fluent-button appearance="subtle" onclick={() => act({ type: "exit" })}>Exit TunnelHop</fluent-button>
    <span class="hint">Disconnects, lifts the block, and closes the app. Closing the window keeps it in the tray.</span>
  </footer>
</main>

<style>
  :global(body) {
    margin: 0;
    font-family: "Segoe UI Variable Text", "Segoe UI", system-ui, sans-serif;
    font-size: 14px;
    background: var(--colorNeutralBackground2, #fafafa);
    color: var(--colorNeutralForeground1, #242424);
  }
  main {
    max-width: 640px;
    margin: 0 auto;
    padding: 20px 24px 24px;
    display: flex;
    flex-direction: column;
    gap: 12px;
  }
  h2 {
    font-family: "Segoe UI Variable Display", "Segoe UI", system-ui, sans-serif;
    font-size: 14px;
    font-weight: 600;
    margin: 12px 0 0;
  }
  .card {
    background: var(--colorNeutralBackground1, #fff);
    border: 1px solid var(--colorNeutralStroke2, #e0e0e0);
    border-radius: var(--borderRadiusLarge, 6px);
  }
  .hero {
    display: flex;
    align-items: center;
    gap: 16px;
    padding: 20px;
  }
  .badge {
    width: 48px;
    height: 48px;
    flex: none;
    display: grid;
    place-items: center;
    border-radius: 50%;
  }
  .badge svg {
    width: 28px;
    height: 28px;
    fill: currentColor;
  }
  .badge.connected {
    color: var(--colorPaletteGreenForeground1, #107c10);
    background: var(--colorPaletteGreenBackground1, #f1faf1);
  }
  .badge.blocked {
    color: var(--colorPaletteRedForeground1, #bc2f32);
    background: var(--colorPaletteRedBackground1, #fdf6f6);
  }
  .badge.off {
    color: var(--colorNeutralForeground3, #616161);
    background: var(--colorNeutralBackground3, #f5f5f5);
  }
  .title {
    font-family: "Segoe UI Variable Display", "Segoe UI", system-ui, sans-serif;
    font-size: 20px;
    font-weight: 600;
  }
  .subtitle,
  .hint,
  .label {
    color: var(--colorNeutralForeground3, #616161);
  }
  .hint {
    font-size: 12px;
  }
  .actions {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .rows .row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 16px;
    min-height: 36px;
    padding: 6px 16px;
  }
  .rows .row + .row {
    border-top: 1px solid var(--colorNeutralStroke2, #e0e0e0);
  }
  .label-strong {
    font-weight: 600;
  }
  footer {
    display: flex;
    align-items: center;
    gap: 12px;
    margin-top: 8px;
  }
  footer fluent-button {
    flex: none;
    white-space: nowrap;
  }
</style>

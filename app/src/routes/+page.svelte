<script lang="ts">
  // The app shell's window (step 17): the service's state, live. The dashboard and bundle
  // import arrive in step 18.
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import { onMount } from "svelte";

  type Report = {
    state: "connected" | "blocked" | "disconnected";
    profile_id: string | null;
    public_ip: string | null;
    killswitch_engaged: boolean;
    bundle: { bundle_name: string } | null;
  };

  let report = $state<Report | null>(null);
  let alert = $state<string | null>(null);

  onMount(() => {
    invoke<Report | null>("status").then((r) => (report = r));
    const unlisten = [
      listen<Report | null>("status", (e) => (report = e.payload)),
      listen<string>("alert", (e) => (alert = e.payload)),
    ];
    return () => unlisten.forEach((p) => p.then((stop) => stop()));
  });

  const label = { connected: "Connected", blocked: "Blocked", disconnected: "Disconnected" };
</script>

<main>
  <h1>TunnelHop</h1>
  {#if report === null}
    <p class="muted">The TunnelHop service is not running.</p>
  {:else}
    <dl>
      <dt>State</dt>
      <dd class={report.state}>{label[report.state]}</dd>
      <dt>Bundle</dt>
      <dd>{report.bundle?.bundle_name ?? "none selected"}</dd>
      <dt>Profile</dt>
      <dd>{report.profile_id ?? "—"}</dd>
      <dt>Public IP</dt>
      <dd>{report.public_ip ?? "—"}</dd>
      <dt>Kill-switch</dt>
      <dd>{report.killswitch_engaged ? "engaged" : "off"}</dd>
    </dl>
  {/if}
  {#if alert}
    <p class="alert" role="alert">{alert}</p>
  {/if}
</main>

<style>
  :global(body) {
    margin: 0;
    font-family: system-ui, "Segoe UI", sans-serif;
    background: #f6f6f7;
    color: #1d1d1f;
  }
  @media (prefers-color-scheme: dark) {
    :global(body) {
      background: #1f1f22;
      color: #ececee;
    }
  }
  main {
    padding: 24px;
  }
  dl {
    display: grid;
    grid-template-columns: max-content 1fr;
    gap: 6px 16px;
  }
  dt {
    opacity: 0.7;
  }
  dd {
    margin: 0;
  }
  .connected {
    color: #1a7f37;
  }
  .blocked {
    color: #c62828;
  }
  .muted {
    opacity: 0.7;
  }
  .alert {
    padding: 8px 12px;
    border-left: 3px solid #c62828;
    background: rgba(198, 40, 40, 0.08);
  }
</style>

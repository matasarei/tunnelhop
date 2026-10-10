<script lang="ts">
  // The window: the service's state, the same controls as the tray, the bundle's servers, and
  // bundle import, in Fluent 2 (the design system of Windows 11). Every control sends a request;
  // what the window shows changes only with the service's next report, so it never shows
  // something the service did not do.
  import { listen } from "@tauri-apps/api/event";
  import { getCurrentWebview } from "@tauri-apps/api/webview";
  import { onMount } from "svelte";
  import AddServers from "$lib/AddServers.svelte";
  import Bundles from "$lib/Bundles.svelte";
  import Confirm from "$lib/Confirm.svelte";
  import Credentials from "$lib/Credentials.svelte";
  import Servers from "$lib/Servers.svelte";
  import { followSystemTheme } from "$lib/fluent";
  import * as ipc from "$lib/ipc";
  import type { Action, Bundle, BundleList, Profile, Report } from "$lib/ipc";

  let report = $state<Report | null>(null);
  let intervals = $state<[number, string][]>([]);
  let busy = $state(false);
  let error = $state<string | null>(null);
  let alert = $state<string | null>(null);
  /** What the last import ended in, until dismissed. */
  let notice = $state<string | null>(null);
  let importErrors = $state<string[] | null>(null);
  /** What the errors above stopped: an import, or an edit of the servers. */
  let refusedWhat = $state("The bundle was not imported.");
  /** Config files chosen to become servers, waiting for their names to be confirmed. */
  let adding = $state<{ bundle: Bundle; files: ipc.ChosenFile[] } | null>(null);
  /** An imported bundle not made active yet, because that would drop the current tunnel. */
  let pendingSwitch = $state<Bundle | null>(null);
  /** The credentials dialog: for a whole bundle (profile null) or one server. */
  let asking = $state<{ bundle: Bundle; profile: Profile | null } | null>(null);
  let dragging = $state(false);
  /** The installed bundles; null while the service cannot be asked. */
  let bundles = $state<BundleList | null>(null);
  /** The service is there, but would not list its bundles. */
  let listFailed = $state(false);
  /** A question waiting for a yes or a no. */
  let confirming = $state<{ title: string; message: string; action: string; run: () => void } | null>(null);

  /** Ask for the installed bundles again. Status reports carry only the active one. */
  async function refreshBundles() {
    try {
      bundles = await ipc.listBundles();
      listFailed = false;
    } catch (e) {
      bundles = null;
      // With the service away the window already says so; anything else is worth reading.
      listFailed = report !== null;
      if (listFailed) error = String(e);
    }
  }

  onMount(() => {
    const stopTheme = followSystemTheme();
    ipc.status().then((r) => {
      report = r;
      refreshBundles();
    });
    ipc.intervals().then((list) => (intervals = list));
    const unlisten = [
      listen<Report | null>("status", (e) => {
        const before = report;
        report = e.payload;
        // The service came or went, or another bundle is active: the list may have changed.
        if ((before === null) !== (report === null) || before?.bundle?.bundle_id !== report?.bundle?.bundle_id)
          refreshBundles();
        // A tunnel up again answers the last alert.
        if (e.payload?.state === "connected") alert = null;
        // The bundle waiting for "Switch now" became the active one some other way.
        if (pendingSwitch && e.payload?.bundle?.bundle_id === pendingSwitch.bundle_id) pendingSwitch = null;
      }),
      listen<string>("alert", (e) => (alert = e.payload)),
      // Files dropped on the window arrive as paths; the Rust side reads the file.
      getCurrentWebview().onDragDropEvent((e) => {
        if (e.payload.type === "enter" || e.payload.type === "over") dragging = true;
        else if (e.payload.type === "leave") dragging = false;
        else if (e.payload.type === "drop") {
          dragging = false;
          const paths = e.payload.paths;
          // Config files are servers for the bundle in use; anything else is tried as a bundle.
          if (paths.length > 0 && paths.every((path) => /\.(conf|ovpn)$/i.test(path))) chooseServers(paths);
          else if (paths.length === 1) importFrom(paths[0]);
          else error = "Drop one bundle .zip, or .conf and .ovpn files to add as servers.";
        }
      }),
    ];
    return () => {
      stopTheme();
      unlisten.forEach((p) => p.then((stop) => stop()));
    };
  });

  /** Run one request with the controls disabled; a refusal is shown in the window. */
  async function run<T>(request: () => Promise<T>): Promise<T | undefined> {
    busy = true;
    error = null;
    try {
      return await request();
    } catch (e) {
      error = String(e);
    } finally {
      busy = false;
    }
  }

  const act = (action: Action) => run(() => ipc.act(action));

  /** Import the dropped file, or one from the native picker when no path is given. */
  async function importFrom(path?: string) {
    // The button is disabled in both cases; a drop is not, and must not vanish without a word.
    if (!report) {
      error = "The TunnelHop service is not running, so the bundle was not imported.";
      return;
    }
    if (busy) {
      error = "The bundle was not imported: another request was still running. Drop it again.";
      return;
    }
    importErrors = null;
    notice = null;
    const outcome = await run(() => ipc.importBundle(path));
    if (!outcome || outcome.type === "cancelled") return;
    if (outcome.type === "refused") {
      refusedWhat = "The bundle was not imported.";
      importErrors = outcome.errors;
      return;
    }
    const { bundle } = outcome;
    const count = bundle.profiles.length;
    notice = outcome.already_present
      ? `"${bundle.bundle_name}" is already installed.`
      : `Imported "${bundle.bundle_name}" (${count} ${count === 1 ? "server" : "servers"}).`;
    if (report?.bundle?.bundle_id !== bundle.bundle_id) {
      // Switching disconnects the current bundle first: with a tunnel up, that is the user's call.
      if (report?.state === "disconnected") await run(() => ipc.selectBundle(bundle.bundle_id));
      else pendingSwitch = bundle;
    }
    refreshBundles();
    if (outcome.needs_credentials.length > 0) asking = { bundle, profile: null };
  }

  /** Offer the dropped config files, or ones from the native picker, as servers to add. */
  async function chooseServers(paths?: string[]) {
    const bundle = report?.bundle;
    if (!bundle) {
      error = report
        ? "Servers are added to the bundle in use: import a bundle or switch to one first."
        : "The TunnelHop service is not running, so the servers were not added.";
      return;
    }
    if (busy) {
      error = "The servers were not added: another request was still running. Drop them again.";
      return;
    }
    const files = await run(() => ipc.chooseServers(paths));
    if (files?.length) adding = { bundle, files };
  }

  /** Run an edit of a bundle's servers and show what it ended in, as an import does. */
  async function edit(refused: string, done: (bundle: Bundle) => string, request: () => Promise<ipc.ImportOutcome>) {
    importErrors = null;
    notice = null;
    const outcome = await run(request);
    if (!outcome || outcome.type === "cancelled") return;
    if (outcome.type === "refused") {
      refusedWhat = refused;
      importErrors = outcome.errors;
      return;
    }
    notice = done(outcome.bundle);
    refreshBundles();
    if (outcome.needs_credentials.length > 0) asking = { bundle: outcome.bundle, profile: null };
  }

  function addServers(bundle: Bundle, files: ipc.ChosenFile[]) {
    adding = null;
    const what = files.length === 1 ? `"${files[0].name || "the server"}"` : `${files.length} servers`;
    edit(
      files.length === 1 ? "The server was not added." : "The servers were not added.",
      (edited) => `Added ${what} to "${edited.bundle_name}".`,
      () => ipc.addServers(bundle.bundle_id, files),
    );
  }

  function askRemove(profile: Profile) {
    const bundle = report?.bundle;
    if (!bundle) return;
    const inUse = connected && report?.profile_id === profile.id;
    const moves = inUse
      ? "You are connected through it: TunnelHop moves to another server first. "
      : connected
        ? "The tunnel reconnects once. "
        : "";
    confirming = {
      title: `Remove "${profile.name}"?`,
      message: `${moves}${connected ? "The block stays in place meanwhile. " : ""}It is removed from "${bundle.bundle_name}" with its saved sign-in.`,
      action: "Remove",
      run: () =>
        edit(
          "The server was not removed.",
          (edited) => `Removed "${profile.name}" from "${edited.bundle_name}".`,
          () => ipc.removeServers(bundle.bundle_id, [profile.id]),
        ),
    };
  }

  async function switchTo(bundle: Bundle) {
    pendingSwitch = null;
    await run(() => ipc.selectBundle(bundle.bundle_id));
  }

  /** Close the question, then act on a yes. The question is taken before it is cleared. */
  function answer(yes: boolean) {
    const question = confirming;
    confirming = null;
    if (yes) question?.run();
  }

  const leavesTunnel = "The current tunnel is disconnected and the block is lifted first.";

  /** Switch from the bundles list: asked about first when it would drop a tunnel or a block. */
  function askSwitch(bundle: Bundle) {
    if (!engaged) return void switchTo(bundle);
    confirming = {
      title: `Switch to "${bundle.bundle_name}"?`,
      message: `${leavesTunnel} Traffic is not protected until you connect again.`,
      action: "Switch",
      run: () => switchTo(bundle),
    };
  }

  function askDelete(bundle: Bundle) {
    const inUse = bundles?.active === bundle.bundle_id && engaged;
    confirming = {
      title: `Delete "${bundle.bundle_name}"?`,
      message: `${inUse ? `${leavesTunnel} ` : ""}Its servers and saved sign-in are removed from this PC. This cannot be undone.`,
      action: "Delete",
      run: () => remove(bundle),
    };
  }

  async function remove(bundle: Bundle) {
    notice = null;
    const left = await run(() => ipc.deleteBundle(bundle.bundle_id));
    if (!left) return void refreshBundles();
    bundles = left;
    notice = `Deleted "${bundle.bundle_name}".`;
    if (pendingSwitch?.bundle_id === bundle.bundle_id) pendingSwitch = null;
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
            : bundles?.bundles.length
              ? "No bundle is in use. Switch to one below."
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
  {#if importErrors}
    <fluent-message-bar intent="error" layout="multiline">
      <div>
        <strong>{refusedWhat}</strong>
        {importErrors.length === 1 ? "One problem was found:" : `${importErrors.length} problems were found:`}
        <ul class="errors">
          {#each importErrors as line}<li>{line}</li>{/each}
        </ul>
      </div>
      <fluent-button slot="dismiss" appearance="transparent" size="small" aria-label="Dismiss" onclick={() => (importErrors = null)}>✕</fluent-button>
    </fluent-message-bar>
  {/if}
  {#if notice}
    <fluent-message-bar intent="success" layout="multiline">
      {notice}
      <fluent-button slot="dismiss" appearance="transparent" size="small" aria-label="Dismiss" onclick={() => (notice = null)}>✕</fluent-button>
    </fluent-message-bar>
  {/if}
  {#if pendingSwitch}
    {@const bundle = pendingSwitch}
    <fluent-message-bar intent="info" layout="multiline">
      {#if engaged}
        Switching to "{bundle.bundle_name}" disconnects the current tunnel and lifts the block first.
      {:else}
        "{bundle.bundle_name}" is installed, but not the bundle in use.
      {/if}
      <fluent-button slot="actions" size="small" disabled={busy} onclick={() => switchTo(bundle)}>Switch now</fluent-button>
      <fluent-button slot="dismiss" appearance="transparent" size="small" aria-label="Dismiss" onclick={() => (pendingSwitch = null)}>✕</fluent-button>
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

  {#if report?.bundle}
    <div class="heading">
      <h2>Servers in {report.bundle.bundle_name}</h2>
      <fluent-button size="small" disabled={busy} onclick={() => chooseServers()}>Add server…</fluent-button>
    </div>
    <Servers
      {report}
      {busy}
      onconnect={(profile) => act({ type: "connect", profile_id: profile.id })}
      oncredentials={(profile) => report?.bundle && (asking = { bundle: report.bundle, profile })}
      onremove={askRemove}
    />
  {/if}

  <div class="heading">
    <h2>Bundles</h2>
    <fluent-button size="small" disabled={!reachable || busy} onclick={() => importFrom()}>Import bundle…</fluent-button>
  </div>
  {#if bundles && bundles.bundles.length > 0}
    <Bundles list={bundles} {busy} onswitch={askSwitch} ondelete={askDelete} />
  {:else if listFailed}
    <section class="card empty">The installed bundles could not be listed.</section>
  {:else if reachable}
    <section class="card empty">
      No bundle yet. Import a bundle <code>.zip</code> from your VPN provider, or drop it on this window.
    </section>
  {:else}
    <section class="card empty">The installed bundles appear here once the service is running.</section>
  {/if}

  <h2>Details</h2>
  <section class="card rows">
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

{#if asking}
  <Credentials bundle={asking.bundle} profile={asking.profile} onclose={() => (asking = null)} />
{/if}

{#if adding}
  {@const chosen = adding}
  <AddServers
    bundle={chosen.bundle}
    files={chosen.files}
    reconnects={connected}
    onadd={(files) => addServers(chosen.bundle, files)}
    oncancel={() => (adding = null)}
  />
{/if}

{#if confirming}
  {@const question = confirming}
  <Confirm
    title={question.title}
    message={question.message}
    action={question.action}
    onconfirm={() => answer(true)}
    oncancel={() => answer(false)}
  />
{/if}

{#if dragging}
  <div class="drop" aria-hidden="true"><div>Drop a bundle <code>.zip</code> to import it, or config files to add as servers</div></div>
{/if}

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
  .heading {
    display: flex;
    align-items: flex-end;
    justify-content: space-between;
    gap: 12px;
    margin-top: 12px;
  }
  .heading h2 {
    margin: 0;
  }
  .empty {
    padding: 16px;
    color: var(--colorNeutralForeground3, #616161);
  }
  .errors {
    margin: 6px 0 0;
    padding-left: 18px;
    max-height: 160px;
    overflow: auto;
    font-family: "Cascadia Mono", Consolas, monospace;
    font-size: 12px;
  }
  .drop {
    position: fixed;
    inset: 0;
    display: grid;
    place-items: center;
    background: color-mix(in srgb, var(--colorNeutralBackground2, #fafafa) 85%, transparent);
    pointer-events: none;
  }
  .drop div {
    padding: 24px 32px;
    border: 2px dashed var(--colorBrandStroke1, #0f6cbd);
    border-radius: var(--borderRadiusLarge, 6px);
    background: var(--colorNeutralBackground1, #fff);
    font-size: 16px;
  }
</style>

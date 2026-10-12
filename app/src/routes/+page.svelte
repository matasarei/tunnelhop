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
  import Export from "$lib/Export.svelte";
  import RuntimeHelp from "$lib/RuntimeHelp.svelte";
  import About from "$lib/About.svelte";
  import Servers from "$lib/Servers.svelte";
  import Unlock from "$lib/Unlock.svelte";
  import { disabledAttribute, followSystemTheme } from "$lib/fluent";
  import { afterInstall } from "$lib/install";
  import { sentences, t, tn } from "$lib/i18n";
  import * as ipc from "$lib/ipc";
  import type { Action, Bundle, BundleList, Profile, Protocol, Report } from "$lib/ipc";

  let report = $state<Report | null>(null);
  let intervals = $state<[number, string][]>([]);
  let busy = $state(false);
  let error = $state<string | null>(null);
  let alert = $state<string | null>(null);
  /** What the last import ended in, until dismissed. */
  let notice = $state<string | null>(null);
  let importErrors = $state<string[] | null>(null);
  /** What the errors above stopped: an import, or an edit of the servers. */
  let refusedWhat = $state(t("refused.import"));
  /**
   * Config files chosen to become servers, waiting for their names to be confirmed: of the
   * bundle in use, or (null) of a new bundle when none is.
   */
  let adding = $state<{ bundle: Bundle | null; files: ipc.ChosenFile[] } | null>(null);
  /** An imported bundle not made active yet, because that would drop the current tunnel. */
  let pendingSwitch = $state<Bundle | null>(null);
  /** The credentials dialog: for a whole bundle (profile null) or one server. */
  let asking = $state<{ bundle: Bundle; profile: Profile | null; refused?: boolean } | null>(null);
  /** A protected bundle waiting for its password. */
  let unlocking = $state<ipc.Locked | null>(null);
  /** The bundle the export dialog is open for. */
  let exporting = $state<Bundle | null>(null);
  /** The protocol whose missing or outdated program is being explained. */
  let explaining = $state<Protocol | null>(null);
  /** Whether the About dialog is open. */
  let about = $state(false);
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
        // A server that had a sign-in and now needs one has just refused it: ask for a new
        // one, as after an import. Its "Sign in" button stays if this is dismissed.
        const now = report?.bundle;
        const was = before?.bundle;
        if (now && was?.bundle_id === now.bundle_id && !asking) {
          const refused = now.profiles.find(
            (p) => !p.has_credentials && was.profiles.find((old) => old.id === p.id)?.has_credentials,
          );
          if (refused) asking = { bundle: now, profile: refused, refused: true };
        }
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
          // Config files are servers, for the bundle in use or a new one; anything else is
          // tried as a bundle.
          if (paths.length > 0 && paths.every((path) => /\.(conf|ovpn)$/i.test(path))) chooseServers(paths);
          else if (paths.length === 1) importFrom(paths[0]);
          else error = t("error.drop");
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

  /**
   * Import the dropped file, or one from the native picker when no path is given; a protected
   * one again with its `password`.
   */
  async function importFrom(path?: string, password?: string) {
    // The button is disabled in both cases; a drop is not, and must not vanish without a word.
    if (!report) {
      error = t("error.import.no_service");
      return;
    }
    if (busy) {
      error = t("error.import.busy");
      return;
    }
    importErrors = null;
    notice = null;
    const outcome = await run(() => ipc.importBundle(path, password));
    if (!outcome || outcome.type === "cancelled") return;
    if (outcome.type === "locked") {
      unlocking = outcome;
      return;
    }
    if (outcome.type === "refused") {
      refusedWhat = t("refused.import");
      importErrors = outcome.errors;
      return;
    }
    await installed(outcome, "notice.imported");
  }

  /** What follows a bundle that was just installed: say so, put it in use, ask for sign-in. */
  async function installed(
    outcome: Extract<ipc.ImportOutcome, { type: "imported" }>,
    how: "notice.imported" | "notice.created",
  ) {
    const { bundle } = outcome;
    notice = outcome.already_present
      ? t("notice.already", { bundle: bundle.bundle_name })
      : tn(how, bundle.profiles.length, { bundle: bundle.bundle_name });
    const next = afterInstall(report, bundle.bundle_id);
    if (next === "switch") await run(() => ipc.selectBundle(bundle.bundle_id));
    else if (next === "offer") pendingSwitch = bundle;
    refreshBundles();
    if (outcome.needs_credentials.length > 0) asking = { bundle, profile: null };
  }

  /**
   * Offer the dropped config files, or ones from the native picker, as servers: added to the
   * bundle in use, or made into a new bundle when none is.
   */
  async function chooseServers(paths?: string[]) {
    if (!report) {
      error = t("error.servers.no_service");
      return;
    }
    const bundle = report.bundle;
    if (busy) {
      error = t("error.servers.busy");
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

  /** Make a new bundle of the chosen files; the service puts it in use when none is. */
  async function createBundle(name: string, files: ipc.ChosenFile[]) {
    adding = null;
    importErrors = null;
    notice = null;
    const outcome = await run(() => ipc.createBundle(name, files));
    if (!outcome || outcome.type === "cancelled") return;
    if (outcome.type === "refused") {
      refusedWhat = t("refused.create");
      importErrors = outcome.errors;
      return;
    }
    await installed(outcome, "notice.created");
  }

  function addServers(bundle: Bundle, files: ipc.ChosenFile[]) {
    adding = null;
    const added = (to: string) =>
      files.length > 1
        ? tn("notice.added", files.length, { bundle: to })
        : files[0].name
          ? t("notice.added.named", { name: files[0].name, bundle: to })
          : t("notice.added.unnamed", { bundle: to });
    edit(
      t(files.length === 1 ? "refused.add_single" : "refused.add_several"),
      (edited) => added(edited.bundle_name),
      () => ipc.addServers(bundle.bundle_id, files),
    );
  }

  function askRemove(profile: Profile) {
    const bundle = report?.bundle;
    if (!bundle) return;
    const inUse = connected && report?.profile_id === profile.id;
    confirming = {
      title: t("confirm.remove.title", { name: profile.name }),
      message: sentences(
        inUse ? t("confirm.remove.moves") : connected && t("confirm.remove.reconnects"),
        connected && t("confirm.block_stays"),
        t("confirm.remove.result", { bundle: bundle.bundle_name }),
      ),
      action: t("action.remove"),
      run: () =>
        edit(
          t("refused.remove"),
          (edited) => t("notice.removed", { name: profile.name, bundle: edited.bundle_name }),
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

  /** Switch from the bundles list: asked about first when it would drop a tunnel or a block. */
  function askSwitch(bundle: Bundle) {
    if (!engaged) return void switchTo(bundle);
    confirming = {
      title: t("confirm.switch.title", { bundle: bundle.bundle_name }),
      message: sentences(t("confirm.leaves_tunnel"), t("confirm.switch.unprotected")),
      action: t("action.switch"),
      run: () => switchTo(bundle),
    };
  }

  function askDelete(bundle: Bundle) {
    const inUse = bundles?.active === bundle.bundle_id && engaged;
    confirming = {
      title: t("confirm.delete.title", { bundle: bundle.bundle_name }),
      message: sentences(inUse && t("confirm.leaves_tunnel"), t("confirm.delete.result")),
      action: t("action.delete"),
      run: () => remove(bundle),
    };
  }

  function askExport(bundle: Bundle) {
    notice = null;
    exporting = bundle;
  }

  async function remove(bundle: Bundle) {
    notice = null;
    const left = await run(() => ipc.deleteBundle(bundle.bundle_id));
    if (!left) return void refreshBundles();
    bundles = left;
    notice = t("notice.deleted", { bundle: bundle.bundle_name });
    if (pendingSwitch?.bundle_id === bundle.bundle_id) pendingSwitch = null;
  }

  /** What stops the explained protocol; null once its program is there, which closes the dialog. */
  const unusable = $derived.by(() => {
    const found = report?.runtimes.find((r) => r.protocol === explaining)?.availability;
    return explaining && found && found.status !== "available" ? { protocol: explaining, availability: found } : null;
  });
  $effect(() => {
    if (explaining && !unusable) explaining = null;
  });

  const reachable = $derived(report !== null);
  const connected = $derived(report?.state === "connected");
  const blocked = $derived(report?.state === "blocked");
  /** There is another server to rotate to: without one, the rotation controls are greyed out. */
  const rotatable = $derived(report?.rotatable ?? false);
  const engaged = $derived(!!report && (report.killswitch_engaged || report.state !== "disconnected"));
  const profileName = $derived(
    report?.bundle?.profiles.find((p) => p.id === report?.profile_id)?.name ?? report?.profile_id ?? null,
  );
  const title = $derived(
    !report
      ? t("tray.status.unreachable")
      : connected
        ? t("status.connected")
        : blocked
          ? t("tray.status.blocked")
          : t("tray.status.disconnected"),
  );
  const subtitle = $derived(
    !report
      ? t("status.hint.unreachable")
      : connected
        ? [profileName, report.public_ip].filter(Boolean).join(" · ")
        : blocked
          ? t("status.hint.blocked")
          : report.bundle
            ? t("status.hint.disconnected")
            : bundles?.bundles.length
              ? t("status.hint.no_bundle")
              : t("status.hint.empty"),
  );
  // A bundle may set an interval the menu does not offer; show it rather than a blank choice.
  const intervalChoices = $derived(
    report && !intervals.some(([m]) => m === report!.interval_minutes)
      ? [
          ...intervals,
          [
            report.interval_minutes,
            t("interval.from_bundle", { label: t("interval.minutes", { n: report.interval_minutes }) }),
          ] as [number, string],
        ]
      : intervals,
  );
</script>

<!-- Drawn, not typed: a character sits wherever its font puts it, an icon in the middle. -->
{#snippet cross()}
  <svg class="cross" viewBox="0 0 12 12" aria-hidden="true"><path d="M1.5 1.5l9 9m0-9l-9 9" /></svg>
{/snippet}

<main>
  {#if !reachable}
    <fluent-message-bar intent="warning" layout="multiline">
      {t("bar.unreachable")}
    </fluent-message-bar>
  {/if}
  {#if alert}
    <fluent-message-bar intent="error" layout="multiline">
      {alert}
      <fluent-button slot="dismiss" appearance="transparent" size="small" icon-only aria-label={t("bar.dismiss")} onclick={() => (alert = null)}>{@render cross()}</fluent-button>
    </fluent-message-bar>
  {/if}
  <!-- A failed connect comes back twice, as the answer and inside the service's alert. -->
  {#if error && !alert?.includes(error)}
    <fluent-message-bar intent="error" layout="multiline">
      {error}
      <fluent-button slot="dismiss" appearance="transparent" size="small" icon-only aria-label={t("bar.dismiss")} onclick={() => (error = null)}>{@render cross()}</fluent-button>
    </fluent-message-bar>
  {/if}
  {#if importErrors}
    <fluent-message-bar intent="error" layout="multiline">
      <div>
        <strong>{refusedWhat}</strong>
        {tn("bar.problems", importErrors.length)}
        <ul class="errors">
          {#each importErrors as line}<li>{line}</li>{/each}
        </ul>
      </div>
      <fluent-button slot="dismiss" appearance="transparent" size="small" icon-only aria-label={t("bar.dismiss")} onclick={() => (importErrors = null)}>{@render cross()}</fluent-button>
    </fluent-message-bar>
  {/if}
  {#if notice}
    <fluent-message-bar intent="success" layout="multiline">
      {notice}
      <fluent-button slot="dismiss" appearance="transparent" size="small" icon-only aria-label={t("bar.dismiss")} onclick={() => (notice = null)}>{@render cross()}</fluent-button>
    </fluent-message-bar>
  {/if}
  {#if pendingSwitch}
    {@const bundle = pendingSwitch}
    <fluent-message-bar intent="info" layout="multiline">
      {#if engaged}
        {t("bar.switch.engaged", { bundle: bundle.bundle_name })}
      {:else}
        {t("bar.switch.idle", { bundle: bundle.bundle_name })}
      {/if}
      <fluent-button slot="actions" size="small" disabled={busy} onclick={() => switchTo(bundle)}>{t("bar.switch.now")}</fluent-button>
      <fluent-button slot="dismiss" appearance="transparent" size="small" icon-only aria-label={t("bar.dismiss")} onclick={() => (pendingSwitch = null)}>{@render cross()}</fluent-button>
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
    <fluent-button class="about" appearance="transparent" size="small" icon-only aria-label={t("about.label")} title={t("about.label")} onclick={() => (about = true)}>
      <svg viewBox="0 0 20 20" aria-hidden="true"><circle cx="10" cy="10" r="8" /><path d="M7.6 7.7a2.5 2.5 0 1 1 3.4 2.3c-.6.3-1 .8-1 1.5v.6M10 14.3v.2" /></svg>
    </fluent-button>
  </section>

  <div class="actions">
    <!-- Blocked with one server: Connect is the way out, since there is nothing to rotate to. -->
    {#if connected || (blocked && rotatable)}
      <fluent-button
        appearance="primary"
        disabled={!report?.bundle || busy || !rotatable}
        onclick={() => act({ type: "rotate" })}
      >
        {t("action.rotate")}
      </fluent-button>
    {:else}
      <fluent-button appearance="primary" disabled={!report?.bundle || busy} onclick={() => act({ type: "connect" })}>
        {t("action.connect")}
      </fluent-button>
    {/if}
    <fluent-button disabled={!engaged || busy} onclick={() => act({ type: "disconnect" })}>
      {t("action.disconnect")}
    </fluent-button>
    {#if busy}<fluent-spinner size="tiny" aria-label={t("action.working")}></fluent-spinner>{/if}
  </div>

  {#if report?.bundle}
    <div class="heading">
      <h2>{t("servers.heading", { bundle: report.bundle.bundle_name })}</h2>
      <fluent-button size="small" disabled={busy} onclick={() => chooseServers()}>{t("servers.add_single")}</fluent-button>
    </div>
    <Servers
      {report}
      {busy}
      onconnect={(profile) => act({ type: "connect", profile_id: profile.id })}
      oncredentials={(profile) => report?.bundle && (asking = { bundle: report.bundle, profile })}
      onremove={askRemove}
      onwhy={(profile) => (explaining = profile.protocol)}
    />
  {/if}

  <div class="heading">
    <h2>{t("bundles.heading")}</h2>
    <div class="buttons">
      {#if reachable && !report?.bundle}
        <fluent-button size="small" disabled={busy} onclick={() => chooseServers()}>{t("servers.add_several")}</fluent-button>
      {/if}
      <fluent-button size="small" disabled={!reachable || busy} onclick={() => importFrom()}>{t("bundles.import")}</fluent-button>
    </div>
  </div>
  {#if bundles && bundles.bundles.length > 0}
    <Bundles list={bundles} {busy} onswitch={askSwitch} onexport={askExport} ondelete={askDelete} />
  {:else if listFailed}
    <section class="card empty">{t("bundles.list_failed")}</section>
  {:else if reachable}
    <section class="card empty">{t("bundles.empty")}</section>
  {:else}
    <section class="card empty">{t("bundles.waiting")}</section>
  {/if}

  <h2>{t("details.heading")}</h2>
  <section class="card rows">
    <div class="row"><span class="label">{t("details.ip")}</span><span>{report?.public_ip ?? "—"}</span></div>
    <div class="row">
      <span class="label">{t("details.killswitch")}</span>
      <span>{report?.killswitch_engaged ? t("details.killswitch.on") : t("details.killswitch.off")}</span>
    </div>
  </section>

  <h2>{t("rotation.heading")}</h2>
  {#if reachable && !rotatable}
    <p class="hint single">{t("rotation.single")}</p>
  {/if}
  <section class="card rows">
    <div class="row">
      <div>
        <div class="label-strong">{t("rotation.auto")}</div>
        <div class="hint">{t("rotation.auto.hint")}</div>
      </div>
      <fluent-switch
        checked={report?.auto_rotate ?? false}
        use:disabledAttribute={!reachable || busy || !rotatable}
        aria-label={t("rotation.auto")}
        onchange={(e: Event) => act({ type: "set_auto_rotate", enabled: (e.currentTarget as HTMLInputElement).checked })}
      ></fluent-switch>
    </div>
    <div class="row">
      <div>
        <div class="label-strong">{t("rotation.interval")}</div>
        <div class="hint">{t("rotation.interval.hint")}</div>
      </div>
      <fluent-dropdown
        aria-label={t("rotation.interval")}
        disabled={!reachable || busy || !rotatable}
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
    <fluent-button appearance="subtle" onclick={() => act({ type: "exit" })}>{t("exit.button")}</fluent-button>
    <span class="hint">{t("exit.hint")}</span>
  </footer>
</main>

{#if asking}
  <Credentials bundle={asking.bundle} profile={asking.profile} refused={asking.refused} onclose={() => (asking = null)} />
{/if}

{#if unlocking}
  {@const locked = unlocking}
  <Unlock
    name={locked.path.split(/[\\/]/).pop() ?? locked.path}
    wrong={locked.wrong}
    onunlock={(password) => {
      // Read first: `locked` follows `unlocking`, and is gone once that is cleared.
      const { path } = locked;
      unlocking = null;
      importFrom(path, password);
    }}
    oncancel={() => (unlocking = null)}
  />
{/if}

{#if exporting}
  {@const bundle = exporting}
  <Export
    {bundle}
    onclose={(saved) => {
      // Read first: `bundle` follows `exporting`, and is gone once that is cleared.
      const name = bundle.bundle_name;
      exporting = null;
      if (saved) notice = t("notice.exported", { bundle: name });
    }}
  />
{/if}

{#if about}
  <About onclose={() => (about = false)} />
{/if}

{#if unusable}
  <RuntimeHelp protocol={unusable.protocol} availability={unusable.availability} onclose={() => (explaining = null)} />
{/if}

{#if adding}
  {@const chosen = adding}
  <AddServers
    bundle={chosen.bundle}
    files={chosen.files}
    reconnects={connected}
    onadd={(files, bundleName) =>
      chosen.bundle ? addServers(chosen.bundle, files) : createBundle(bundleName, files)}
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
  <div class="drop" aria-hidden="true"><div>{t("drop.overlay")}</div></div>
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
  /* Help sits in the card's corner, out of the way of the status it describes. */
  .about {
    margin-left: auto;
    align-self: flex-start;
    margin-top: -8px;
    margin-right: -8px;
  }
  .about svg {
    width: 20px;
    height: 20px;
    fill: none;
    stroke: currentColor;
    stroke-width: 1.5;
    stroke-linecap: round;
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
  /* A small button is 24px high and a line of the message 20px: centre the cross on the line.
     Fluent's multi-line bar pins the button's content to its top edge; put it back in the middle. */
  fluent-button[slot="dismiss"] {
    align-items: center;
    height: 24px;
    margin-block: -2px;
    margin-inline-end: -4px;
  }
  .cross {
    width: 12px;
    height: 12px;
    fill: none;
    stroke: currentColor;
    stroke-width: 1.2;
    stroke-linecap: round;
  }
  /* Fluent keeps room for action buttons under the text even when a bar has none. */
  fluent-message-bar:not(:has([slot="actions"])) {
    padding-block-end: 0;
  }
  .single {
    margin: -4px 0 8px;
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
  .buttons {
    display: flex;
    gap: 8px;
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

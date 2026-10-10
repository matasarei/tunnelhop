<script lang="ts">
  // The config files about to become servers (SPEC §3.5), each with the name it will show
  // under: of the bundle in use, or of a new bundle when there is none. Nothing has been read
  // yet: the files are read when the button is pressed.
  import { untrack } from "svelte";
  import { sentences, t, tn } from "$lib/i18n";
  import type { Bundle, ChosenFile } from "$lib/ipc";

  let {
    bundle,
    files,
    reconnects,
    onadd,
    oncancel,
  }: {
    /** The bundle that gets the servers; null to make a new bundle of them. */
    bundle: Bundle | null;
    files: ChosenFile[];
    /** A tunnel is up: the edit reconnects it. */
    reconnects: boolean;
    /** `bundleName` is the new bundle's name, and unused when adding to one. */
    onadd: (files: ChosenFile[], bundleName: string) => void;
    oncancel: () => void;
  } = $props();

  let dialog: (HTMLElement & { show(): void; hide(): void }) | undefined = $state();
  // The names start as offered and are the user's from then on.
  let names = $state(untrack(() => files.map((file) => file.name)));
  let bundleName = $state(t("add.default_name"));
  let answered = false;

  $effect(() => {
    dialog?.show();
  });

  const fileName = (path: string) => path.split(/[\\/]/).pop() ?? path;

  function add() {
    answered = true;
    onadd(
      files.map((file, i) => ({ path: file.path, name: names[i].trim() })),
      bundleName.trim(),
    );
  }

  // Fluent's inputs keep the real input element in a shadow root, so Enter is handled here.
  function onkeydown(event: KeyboardEvent) {
    if (event.key === "Enter") add();
  }

  // Esc and a click outside close the dialog without either button: that is Cancel.
  function ontoggle(event: Event) {
    if ((event as CustomEvent<{ newState: string }>).detail?.newState === "closed" && !answered) oncancel();
  }
</script>

<fluent-dialog bind:this={dialog} type="modal" aria-label={bundle ? t("picker.servers.title") : t("add.label.bundle")} {ontoggle}>
  <fluent-dialog-body>
    <div slot="title">
      {#if bundle}
        {tn("add.title", files.length, { bundle: bundle.bundle_name })}
      {:else}
        {tn("add.new_title", files.length)}
      {/if}
    </div>
    {#if !bundle}
      <fluent-text-input
        class="bundle-name"
        value={bundleName}
        oninput={(e: Event) => (bundleName = (e.currentTarget as HTMLInputElement).value)}
        {onkeydown}
        maxlength="100"
        autocomplete="off"
        spellcheck="false"
      >{t("add.bundle_name")}</fluent-text-input>
    {/if}
    <div class="files">
      {#each files as file, i (file.path)}
        <fluent-text-input
          value={names[i]}
          oninput={(e: Event) => (names[i] = (e.currentTarget as HTMLInputElement).value)}
          {onkeydown}
          maxlength="100"
          autocomplete="off"
          spellcheck="false"
        >{fileName(file.path)}</fluent-text-input>
      {/each}
    </div>
    <p class="hint">{sentences(t("add.hint"), reconnects && t("add.reconnects"))}</p>
    <fluent-button slot="action" appearance="primary" onclick={add}>{bundle ? t("action.add") : t("action.create")}</fluent-button>
    <fluent-button slot="action" onclick={oncancel}>{t("action.cancel")}</fluent-button>
  </fluent-dialog-body>
</fluent-dialog>

<style>
  .bundle-name {
    display: block;
    margin-bottom: 16px;
  }
  .files {
    display: flex;
    flex-direction: column;
    gap: 12px;
    max-height: 280px;
    overflow: auto;
    padding: 2px;
  }
  .hint {
    margin: 12px 0 0;
    font-size: 12px;
    color: var(--colorNeutralForeground3, #616161);
  }
</style>

<script lang="ts">
  // The config files about to become servers of a bundle (SPEC §3.5), each with the name it
  // will show under. Nothing has been read yet: the files are read when Add is pressed.
  import { untrack } from "svelte";
  import type { Bundle, ChosenFile } from "$lib/ipc";

  let {
    bundle,
    files,
    reconnects,
    onadd,
    oncancel,
  }: {
    bundle: Bundle;
    files: ChosenFile[];
    /** A tunnel is up: the edit reconnects it. */
    reconnects: boolean;
    onadd: (files: ChosenFile[]) => void;
    oncancel: () => void;
  } = $props();

  let dialog: (HTMLElement & { show(): void; hide(): void }) | undefined = $state();
  // The names start as offered and are the user's from then on.
  let names = $state(untrack(() => files.map((file) => file.name)));
  let answered = false;

  $effect(() => {
    dialog?.show();
  });

  const fileName = (path: string) => path.split(/[\\/]/).pop() ?? path;

  function add() {
    answered = true;
    onadd(files.map((file, i) => ({ path: file.path, name: names[i].trim() })));
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

<fluent-dialog bind:this={dialog} type="modal" aria-label="Add servers" {ontoggle}>
  <fluent-dialog-body>
    <div slot="title">
      {files.length === 1 ? "Add a server" : `Add ${files.length} servers`} to {bundle.bundle_name}
    </div>
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
    <p class="hint">
      The name is what the server list shows. Each file is checked like an imported bundle; a
      file that could run a program is refused.
      {#if reconnects}The tunnel reconnects once; the block stays in place meanwhile.{/if}
    </p>
    <fluent-button slot="action" appearance="primary" onclick={add}>Add</fluent-button>
    <fluent-button slot="action" onclick={oncancel}>Cancel</fluent-button>
  </fluent-dialog-body>
</fluent-dialog>

<style>
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

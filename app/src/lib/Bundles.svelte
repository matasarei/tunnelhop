<script lang="ts">
  // The installed bundles (SPEC §3.5): which one is in use, switch to another, delete one.
  import type { Bundle, BundleList } from "$lib/ipc";

  let {
    list,
    busy,
    onswitch,
    ondelete,
  }: {
    list: BundleList;
    busy: boolean;
    onswitch: (bundle: Bundle) => void;
    ondelete: (bundle: Bundle) => void;
  } = $props();

  const servers = (count: number) => (count === 1 ? "1 server" : `${count} servers`);
  // Bundles may share a name (a provider's file downloaded twice): those also show their id.
  const shared = $derived(
    new Set(list.bundles.map((b) => b.bundle_name).filter((name, i, all) => all.indexOf(name) !== i)),
  );
</script>

<section class="card rows">
  {#each list.bundles as bundle (bundle.bundle_id)}
    <div class="row">
      <div class="who">
        <div class="name">{bundle.bundle_name}</div>
        <div class="hint">
          {servers(bundle.profiles.length)}{#if shared.has(bundle.bundle_name)}&nbsp;· {bundle.bundle_id.slice(0, 8)}{/if}
        </div>
      </div>
      <div class="what">
        {#if bundle.bundle_id === list.active}
          <fluent-badge appearance="tint" color="success">In use</fluent-badge>
        {:else if bundle.profiles.length > 0}
          <fluent-button size="small" appearance="subtle" disabled={busy} onclick={() => onswitch(bundle)}>
            Switch
          </fluent-button>
        {/if}
        <fluent-button
          size="small"
          appearance="subtle"
          disabled={busy}
          aria-label={`Delete ${bundle.bundle_name}`}
          onclick={() => ondelete(bundle)}
        >
          Delete
        </fluent-button>
      </div>
    </div>
  {/each}
</section>

<style>
  .card {
    background: var(--colorNeutralBackground1, #fff);
    border: 1px solid var(--colorNeutralStroke2, #e0e0e0);
    border-radius: var(--borderRadiusLarge, 6px);
  }
  .row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 16px;
    min-height: 36px;
    padding: 6px 16px;
  }
  .row + .row {
    border-top: 1px solid var(--colorNeutralStroke2, #e0e0e0);
  }
  .who {
    min-width: 0;
  }
  .name {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .hint {
    font-size: 12px;
    color: var(--colorNeutralForeground3, #616161);
  }
  .what {
    display: flex;
    align-items: center;
    gap: 8px;
    flex: none;
  }
</style>

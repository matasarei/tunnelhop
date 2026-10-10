<script lang="ts">
  // The active bundle's servers: connect to one, remove one, and see which cannot be used yet
  // (waiting for credentials, or a runtime that is missing or too old) and why.
  import { protocolName, runtimeProblem, type Profile, type Report } from "$lib/ipc";

  let {
    report,
    busy,
    onconnect,
    oncredentials,
    onremove,
  }: {
    report: Report;
    busy: boolean;
    onconnect: (profile: Profile) => void;
    oncredentials: (profile: Profile) => void;
    onremove: (profile: Profile) => void;
  } = $props();

  const profiles = $derived(report.bundle?.profiles ?? []);
</script>

<section class="card rows">
  {#each profiles as profile (profile.id)}
    {@const problem = runtimeProblem(report, profile.protocol)}
    {@const current = report.state === "connected" && report.profile_id === profile.id}
    <div class="row">
      <div class="who">
        <div class="name">{profile.name}</div>
        <div class="hint">
          {protocolName[profile.protocol]}{#if problem}&nbsp;· {problem}{/if}
        </div>
      </div>
      <div class="what">
        {#if current}
          <fluent-badge appearance="tint" color="success">Connected</fluent-badge>
        {:else if problem}
          <fluent-badge appearance="tint" color="severe">Unavailable</fluent-badge>
        {:else if !profile.has_credentials}
          <fluent-badge appearance="tint" color="warning">Needs sign-in</fluent-badge>
          <fluent-button size="small" disabled={busy} onclick={() => oncredentials(profile)}>
            Sign in…
          </fluent-button>
        {:else}
          <fluent-button size="small" appearance="subtle" disabled={busy} onclick={() => onconnect(profile)}>
            Connect
          </fluent-button>
        {/if}
        <!-- A bundle keeps at least one server: the last one goes with the bundle. -->
        {#if profiles.length > 1}
          <fluent-button
            size="small"
            appearance="subtle"
            disabled={busy}
            aria-label={`Remove ${profile.name}`}
            onclick={() => onremove(profile)}
          >
            Remove
          </fluent-button>
        {/if}
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

<script lang="ts">
  // The volumes shelf, read-only: what the catalog knows about every drive,
  // card and archive it has ever been shown, whether or not one is plugged
  // in right now. Nothing here mounts, forgets or renames a volume — the
  // mutating verbs stay with the CLI and MCP this phase.
  import { api, errorMessage } from "./api";
  import type { VolumesOutcome } from "./api";
  import { isoDay } from "./format";
  import Notices from "./Notices.svelte";
  import OnlineBadge from "./OnlineBadge.svelte";

  let outcome = $state<VolumesOutcome | null>(null);
  let error = $state<string | null>(null);

  $effect(() => {
    void load();
  });

  async function load() {
    try {
      outcome = await api.listVolumes();
    } catch (failure) {
      error = errorMessage(failure);
    }
  }
</script>

<div class="surface">
  <h2 class="surface-title">Volumes</h2>

  <Notices notices={outcome?.notices} />

  {#if error}
    <p class="error" role="alert">{error}</p>
  {:else if outcome}
    {#if outcome.volumes.length === 0}
      <p class="empty">No volumes yet — scan one to put it in the catalog.</p>
    {:else}
      <table class="volume-table">
        <thead>
          <tr>
            <th scope="col">Volume</th>
            <th scope="col">Status</th>
            <th scope="col">Assets</th>
            <!-- "Last seen", not "last verified": `VolumeRow` carries the
                 last time the volume was observed mounted, which is not the
                 same claim as the bytes having been re-hashed. -->
            <th scope="col">Last seen</th>
          </tr>
        </thead>
        <tbody>
          <!-- Keyed by id: `volumes` is `SELECT id, … FROM volumes` and id is
               that table's PRIMARY KEY, so two rows cannot share one. -->
          {#each outcome.volumes as volume (volume.id)}
            <tr>
              <td>
                <span class="label">{volume.label}</span>
                <span class="volume-id">{volume.id}</span>
              </td>
              <td><OnlineBadge online={volume.online} /></td>
              <td class="asset-count">{volume.asset_count}</td>
              <td>
                {isoDay(volume.last_seen_ms)}
                {#if volume.clock_suspect}
                  <span
                    class="suspect"
                    title="Last seen is later than this machine's clock allows — the writing machine's clock was probably wrong."
                    >⚠ clock suspect</span
                  >
                {/if}
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
    {/if}
  {/if}
</div>

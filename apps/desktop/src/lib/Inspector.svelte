<script lang="ts">
  // Phase 7B task 8 builds the skeleton: identity, tags, PARA and where the
  // asset lives. Task 9 adds verification state, the metadata field table and
  // the keyframe strip.
  import { api, errorMessage } from "./api";
  import type { AssetDetail } from "./api";
  import { thumbUrl } from "./thumb";

  let { assetId }: { assetId: string | null } = $props();

  let detail = $state<AssetDetail | null>(null);
  let missing = $state<string | null>(null);
  let error = $state<string | null>(null);
  /** Same rule as the search surface: a slow lookup for an asset the user has
   *  already clicked past must not replace the one they are looking at. */
  let requestSeq = 0;
  /** The instance the header describes — they share one asset's bytes, so any
   *  of them gives the same name, size and date. */
  let primary = $derived(detail?.instances[0] ?? null);

  $effect(() => {
    void load(assetId);
  });

  async function load(id: string | null) {
    const seq = ++requestSeq;
    detail = null;
    missing = null;
    error = null;
    if (id === null) return;
    try {
      const found = await api.getAsset(id);
      if (seq !== requestSeq) return;
      if (found === null) {
        missing = id;
        return;
      }
      detail = found;
    } catch (failure) {
      if (seq !== requestSeq) return;
      error = errorMessage(failure);
    }
  }

  function basename(path: string): string {
    const cut = path.lastIndexOf("/");
    return cut === -1 ? path : path.slice(cut + 1);
  }

  const UNITS = ["B", "KB", "MB", "GB", "TB", "PB"];

  function fileSize(bytes: number): string {
    let value = bytes;
    let unit = 0;
    while (value >= 1024 && unit < UNITS.length - 1) {
      value /= 1024;
      unit += 1;
    }
    const rounded = unit === 0 ? String(value) : value.toFixed(1);
    return `${rounded} ${UNITS[unit] ?? "B"}`;
  }

  function day(ms: number): string {
    return new Date(ms).toISOString().slice(0, 10);
  }
</script>

{#if assetId !== null}
  <aside class="inspector">
    {#if error}
      <p class="error" role="alert">{error}</p>
    {:else if missing !== null}
      <p class="missing">
        This catalog knows no asset {missing}.
      </p>
    {:else if detail}
      {#if detail.has_thumb}
        <img class="preview" src={thumbUrl(detail.asset)} alt="" />
      {/if}

      {#if primary}
        <h2 class="name">{basename(primary.path)}</h2>
        <p class="size">{fileSize(primary.size)} · {day(primary.mtime_ms)}</p>
      {/if}
      <p class="asset-id">{detail.asset}</p>

      <!-- Unkeyed for the same reason as the search surface: one outcome can
           carry the same notice twice, and a keyed each throws on a repeat. -->
      {#each detail.notices ?? [] as notice}
        <p class="notice">{notice}</p>
      {/each}

      {#if detail.para !== null}
        <p class="para">{detail.para}</p>
      {/if}

      {#if detail.tags.length > 0}
        <ul class="tags">
          {#each detail.tags as tag (tag)}
            <li>{tag}</li>
          {/each}
        </ul>
      {/if}

      <ul class="instances">
        {#each detail.instances as instance}
          <li>
            <span class="badge"
              >{instance.volume_label}{instance.online ? "●" : "○"}</span
            >
            <span class="path">{instance.path}</span>
          </li>
        {/each}
      </ul>
    {/if}
  </aside>
{/if}

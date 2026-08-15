<script lang="ts">
  // Everything the catalog knows about one asset: identity, tags, PARA,
  // where its copies live, whether its bytes have been verified, and the
  // keyframes the index found in it.
  import { api, errorMessage, errorNotices } from "./api";
  import type { AssetDetail, AssetVerification } from "./api";
  import { isoDay, timecode } from "./format";
  import Notices from "./Notices.svelte";
  import OnlineBadge from "./OnlineBadge.svelte";
  import { fetchKeyframes, keyframeImageUrl, thumbUrl } from "./thumb";
  import type { KeyframeManifest } from "./thumb";

  let { assetId }: { assetId: string | null } = $props();

  let detail = $state<AssetDetail | null>(null);
  let missing = $state<string | null>(null);
  let error = $state<string | null>(null);
  /** From `commands::CommandError.notices` — the lookup goes over `invoke`,
   *  so a failure here (unlike `keyframeError` below, a `thumb://` fetch that
   *  never carries a `CommandError`) can genuinely carry notices. */
  let failureNotices = $state<string[]>([]);
  let keyframes = $state<KeyframeManifest | null>(null);
  /** Why the keyframe manifest could not be read. A plain absence (404) is
   *  not a failure and leaves this null: most assets are stills and have no
   *  manifest at all. */
  let keyframeError = $state<string | null>(null);
  /** Indices of the strip whose `<img>` 404'd (extraction hasn't reached that
   *  timestamp yet, or never will). Tracked so those slots fall back to the
   *  plain timecode chip instead of a broken-image icon. */
  let failedKeyframes = $state<number[]>([]);
  /** Same rule as the search surface: a slow lookup for an asset the user has
   *  already clicked past must not replace the one they are looking at. */
  let requestSeq = 0;
  /** The instance the header describes — they share one asset's bytes, so any
   *  of them gives the same name, size and date. */
  let primary = $derived(detail?.instances[0] ?? null);
  /** Verifications newest first. They arrive as the plain observations the
   *  catalog recorded, in no particular order — `AssetVerification`'s own
   *  doc says a caller that wants the newest sorts by `hashdate_ms` — and
   *  the newest is what answers "is this asset still good". */
  let history = $derived(newestFirst(detail?.verifications ?? []));
  let latest = $derived(history[0] ?? null);

  $effect(() => {
    void load(assetId);
  });

  async function load(id: string | null) {
    const seq = ++requestSeq;
    detail = null;
    missing = null;
    error = null;
    failureNotices = [];
    keyframes = null;
    keyframeError = null;
    failedKeyframes = [];
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
      failureNotices = errorNotices(failure);
      return;
    }
    await loadKeyframes(id, seq);
  }

  /** The manifest rides the `thumb://` protocol rather than IPC, so its
   *  failures are separate from the command's and never take the panel over:
   *  a strip that cannot be drawn must not hide the asset behind it. */
  async function loadKeyframes(id: string, seq: number) {
    try {
      const manifest = await fetchKeyframes(id);
      if (seq !== requestSeq) return;
      keyframes = manifest;
    } catch (failure) {
      if (seq !== requestSeq) return;
      keyframeError = errorMessage(failure);
    }
  }

  /** Sorting a fresh copy mutates nothing shared; `toSorted` would need a
   *  newer lib target than this app builds against. */
  function newestFirst(records: AssetVerification[]): AssetVerification[] {
    // eslint-disable-next-line unicorn/no-array-sort -- see above.
    return [...records].sort((a, b) => b.hashdate_ms - a.hashdate_ms);
  }

  /** Falls the strip slot at `index` back to the timecode chip. Idempotent:
   *  a broken `<img>` can fire `error` more than once. */
  function markKeyframeFailed(index: number) {
    if (!failedKeyframes.includes(index)) failedKeyframes.push(index);
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
</script>

{#if assetId !== null}
  <aside class="inspector">
    {#if error}
      <Notices notices={failureNotices} />
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
        <p class="size">{fileSize(primary.size)} · {isoDay(primary.mtime_ms)}</p>
      {/if}
      <p class="asset-id">{detail.asset}</p>

      <Notices notices={detail.notices} />

      {#if detail.para !== null}
        <p class="para">{detail.para}</p>
      {/if}

      {#if detail.tags.length > 0}
        <ul class="tags">
          <!-- Keyed: `projection.tags` hands back a `BTreeSet`, so an asset
               cannot carry the same tag twice. -->
          {#each detail.tags as tag (tag)}
            <li>{tag}</li>
          {/each}
        </ul>
      {/if}

      {#if Object.keys(detail.fields).length > 0}
        <dl class="fields">
          <!-- Keyed by name: `fields` reaches the wire through
               `meta::serialize_pairs_as_map`, so a name arrives once. -->
          {#each Object.entries(detail.fields) as [name, value] (name)}
            <dt>{name}</dt>
            <dd>{value}</dd>
          {/each}
        </dl>
      {/if}

      <section class="verify">
        <h3>Verification</h3>
        {#if latest === null}
          <p class="verify-state">Never verified</p>
        {:else}
          <p class="verify-state">
            {latest.outcome} · {isoDay(latest.hashdate_ms)}
          </p>
          <details>
            <summary>Full history ({history.length})</summary>
            <ul class="verifications">
              {#each history as record}
                <li>
                  {record.outcome} · {isoDay(record.hashdate_ms)} · {record.path}
                </li>
              {/each}
            </ul>
          </details>
        {/if}
      </section>

      {#if keyframeError !== null}
        <p class="notice">{keyframeError}</p>
      {:else if keyframes !== null && keyframes.timestamps.length > 0}
        <section class="keyframes">
          <h3>Keyframes</h3>
          <!-- One extracted image per timestamp, addressed by its position
               in `timestamps` (`keyframeImageUrl`). A slot whose image 404s
               (extraction hasn't reached it yet, or never will) falls back
               to the plain timecode chip instead of a broken-image icon. -->
          <ul class="strip">
            {#each keyframes.timestamps as ts, index (ts)}
              <li>
                {#if failedKeyframes.includes(index)}
                  <span class="timecode">{timecode(ts)}</span>
                {:else}
                  <img
                    class="keyframe"
                    src={keyframeImageUrl(assetId, index)}
                    alt={timecode(ts)}
                    width="64"
                    height="48"
                    onerror={() => markKeyframeFailed(index)}
                  />
                {/if}
              </li>
            {/each}
          </ul>
          {#if keyframes.detected > keyframes.timestamps.length}
            <p class="notice">
              {keyframes.timestamps.length} of {keyframes.detected} detected keyframes
              indexed
            </p>
          {/if}
        </section>
      {/if}

      <ul class="instances">
        {#each detail.instances as instance}
          <li>
            <OnlineBadge label={instance.volume_label} online={instance.online} />
            <span class="path">{instance.path}</span>
          </li>
        {/each}
      </ul>
    {/if}
  </aside>
{/if}

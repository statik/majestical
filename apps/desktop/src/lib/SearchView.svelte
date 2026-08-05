<script lang="ts">
  import { onDestroy } from "svelte";
  import { api, errorMessage } from "./api";
  import type { SavedSearch, SearchOutcome } from "./api";
  import { thumbUrl } from "./thumb";

  let { onselect }: { onselect: (assetId: string) => void } = $props();

  /** Long enough that a typed word is one search, short enough to feel live. */
  const DEBOUNCE_MS = 200;

  let query = $state("");
  let outcome = $state<SearchOutcome | null>(null);
  let saved = $state<SavedSearch[]>([]);
  let savedNotices = $state<string[]>([]);
  let error = $state<string | null>(null);
  let debounce: ReturnType<typeof setTimeout> | undefined;
  /**
   * Every search takes the next number. A response whose number is no longer
   * the current one answers a query the user has already replaced, and must
   * not land on the surface however late it arrives.
   */
  let requestSeq = 0;

  $effect(() => {
    void loadSaved();
  });

  // A timer surviving the component would search a surface nobody is looking
  // at any more.
  onDestroy(() => clearTimeout(debounce));

  async function loadSaved() {
    try {
      const searches = await api.listSavedSearches();
      saved = searches.saved;
      savedNotices = searches.notices ?? [];
    } catch (failure) {
      error = errorMessage(failure);
    }
  }

  function queueSearch(text: string) {
    clearTimeout(debounce);
    const trimmed = text.trim();
    if (!trimmed) {
      // An empty box owns the surface too: taking the next number cancels an
      // in-flight search for the text just deleted, which would otherwise
      // render results — or an error — under a box asking for nothing.
      requestSeq += 1;
      outcome = null;
      error = null;
      return;
    }
    debounce = setTimeout(
      () => void runSearch(() => api.searchAssets(trimmed)),
      DEBOUNCE_MS,
    );
  }

  async function runSearch(call: () => Promise<SearchOutcome>) {
    const seq = ++requestSeq;
    error = null;
    try {
      const result = await call();
      if (seq !== requestSeq) return;
      outcome = result;
    } catch (failure) {
      if (seq !== requestSeq) return;
      // The failed query owns the surface: leaving the previous query's count
      // and grid under the error would attribute those results to this one.
      outcome = null;
      error = errorMessage(failure);
    }
  }

  /** `@MmSSs`, the timecode `maj search` prints for a keyframe hit. */
  function timecode(ms: number): string {
    const minutes = Math.floor(ms / 60_000);
    const seconds = Math.floor((ms % 60_000) / 1000);
    return `@${minutes}m${String(seconds).padStart(2, "0")}s`;
  }
</script>

<div class="surface">
  <input
    type="search"
    class="omnibox"
    aria-label="Search the catalog"
    placeholder="Search — terms and key:value filters"
    bind:value={query}
    oninput={() => queueSearch(query)}
  />

  {#if saved.length > 0}
    <div class="chips">
      {#each saved as search (search.name)}
        <button
          class="chip"
          title={search.query}
          onclick={() => void runSearch(() => api.runSavedSearch(search.name))}
        >
          {search.name}
        </button>
      {/each}
    </div>
  {/if}

  <!-- Notice lists are deliberately unkeyed: the same notice can legitimately
       appear twice in one outcome, and a keyed each throws on a repeat. -->
  {#each savedNotices as notice}
    <p class="notice">{notice}</p>
  {/each}

  {#if error}
    <p class="error" role="alert">{error}</p>
  {/if}

  {#if outcome}
    <p class="count">{outcome.count} results</p>

    {#each outcome.notices ?? [] as notice}
      <p class="notice">{notice}</p>
    {/each}
    {#if outcome.semantic_coverage && outcome.semantic_coverage.embedded < outcome.semantic_coverage.eligible}
      <p class="notice">
        semantic index: {outcome.semantic_coverage.embedded} of {outcome
          .semantic_coverage.eligible} eligible assets
      </p>
    {/if}
    {#each outcome.text_coverage ?? [] as coverage (coverage.source)}
      <p class="notice">
        {coverage.label}: {coverage.covered} of {coverage.eligible}
        {coverage.noun} — {coverage.remedy}
      </p>
    {/each}

    <ul class="grid">
      {#each outcome.results as hit (hit.asset)}
        <li>
          <button class="card" onclick={() => onselect(hit.asset)}>
            {#if hit.known}
              <img src={thumbUrl(hit.asset)} alt="" loading="lazy" />
              <span class="name">{hit.name}</span>
              <span class="volumes">
                {#each hit.volumes as volume}
                  <span class="badge">{volume.label}{volume.online ? "●" : "○"}</span>
                {/each}
              </span>
              {#if hit.timestamp_ms !== undefined}
                <span class="timecode">{timecode(hit.timestamp_ms)}</span>
              {/if}
              {#if hit.snippet}
                <span class="snippet">“{hit.snippet}”</span>
              {/if}
            {:else}
              <!-- The catalog no longer knows this asset, so every other field
                   on the hit is a placeholder; the id is all that is true. -->
              <span class="name unknown">{hit.asset}</span>
            {/if}
          </button>
        </li>
      {/each}
    </ul>
  {/if}
</div>

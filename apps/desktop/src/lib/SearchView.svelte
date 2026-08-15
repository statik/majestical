<script lang="ts">
  import { onDestroy } from "svelte";
  import { api, errorMessage, errorNotices } from "./api";
  import type { SavedSearch, SearchOutcome } from "./api";
  import { timecode } from "./format";
  import Notices from "./Notices.svelte";
  import OnlineBadge from "./OnlineBadge.svelte";
  import type { SelectionState } from "./selection";
  import { EMPTY_SELECTION, clickSelection, reconcileSelection } from "./selection";
  import SelectionBar from "./SelectionBar.svelte";
  import { thumbUrl } from "./thumb";

  let {
    onselect,
  }: {
    /** Show this asset in the inspector. Only a plain click asks for it:
     *  the modified clicks build this surface's own multi-selection, which
     *  is a different selection and none of the inspector's business. */
    onselect: (assetId: string) => void;
  } = $props();

  /** Long enough that a typed word is one search, short enough to feel live. */
  const DEBOUNCE_MS = 200;

  let query = $state("");
  let outcome = $state<SearchOutcome | null>(null);
  let saved = $state<SavedSearch[]>([]);
  let savedNotices = $state<string[]>([]);
  let error = $state<string | null>(null);
  let failureNotices = $state<string[]>([]);
  let debounce: ReturnType<typeof setTimeout> | undefined;
  /**
   * Every search takes the next number. A response whose number is no longer
   * the current one answers a query the user has already replaced, and must
   * not land on the surface however late it arrives.
   */
  let requestSeq = 0;
  /** The results picked out for a bulk action — this surface's own
   *  selection, kept apart from the inspector's by `selection.ts`, and
   *  reconciled against every new set of results. */
  let selection = $state<SelectionState>(EMPTY_SELECTION);

  /** What the grid is drawing, in the order it draws it. */
  let results = $derived(outcome?.results ?? []);
  /** The selected assets in that same order — what the assignment verbs are
   *  handed. */
  let picked = $derived(
    results.filter((hit) => selection.selected.has(hit.asset)).map((h) => h.asset),
  );

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
      failureNotices = errorNotices(failure);
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
      setResults(null);
      error = null;
      failureNotices = [];
      return;
    }
    debounce = setTimeout(
      () => void runSearch(() => api.searchAssets(trimmed)),
      DEBOUNCE_MS,
    );
  }

  /**
   * Every path that replaces the results goes through here, so a selection
   * can never outlive the cards it was made over: a new query that no longer
   * finds an asset drops it, and a bulk action can only ever reach what the
   * user is looking at.
   */
  function setResults(next: SearchOutcome | null) {
    outcome = next;
    selection = reconcileSelection(
      selection,
      (next?.results ?? []).map((hit) => hit.asset),
    );
  }

  /**
   * A click on a result. Which asset the inspector shows and which assets
   * the bar acts on are two different selections; `clickSelection` is where
   * the modifier keys decide between them.
   */
  function clickCard(assetId: string, event: MouseEvent) {
    const order = results.map((hit) => hit.asset);
    const click = clickSelection(selection, order, assetId, event);
    selection = click.state;
    if (click.inspect !== null) onselect(click.inspect);
  }

  async function runSearch(call: () => Promise<SearchOutcome>) {
    const seq = ++requestSeq;
    error = null;
    failureNotices = [];
    try {
      const result = await call();
      if (seq !== requestSeq) return;
      setResults(result);
    } catch (failure) {
      if (seq !== requestSeq) return;
      // The failed query owns the surface: leaving the previous query's count
      // and grid under the error would attribute those results to this one.
      setResults(null);
      error = errorMessage(failure);
      failureNotices = errorNotices(failure);
    }
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

  <Notices notices={savedNotices} />

  {#if error}
    <Notices notices={failureNotices} />
    <p class="error" role="alert">{error}</p>
  {/if}

  <!-- The live region is always in the document, empty between searches: a
       `role="status"` element created together with its text is not reliably
       announced, so what changes has to be the region's contents. -->
  <div role="status">
    {#if outcome}
      <p class="count">{outcome.count} results</p>

      <Notices notices={outcome.notices} />
      {#if outcome.semantic_coverage && outcome.semantic_coverage.embedded < outcome.semantic_coverage.eligible}
        <p class="notice">
          semantic index: {outcome.semantic_coverage.embedded} of {outcome
            .semantic_coverage.eligible} eligible assets
        </p>
      {/if}
      <!-- Keyed by source: one notice per `TEXT_SOURCE_INFO` entry, so the
           source key cannot repeat. -->
      {#each outcome.text_coverage ?? [] as coverage (coverage.source)}
        <p class="notice">
          {coverage.label}: {coverage.covered} of {coverage.eligible}
          {coverage.noun} — {coverage.remedy}
        </p>
      {/each}
    {/if}
  </div>

  {#if outcome}
    <ul class="grid">
      {#each results as hit (hit.asset)}
        <li>
          <!-- `aria-pressed` only on the cards in the set: an unpressed
               toggle on every card would announce a selection state this
               grid does not have until one is made. -->
          <button
            class="card"
            class:card-picked={selection.selected.has(hit.asset)}
            aria-pressed={selection.selected.has(hit.asset) ? "true" : undefined}
            onclick={(event) => clickCard(hit.asset, event)}
          >
            {#if hit.known}
              <img src={thumbUrl(hit.asset)} alt="" loading="lazy" />
              <span class="name">{hit.name}</span>
              <span class="volumes">
                {#each hit.volumes as volume}
                  <OnlineBadge label={volume.label} online={volume.online} />
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

  <!-- Outside the results: the bar draws itself only when two or more cards
       are picked, and the set it counts is already reconciled against
       whatever the grid is showing. -->
  <SelectionBar selected={picked} onclear={() => (selection = EMPTY_SELECTION)} />
</div>

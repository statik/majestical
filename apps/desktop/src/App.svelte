<script lang="ts">
  import { api, errorMessage } from "./lib/api";
  import type { AppStatus } from "./lib/api";
  import Inspector from "./lib/Inspector.svelte";
  import SearchView from "./lib/SearchView.svelte";
  import VolumesView from "./lib/VolumesView.svelte";
  import Welcome from "./lib/Welcome.svelte";

  type Surface = "search" | "volumes";

  let status = $state<AppStatus | null>(null);
  let error = $state<string | null>(null);
  let selected = $state<string | null>(null);
  let surface = $state<Surface>("search");

  $effect(() => {
    void loadStatus();
  });

  async function loadStatus() {
    try {
      status = await api.appStatus();
    } catch (failure) {
      error = errorMessage(failure);
    }
  }

  /** Clearing the error first puts the shell back on its loading line, so a
   *  retry that fails the same way is visibly a second attempt. */
  async function retry() {
    error = null;
    await loadStatus();
  }

  /**
   * The inspector describes a search result, and this phase has no way to
   * reach an asset from the volumes surface — so leaving Search drops the
   * selection rather than parking a panel beside a table it has nothing to
   * do with. Coming back to Search starts from an empty box either way: the
   * surface is unmounted while you are on Volumes.
   */
  function show(next: Surface) {
    surface = next;
    selected = null;
  }
</script>

<main>
  {#if error}
    <p class="error" role="alert">{error}</p>
    <button class="retry" onclick={() => void retry()}>Retry</button>
  {:else if status === null}
    <p class="loading">Opening the catalog…</p>
  {:else if status.catalog_ready}
    <div class="shell" class:with-inspector={selected !== null}>
      <!-- Layout C's sidebar: the surfaces this phase ships, then the catalog
           these surfaces are reading. -->
      <nav class="sidebar">
        <h1 class="brand">Majestical</h1>
        <ul class="surfaces">
          <li>
            <button
              aria-current={surface === "search" ? "page" : undefined}
              onclick={() => show("search")}>Search</button
            >
          </li>
          <li>
            <button
              aria-current={surface === "volumes" ? "page" : undefined}
              onclick={() => show("volumes")}>Volumes</button
            >
          </li>
        </ul>
        <p class="catalog-path" title={status.catalog_path}>
          {status.catalog_path}
        </p>
      </nav>
      {#if surface === "search"}
        <SearchView onselect={(asset) => (selected = asset)} />
      {:else}
        <VolumesView />
      {/if}
      <Inspector assetId={selected} />
    </div>
  {:else}
    <Welcome oninitialized={(next) => (status = next)} />
  {/if}
</main>

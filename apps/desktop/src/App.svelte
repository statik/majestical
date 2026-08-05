<script lang="ts">
  import { api, errorMessage } from "./lib/api";
  import type { AppStatus } from "./lib/api";
  import Inspector from "./lib/Inspector.svelte";
  import SearchView from "./lib/SearchView.svelte";
  import Welcome from "./lib/Welcome.svelte";

  let status = $state<AppStatus | null>(null);
  let error = $state<string | null>(null);
  let selected = $state<string | null>(null);

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
</script>

<main>
  {#if error}
    <p class="error" role="alert">{error}</p>
  {:else if status === null}
    <p class="loading">Opening the catalog…</p>
  {:else if status.catalog_ready}
    <div class="shell" class:with-inspector={selected !== null}>
      <!-- Layout C's sidebar. Search is the only surface this phase has, so it
           is a label rather than a switcher; task 9 adds Volumes beside it. -->
      <nav class="sidebar">
        <h1 class="brand">Majestical</h1>
        <ul class="surfaces">
          <li aria-current="page">Search</li>
        </ul>
        <p class="catalog-path" title={status.catalog_path}>
          {status.catalog_path}
        </p>
      </nav>
      <SearchView onselect={(asset) => (selected = asset)} />
      <Inspector assetId={selected} />
    </div>
  {:else}
    <Welcome oninitialized={(next) => (status = next)} />
  {/if}
</main>

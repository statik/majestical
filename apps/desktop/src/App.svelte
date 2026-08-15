<script lang="ts">
  import { api, errorMessage, errorNotices } from "./lib/api";
  import type { AppStatus } from "./lib/api";
  import BrowseView from "./lib/BrowseView.svelte";
  import Inspector from "./lib/Inspector.svelte";
  import Notices from "./lib/Notices.svelte";
  import OrganizeView from "./lib/OrganizeView.svelte";
  import SearchView from "./lib/SearchView.svelte";
  import UpdateBanner from "./lib/UpdateBanner.svelte";
  import VolumesView from "./lib/VolumesView.svelte";
  import Welcome from "./lib/Welcome.svelte";

  type Surface = "search" | "browse" | "organize" | "volumes";

  let status = $state<AppStatus | null>(null);
  let error = $state<string | null>(null);
  let failureNotices = $state<string[]>([]);
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
      failureNotices = errorNotices(failure);
    }
  }

  /** Clearing the error first puts the shell back on its loading line, so a
   *  retry that fails the same way is visibly a second attempt. */
  async function retry() {
    error = null;
    failureNotices = [];
    await loadStatus();
  }

  /**
   * The inspector describes the asset a surface handed it, and no other
   * surface can reach that same asset — so switching drops the selection
   * rather than parking a panel beside something it has nothing to do with.
   * The surface left behind is unmounted, so coming back starts empty
   * either way.
   */
  function show(next: Surface) {
    surface = next;
    selected = null;
  }
</script>

<main>
  <!-- Mounted only once `app_status` has answered, so the update check can
       never be what the first paint is waiting on. It outlives the branch
       below: an update is worth offering whether or not a catalog is open. -->
  {#if status !== null}
    <UpdateBanner />
  {/if}
  {#if error}
    <Notices notices={failureNotices} />
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
              aria-current={surface === "browse" ? "page" : undefined}
              onclick={() => show("browse")}>Browse</button
            >
          </li>
          <li>
            <button
              aria-current={surface === "organize" ? "page" : undefined}
              onclick={() => show("organize")}>Organize</button
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
      {:else if surface === "browse"}
        <BrowseView
          onselect={(asset) => (selected = asset)}
          inspectorOpen={selected !== null}
        />
      {:else if surface === "organize"}
        <!-- No `onselect`: Organize manages the taxonomy, and nothing on it
             addresses an asset, so it never opens the inspector. -->
        <OrganizeView />
      {:else}
        <VolumesView />
      {/if}
      <Inspector assetId={selected} />
    </div>
  {:else}
    <Welcome oninitialized={(next) => (status = next)} />
  {/if}
</main>

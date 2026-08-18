<script lang="ts">
  import { onDestroy } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import type { UnlistenFn } from "@tauri-apps/api/event";
  import { api, errorMessage, errorNotices, INGEST_PROGRESS_EVENT } from "./lib/api";
  import type { AppStatus, IngestProgress } from "./lib/api";
  import BrowseView from "./lib/BrowseView.svelte";
  import IngestView from "./lib/IngestView.svelte";
  import Inspector from "./lib/Inspector.svelte";
  import Notices from "./lib/Notices.svelte";
  import OrganizeView from "./lib/OrganizeView.svelte";
  import SearchView from "./lib/SearchView.svelte";
  import UpdateBanner from "./lib/UpdateBanner.svelte";
  import VolumesView from "./lib/VolumesView.svelte";
  import Welcome from "./lib/Welcome.svelte";

  type Surface = "search" | "browse" | "ingest" | "organize" | "volumes";

  /** How often the shell re-asks whether a stopped run has finished
   *  finishing — the sweep and the MHL generations land after the copy
   *  loop ends, and the marker stays lit until they have. */
  const INGEST_POLL_MS = 200;

  let status = $state<AppStatus | null>(null);
  let error = $state<string | null>(null);
  let failureNotices = $state<string[]>([]);
  let selected = $state<string | null>(null);
  let surface = $state<Surface>("search");
  /**
   * Whether an ingest run is going, for the sidebar's marker. Learned from
   * the progress stream rather than a read on mount, because the run
   * outlives this webview and the stream is what says it still exists:
   * a shell that opened mid-run lights the marker on the next event, which
   * on a live copy is within a fraction of a second.
   */
  let ingestBusy = $state(false);
  /** False once the shell is gone, so the poll below stops. Its own
   *  `onDestroy` rather than a line in the subscription effect's teardown,
   *  which would start running between re-runs the day that effect gains a
   *  reactive read. */
  let shellAlive = true;

  onDestroy(() => {
    shellAlive = false;
  });
  /** One settle loop at a time, however many `run_stopped`s arrive. */
  let settling = false;

  $effect(() => {
    void loadStatus();
  });

  /**
   * The same progress stream the Ingest surface reads, listened to here as
   * well: the surface unmounts when you leave it, and the marker's whole
   * job is to be visible while you are somewhere else.
   */
  $effect(() => {
    let unlisten: UnlistenFn | null = null;
    let gone = false;
    void listen<IngestProgress>(INGEST_PROGRESS_EVENT, (event) => {
      if (event.payload.event.type === "run_stopped") {
        void settleIngest();
        return;
      }
      ingestBusy = true;
    }).then((off) => {
      if (gone) {
        void off();
        return;
      }
      unlisten = off;
    });
    return () => {
      gone = true;
      if (unlisten !== null) void unlisten();
    };
  });

  /**
   * `run_stopped` is the copy loop ending, not the run: the marker goes out
   * when `ingest_state` says the job slot is free. A failed read puts it
   * out too — the Ingest surface is where that failure is reported, and a
   * marker stuck on forever is the worse of the two lies.
   */
  async function settleIngest() {
    if (settling) return;
    settling = true;
    try {
      // `for (;;)` with the guard inside: the shell can go away across
      // either await, so the check belongs after them, not only at the top.
      for (;;) {
        if (!shellAlive) break;
        const state = await api.ingestState();
        if (!state.busy) break;
        await new Promise((resolve) => {
          setTimeout(resolve, INGEST_POLL_MS);
        });
      }
    } catch {
      // Deliberately not surfaced here; see the comment above.
    } finally {
      ingestBusy = false;
      settling = false;
    }
  }

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
            <!-- The running marker: a run outlives this surface, so the
                 sidebar is where it stays visible. The dot is the mockup's,
                 and it is drawn only — what a screen reader gets is the
                 label, because "Ingest black circle" is not a sentence. -->
            <button
              aria-current={surface === "ingest" ? "page" : undefined}
              aria-label={ingestBusy ? "Ingest — a run is going" : undefined}
              onclick={() => show("ingest")}
              >Ingest{#if ingestBusy}<span class="ingest-mark" aria-hidden="true"
                  >●</span
                >{/if}</button
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
      {:else if surface === "ingest"}
        <!-- No `onselect`: an ingest names files that are not in the
             catalog yet, so nothing on it can open the inspector. -->
        <IngestView />
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

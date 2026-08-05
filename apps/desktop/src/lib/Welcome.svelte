<script lang="ts">
  import { open } from "@tauri-apps/plugin-dialog";
  import { api, errorMessage } from "./api";
  import type { AppStatus } from "./api";

  let { oninitialized }: { oninitialized: (status: AppStatus) => void } =
    $props();

  let error = $state<string | null>(null);

  /**
   * Picks a folder and hands it to `adopt`. Both commands return the new
   * status, so the shell never needs a second round trip to learn it.
   */
  async function choose(adopt: (path: string) => Promise<AppStatus>) {
    error = null;
    try {
      const picked = await open({ directory: true });
      if (typeof picked !== "string") return;
      oninitialized(await adopt(picked));
    } catch (failure) {
      error = errorMessage(failure);
    }
  }
</script>

<section class="welcome">
  <h1>Majestical</h1>
  <p>
    Majestical keeps one catalog of everything you have shot — across every
    drive, card and archive, whether or not it happens to be plugged in. Choose
    a folder to hold that catalog: an empty one to start a new catalog, or one
    that already holds a catalog you or a teammate created.
  </p>
  <div class="actions">
    <button onclick={() => void choose(api.initializeCatalog)}>
      Initialize catalog…
    </button>
    <button onclick={() => void choose(api.useExistingCatalog)}>
      Use existing catalog…
    </button>
  </div>
  {#if error}
    <p class="error" role="alert">{error}</p>
  {/if}
</section>

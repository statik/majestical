<script lang="ts">
  import { checkForUpdate, installAndRestart } from "./updater";
  import type { Update } from "./updater";

  let update = $state<Update | null>(null);
  let installing = $state(false);

  /**
   * Fire-and-forget on mount: `checkForUpdate` never rejects and the shell
   * never awaits it, so an endpoint that is slow or hanging delays this
   * banner and nothing else. The effect reads no reactive state, so it runs
   * once — dismissing the banner does not start another check.
   */
  $effect(() => {
    void checkForUpdate().then((found) => (update = found));
  });

  async function apply() {
    if (update === null) return;
    installing = true;
    await installAndRestart(update);
    // Only reached when the install failed, which `installAndRestart` has
    // already logged. The banner stays up so a second click can try again.
    installing = false;
  }
</script>

{#if update !== null}
  <!-- Fixed rather than a row in the shell grid: an update is transient and
       has no claim on the layout the catalog surfaces were given. -->
  <div class="update-banner" role="status">
    <p>Update to v{update.version} available</p>
    <button disabled={installing} onclick={() => void apply()}>
      {installing ? "Installing…" : "Restart to apply"}
    </button>
    <button
      class="dismiss"
      aria-label="Dismiss update notice"
      onclick={() => (update = null)}>×</button
    >
  </div>
{/if}

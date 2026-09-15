<script lang="ts">
  // The Always-on section of Settings: the background indexer's
  // power-aware throttle override — the same one `tray.rs::apply_throttle`
  // exposes from the menu bar, here reached through `scheduler_state`/
  // `set_throttle` instead of the tray's direct `set_throttle_impl` call —
  // and the OS "start at login" toggle via `autostart.ts`, plus a Retry
  // button for the failure ledger (`retryFailedItems`) shown once
  // `failed_items > 0`. Both scheduler reads happen once on mount; the
  // status line refreshes on a throttle change or a retry, not on a timer.
  // A sibling component to `SettingsView.svelte` rather than folded into
  // it: the scheduler commands and the autostart plugin are two unrelated
  // backends, and keeping them apart keeps each file's state readable at a
  // glance.
  import { autostartEnabled, setAutostart } from "./autostart";
  import { api, errorMessage } from "./api";
  import type { SchedulerStateOutcome, ThrottleOverride } from "./api-alwayson";
  import { failedLine, statusLine } from "./scheduler-status";

  const THROTTLES: { value: ThrottleOverride; label: string }[] = [
    { value: "auto", label: "Auto" },
    { value: "paused", label: "Paused" },
    { value: "low", label: "Low" },
    { value: "full", label: "Full" },
  ];

  let scheduler = $state<SchedulerStateOutcome | null>(null);
  let autostart = $state(false);
  let autostartError = $state<string | null>(null);
  let retryError = $state<string | null>(null);

  $effect(() => {
    void loadScheduler();
    void loadAutostart();
  });

  async function loadScheduler() {
    scheduler = await api.schedulerState();
  }

  async function loadAutostart() {
    autostart = await autostartEnabled();
  }

  async function changeThrottle(throttle: ThrottleOverride) {
    scheduler = await api.setThrottle(throttle);
  }

  async function retryFailed() {
    retryError = null;
    try {
      scheduler = await api.retryFailedItems();
    } catch (failure) {
      retryError = errorMessage(failure);
    }
  }

  async function toggleAutostart(next: boolean) {
    const previous = autostart;
    autostart = next;
    autostartError = null;
    try {
      await setAutostart(next);
    } catch (failure) {
      autostart = previous;
      autostartError = errorMessage(failure);
    }
  }
</script>

<section class="settings-section">
  <div class="settings-section-head">
    <div>
      <h3>Always-on</h3>
      <p class="settings-section-sub">
        Menu-bar tray, power-aware indexing throttle, start at login.
      </p>
    </div>
  </div>

  <p class="settings-section-sub settings-status-label">Indexing throttle</p>
  <div class="settings-throttle" role="radiogroup" aria-label="Indexing throttle">
    {#each THROTTLES as { value, label }}
      <label class="settings-radio">
        <input
          type="radio"
          name="throttle"
          value={value}
          checked={scheduler?.throttle === value}
          onchange={() => void changeThrottle(value)}
        />
        {label}
      </label>
    {/each}
  </div>
  {#if scheduler}
    <p class="settings-status" role="status">{statusLine(scheduler)}</p>
  {/if}
  {#if scheduler && scheduler.failed_items > 0}
    <div class="ctl-actions">
      <p class="settings-status settings-failed" role="status">
        {failedLine(scheduler.failed_items)}
      </p>
      <button class="ctl-btn" onclick={() => void retryFailed()}>
        Retry failed items
      </button>
    </div>
  {/if}
  {#if retryError !== null}
    <p class="error" role="alert">{retryError}</p>
  {/if}

  <div class="settings-toggle-row">
    <input
      id="start-at-login"
      type="checkbox"
      checked={autostart}
      onchange={(event) => void toggleAutostart(event.currentTarget.checked)}
    />
    <label for="start-at-login">Start at login</label>
  </div>
  {#if autostartError}
    <p class="error" role="alert">{autostartError}</p>
  {/if}
</section>

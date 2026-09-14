<script lang="ts">
  // The Always-on section of Settings: the background indexer's
  // power-aware throttle override — the same one `tray.rs::apply_throttle`
  // exposes from the menu bar, here reached through `scheduler_state`/
  // `set_throttle` instead of the tray's direct `set_throttle_impl` call —
  // and the OS "start at login" toggle via `autostart.ts`. A sibling
  // component to `SettingsView.svelte` rather than folded into it: the two
  // sections poll unrelated backends (the scheduler command vs. the
  // autostart plugin) on independent mount effects, and keeping them apart
  // keeps each file's state readable at a glance.
  import { autostartEnabled, setAutostart } from "./autostart";
  import { api, errorMessage } from "./api";
  import type { SchedulerStateOutcome, ThrottleOverride } from "./api";

  const THROTTLES: { value: ThrottleOverride; label: string }[] = [
    { value: "auto", label: "Auto" },
    { value: "paused", label: "Paused" },
    { value: "low", label: "Low" },
    { value: "full", label: "Full" },
  ];

  let scheduler = $state<SchedulerStateOutcome | null>(null);
  let autostart = $state(false);
  let autostartError = $state<string | null>(null);

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

  /**
   * The scheduler's current activity in one line, straight from the wire's
   * own discriminants (`decision.mode`/`hold_reason` plus `pending_items`)
   * — deliberately NOT a port of `tray.rs`'s `menu_model`: this omits the
   * power-source-aware second line `run_low_lines` adds under Auto (it
   * would mean re-deriving `PowerSource` branching here) and the "only
   * show the pending count when nonzero" refinement `menu_model` applies
   * to a Low Power Mode hold. Both are tray-only polish, not information
   * this line claims to give.
   */
  function statusLine(state: SchedulerStateOutcome): string {
    if (state.decision === null) return "Starting…";
    if (state.decision.mode === "hold") {
      switch (state.decision.hold_reason) {
        case "paused":
          return "Paused";
        case "low_power_mode":
          return "Paused (Low Power Mode)";
        case "no_pending_work":
          return "Idle";
      }
    }
    const pending =
      state.pending_items === 1 ? "1 item" : `${state.pending_items} items`;
    return state.decision.mode === "run_full"
      ? `Indexing — ${pending} pending`
      : `Indexing slowly — ${pending} pending`;
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

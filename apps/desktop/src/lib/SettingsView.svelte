<script lang="ts">
  // The Settings surface: right now just the health panel, what `maj
  // doctor` sees, rendered in the order it checks — services owns that
  // order, so this view never sorts. Read-only, the same as Volumes:
  // nothing here fixes a check, it only reports it and offers to look
  // again.
  import { api, errorMessage, errorNotices } from "./api";
  import type { DoctorOutcome } from "./api";
  import Notices from "./Notices.svelte";

  let outcome = $state<DoctorOutcome | null>(null);
  let error = $state<string | null>(null);
  let failureNotices = $state<string[]>([]);
  let loading = $state(false);

  $effect(() => {
    void load();
  });

  async function load() {
    loading = true;
    try {
      outcome = await api.doctorReport();
      error = null;
      failureNotices = [];
    } catch (failure) {
      error = errorMessage(failure);
      failureNotices = errorNotices(failure);
    } finally {
      loading = false;
    }
  }
</script>

<div class="surface">
  <h2 class="surface-title">Settings</h2>

  <section class="settings-section">
    <div class="settings-section-head">
      <div>
        <h3>Health</h3>
        <p class="settings-section-sub">
          What <code>maj doctor</code> sees right now, in the order it checks.
        </p>
      </div>
      <button
        type="button"
        class="ctl-btn"
        disabled={loading}
        onclick={() => void load()}>Run checks again</button
      >
    </div>

    {#if error}
      <Notices notices={failureNotices} />
      <p class="error" role="alert">{error}</p>
    {:else if outcome}
      <Notices notices={outcome.notices} />
      <ul class="settings-checks">
        {#each outcome.checks as check}
          <li class="settings-check">
            <span class="settings-pill settings-pill-{check.status}"
              >{check.status}</span
            >
            <span class="settings-check-name">{check.name}</span>
            <span class="settings-check-detail">{check.detail}</span>
            {#if check.remedy}
              <span class="settings-check-remedy">remedy: {check.remedy}</span>
            {/if}
          </li>
        {/each}
      </ul>
    {/if}
  </section>
</div>

<script lang="ts">
  /**
   * One ingest run while it is going: the preparing window, the progress
   * stream, the counters and the elapsed clock its events feed, and the
   * wait for the outcome after the copy loop ends. Lifted out of
   * `IngestView.svelte` when that file reached its line cap — the split
   * its cap comment named — with no change to what any of it does. The
   * surface's three rules and its WIRE GAPS ledger (no `dest_root` on a
   * failure, no free space, no run timing, no target for a copying file)
   * stay in that file's header rather than being said twice. Two of the
   * rules are the ones this panel keeps: that the BACKEND owns the run —
   * it outlives this component, leaving the surface cancels nothing, and
   * a reload rejoins it through `ingest_state` — and that `run_stopped`
   * is the copy loop ending rather than the run, so it is followed by
   * polling `ingest_state` until `busy` is false, because the `IngestRun`
   * that poll returns (never the events accumulated here) is the
   * authority on what the run placed.
   *
   * The board and the completion card are the parent's, so `phase` and
   * `finished` bind back to it, and one run is driven through
   * `beginRun`/`nameRun`/`dropRun` — the three things a `start_ingest`
   * call can turn into.
   */
  import { onDestroy } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import type { UnlistenFn } from "@tauri-apps/api/event";
  import { api, errorMessage, errorNotices, INGEST_PROGRESS_EVENT } from "./api";
  import type { FinishedIngest, IngestProgress, IngestState } from "./api";
  import { fileSize } from "./format";
  import {
    applyProgress,
    barPercent,
    bytesDone,
    destRoots,
    filePercent,
    noProgress,
    remainingMs,
    runHeading,
    timingLine,
    type Phase,
  } from "./ingest-progress";
  import Notices from "./Notices.svelte";

  /** How often `ingest_state` is asked whether the sweep has finished. */
  const POLL_MS = 200;

  /** How often the elapsed clock is re-read while a run is copying. The
   *  line reads in whole seconds, so anything faster repaints for nothing. */
  const TICK_MS = 1000;

  let {
    dests,
    clock = () => Date.now(),
    onended,
    phase = $bindable<Phase>("idle"),
    finished = $bindable<FinishedIngest | null>(null),
  }: {
    /** The roots the board is copying to: a destination the run has
     *  verified nothing at yet still gets a row, at zero. */
    dests: string[];
    /** Where the elapsed clock reads from. A parameter so a test can pin
     *  the line without waiting real seconds out; nothing else passes it. */
    clock?: () => number;
    /** A run ended: the journal's unfinished list is the board's to
     *  re-read. */
    onended: () => void;
    phase?: Phase;
    finished?: FinishedIngest | null;
  } = $props();

  /** The run this panel is watching, or null while it is still unnamed. */
  let runId = $state<string | null>(null);
  let stopping = $state(false);
  let runError = $state<string | null>(null);
  let runFailureNotices = $state<string[]>([]);

  /** Everything the progress stream has said about the run being watched;
   *  see `ingest-progress.ts` for what each event does to it. */
  let progress = $state(noProgress());
  /** When `run_started` arrived, and the clock's latest reading. Null until
   *  it does: a surface that joined a run mid-flight never saw the start
   *  and has no elapsed time to claim. */
  let startedMs = $state<number | null>(null);
  let nowMs = $state(0);

  /** False once this component is gone, so the outcome poll below stops
   *  asking a backend nobody is listening to. Its own `onDestroy` rather
   *  than a line in the subscription effect's teardown: that effect reads
   *  nothing reactive today, but the day it does, its teardown would start
   *  running between re-runs and quietly kill a poll of a live run. */
  let alive = true;

  onDestroy(() => {
    alive = false;
  });

  let copied = $derived(bytesDone(progress));
  let percent = $derived(barPercent(progress));
  let roots = $derived(destRoots(progress, dests));
  let elapsedMs = $derived(
    startedMs === null ? null : Math.max(0, nowMs - startedMs),
  );
  let timing = $derived(timingLine(elapsedMs, remainingMs(progress, elapsedMs)));

  $effect(() => {
    void adoptRunningState();
  });

  /** The elapsed line has to move between events — a big file copies for
   *  minutes with nothing to say — so a copying run re-reads the clock once
   *  a second, and only while it is copying. */
  $effect(() => {
    if (phase !== "running") return;
    const timer = setInterval(() => {
      nowMs = clock();
    }, TICK_MS);
    return () => clearInterval(timer);
  });

  /**
   * The progress stream. Subscribed once on mount and dropped on destroy;
   * `listen` resolves asynchronously, so a component torn down before it
   * does unlistens as soon as the handle arrives.
   */
  $effect(() => {
    let unlisten: UnlistenFn | null = null;
    let gone = false;
    void listen<IngestProgress>(INGEST_PROGRESS_EVENT, (event) => {
      accept(event.payload);
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

  /** The board asked for a run. The last run's card goes with everything
   *  the last run accumulated: what is on screen from here is this run's. */
  export function beginRun() {
    finished = null;
    resetRun(null);
    phase = "preparing";
  }

  /** `start_ingest` answered. An event that beat the answer has already
   *  adopted the same name: the backend holds one job slot. */
  export function nameRun(id: string) {
    runId = id;
  }

  /** `start_ingest` refused, so there is no run to watch. */
  export function dropRun() {
    phase = "idle";
    runId = null;
  }

  /**
   * What the backend says is going on, which is the authority on mount:
   * the run outlives the webview, so a reload mid-run rejoins it rather
   * than offering to start a second one.
   */
  async function adoptRunningState() {
    try {
      const state = await api.ingestState();
      if (state.busy) {
        resetRun(state.running ?? null);
        // `running` is absent for the instant between the job slot being
        // claimed and the run naming itself — busy without a name is
        // exactly the preparing window.
        phase = state.running === undefined ? "preparing" : "running";
        return;
      }
      finished = state.finished ?? null;
    } catch (failure) {
      runError = errorMessage(failure);
      runFailureNotices = errorNotices(failure);
    }
  }

  /** Everything one run accumulated. Called at the start of every run, so
   *  no counter, row or tally can survive into the next one. */
  function resetRun(id: string | null) {
    runId = id;
    progress = noProgress();
    startedMs = null;
    stopping = false;
    runError = null;
    runFailureNotices = [];
  }

  /**
   * One forwarded progress notification, filtered to the run this panel is
   * watching. While a run is being prepared there is no id to filter by
   * yet — the backend runs one ingest at a time, so the first event to
   * arrive in that window is this run's, and its envelope names it.
   */
  function accept(notification: IngestProgress) {
    if (phase === "idle") return;
    if (runId === null) {
      runId = notification.run_id;
    } else if (notification.run_id !== runId) {
      return;
    }
    progress = applyProgress(progress, notification.event);
    // Every event is also a clock reading, so the elapsed line and the
    // estimate move with the bytes rather than only on the tick.
    nowMs = clock();
    // Two of the events are a phase rather than a number: the run really
    // going, and the copy loop ending — which is not the run ending, so
    // what it ended with is fetched rather than assumed.
    if (notification.event.type === "run_started") {
      startedMs = nowMs;
      phase = "running";
    } else if (notification.event.type === "run_stopped") {
      phase = "finishing";
      void awaitOutcome();
    }
  }

  function sleep(ms: number): Promise<void> {
    return new Promise((resolve) => {
      setTimeout(resolve, ms);
    });
  }

  /**
   * Waits out the end of a run. `run_stopped` says the copy loop ended, not
   * that the outcome exists: the missing-file sweep, the ASC MHL generation
   * per destination and the catalog events all land after it, seconds later
   * on a big run. The progress stays on screen the whole time.
   */
  async function awaitOutcome() {
    const watched = runId;
    // `for (;;)` with the guard inside: every await below can outlive the
    // component, so the check has to happen after each one, not only at
    // the top of the loop.
    for (;;) {
      if (!alive || phase !== "finishing" || runId !== watched) return;
      let state: IngestState;
      try {
        state = await api.ingestState();
      } catch (failure) {
        if (!alive || phase !== "finishing" || runId !== watched) return;
        // What is lost here is this surface's view of how the run ended,
        // not the run: it is still the backend's, and still resumable by
        // the id — which is why the id stays on screen with the message
        // rather than the panel simply disappearing.
        runError = errorMessage(failure);
        runFailureNotices = errorNotices(failure);
        phase = "idle";
        return;
      }
      if (!alive || phase !== "finishing" || runId !== watched) return;
      if (!state.busy) {
        finished = state.finished ?? null;
        if (state.finished === undefined) {
          // The job slot is free and the backend has no outcome to hand
          // over. Nothing else on this surface would mention it: the run
          // panel goes, no card takes its place, and the operator is left
          // to guess what became of the copy.
          runError = `run ${watched ?? "(unnamed)"} ended, but the backend has no outcome for it`;
          runFailureNotices = [];
        }
        phase = "idle";
        runId = null;
        onended();
        return;
      }
      await sleep(POLL_MS);
    }
  }

  /**
   * Cancellation is cooperative and file-granular: the engine checks
   * between files, so the run ends after whatever is in flight — and is
   * resumable by its id afterwards.
   */
  async function askToStop() {
    stopping = true;
    try {
      await api.cancelIngest();
    } catch (failure) {
      stopping = false;
      runError = errorMessage(failure);
      runFailureNotices = errorNotices(failure);
    }
  }

  /** "1 file" / "2 files" — every line that counts them says it the same
   *  way, so two of them cannot disagree about the plural. */
  function plural(count: number, noun: string): string {
    return `${count} ${noun}${count === 1 ? "" : "s"}`;
  }
</script>

<!-- Outside the phase branch on purpose. Both the mount-time state read
     and the end-of-run poll can fail, and both of them leave the surface
     sitting on the idle board — where a run panel's error would never be
     drawn, and the operator would be told nothing at all. -->
{#if runError}
  <div class="ingest-panel" role="group" aria-label="Run state">
    <Notices notices={runFailureNotices} />
    <p class="error" role="alert">{runError}</p>
    {#if runId !== null}
      <p class="empty">
        Run {runId} was going when this happened. The run is the backend's,
        not this window's — `maj ingest unfinished` still lists it if it did
        not finish.
      </p>
    {/if}
  </div>
{/if}

{#if phase !== "idle"}
  <div class="ingest-panel" role="group" aria-label="Run">
    <h3 class="ingest-title">{runHeading(phase)}</h3>
    <p class="count">
      {runId === null ? "naming the run…" : `run ${runId} — resumable`}
    </p>

    {#if phase === "preparing"}
      <p class="empty">
        The run is walking and hashing the source to plan itself. Nothing
        has been copied yet.
      </p>
    {:else}
      {#if progress.totalsKnown}
        <div
          class="ingest-bar"
          role="progressbar"
          aria-label="Bytes copied"
          aria-valuemin="0"
          aria-valuemax="100"
          aria-valuenow={percent}
        >
          <i style="--w: {percent}%"></i>
        </div>
        <p class="ingest-counts">
          <b>{progress.placed + progress.failed} / {progress.filesTotal} files</b>
          <span>{fileSize(copied)} / {fileSize(progress.bytesTotal)}</span>
          <span class:ingest-bad={progress.failed > 0}>
            {progress.failed} failed
          </span>
          {#if timing !== null}
            <span>{timing}</span>
          {/if}
        </p>
      {:else}
        <p class="ingest-counts">
          <b>{plural(progress.placed + progress.failed, "file")} done</b>
          <span>{fileSize(copied)} copied</span>
          <span class:ingest-bad={progress.failed > 0}>
            {progress.failed} failed
          </span>
        </p>
        <p class="empty">
          This surface joined the run after it started, so the totals its
          `run_started` carried are not on screen.
        </p>
      {/if}

      <h4 class="ingest-sub">Now</h4>
      <!-- Unkeyed: `rel` is unique within a run, but this list is built
           here rather than handed over as a map. -->
      <ul class="ingest-rows" aria-label="Files in flight">
        {#each progress.copying as file}
          <li>
            <span class="ingest-path">{file.rel}</span>
            <span class="ingest-stat">
              {fileSize(file.done)} of {fileSize(file.size)} · {filePercent(
                file,
              )}%
            </span>
          </li>
        {/each}
      </ul>

      <h4 class="ingest-sub">Destinations</h4>
      <ul class="ingest-rows" aria-label="Destination tallies">
        {#each roots as root}
          <li>
            <span class="ingest-path">{root}</span>
            <span class="ingest-stat">
              {progress.verified[root] ?? 0} verified
            </span>
          </li>
        {/each}
      </ul>

      {#if progress.failures.length > 0}
        <h4 class="ingest-sub">Failures so far</h4>
        <ul class="ingest-rows" aria-label="Failures so far">
          {#each progress.failures as fail}
            <li>
              <span class="ingest-path">{fail.rel}</span>
              <span class="ingest-bad">{fail.reason}</span>
            </li>
          {/each}
        </ul>
      {/if}
    {/if}

    {#if phase === "finishing"}
      <p class="empty">
        The copy loop ended — waiting for the sweep, the MHL generation per
        destination and the catalog events.
      </p>
    {:else}
      <div class="ctl-actions">
        <button class="ctl-btn ctl-warn" disabled={stopping} onclick={() => void askToStop()}>
          {stopping
            ? "Stopping after the current file…"
            : "Stop after current file"}
        </button>
      </div>
    {/if}
  </div>
{/if}

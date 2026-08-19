<script lang="ts">
  /**
   * The Ingest surface (mockup: `ingest.html`): one verified copy job in
   * three honest states — setup with a plan on screen before anything runs,
   * a live run, and a completion card drawn from the run's own outcome.
   *
   * Three rules this surface is built around, all of them the backend's:
   *
   * - Nothing copies before the plan is visible. Start is refused until a
   *   source, at least one destination and a PARA node are set AND the plan
   *   on screen is current; any edit stales it back to "Plan again".
   * - The BACKEND owns the run. It outlives this component: leaving the
   *   surface does not cancel anything, and coming back (or reloading)
   *   reconstructs the state from `ingest_state`.
   * - The finished `IngestRun` — never the progress events this component
   *   accumulated — is the authority on what a run placed. The end-of-run
   *   sweep can demote a file already announced as `file_placed`, and that
   *   demotion appears in the outcome only. `run_stopped` is not the end
   *   either: the sweep, the ASC MHL generation per destination and the
   *   catalog events land after it, so a `run_stopped` is followed by
   *   polling `ingest_state` until `busy` is false.
   *
   * WIRE GAPS — everything the mockup draws that this surface does not, and
   * the field each one is waiting on. Nothing here is computed from a guess:
   *
   * - `UnfinishedRun` carries the run's source and destinations but not the
   *   PARA node it filed into, and `--resume` needs one — the resumed run
   *   re-derives its plan, exactly as `maj ingest --resume` does. So the
   *   banner's Resume fills the board in with what the journal knows and
   *   asks for the node again, rather than pretending to resume in one
   *   click. A `para` on `UnfinishedRun` would close this.
   * - `ProgressEvent::FileFailed` carries no `dest_root` (the engine's own
   *   comment: a failure reason joins every destination into one string
   *   with no clean per-destination attribution), so a destination row
   *   tallies the files verified AT it and the failures are a run-level
   *   list. The mockup's "88 placed · 1 failed" per destination needs that
   *   field before it can be true.
   * - Destination free space ("1.2 TB free" on each destination row): no
   *   command reports it. `list_mounted_roots` answers volume, label and
   *   path and nothing about capacity, and a destination is a folder the
   *   operator picked, not necessarily a mounted root at all.
   * - The run's duration on the completion card ("94.0 GB · 14:52"):
   *   `IngestRun` carries no timing, and the elapsed clock below belongs to
   *   the surface that watched the run — a card drawn after a reload never
   *   saw it. A started/finished pair on `IngestRun` would close this.
   * - The now-row's target and verb ("copy → SSD-A", "verify → NAS-1"):
   *   `file_started` and `bytes_copied` name only the file, and the source
   *   is read once and fanned out to every destination, so there is no one
   *   destination a copying file is "at". The percent beside each row IS
   *   drawn — that one is bytes over the size `file_started` announced.
   *
   * Two deliberate divergences from the mockup, neither of them blocked:
   * the plan's rendered subfolder sits in the "File as" panel beside the
   * template that produced it rather than in the plan panel, and Start
   * shares the plan panel's action row rather than sitting under it — both
   * put the button next to the thing it is a decision about.
   */
  import { onDestroy } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import type { UnlistenFn } from "@tauri-apps/api/event";
  import { open } from "@tauri-apps/plugin-dialog";
  import { api, errorMessage, errorNotices, INGEST_PROGRESS_EVENT } from "./api";
  import type {
    FinishedIngest,
    IngestPlanOutcome,
    IngestProgress,
    IngestState,
    ParaNodeRow,
    UnfinishedRun,
  } from "./api";
  import { fileSize } from "./format";
  import { planSummary } from "./ingest-plan";
  import {
    applyProgress,
    barPercent,
    bytesDone,
    destRoots,
    filePercent,
    noProgress,
    remainingMs,
    timingLine,
  } from "./ingest-progress";
  import Notices from "./Notices.svelte";

  /**
   * What the layout template box shows when it is left empty. Display only
   * — an untouched box sends no `template` at all, so the value that
   * actually applies is `commands::DEFAULT_INGEST_TEMPLATE`, and this
   * string never reaches the wire.
   */
  const DEFAULT_TEMPLATE = "{date}/{source-label}";

  /** How often `ingest_state` is asked whether the sweep has finished. */
  const POLL_MS = 200;

  /** How often the elapsed clock is re-read while a run is copying. The
   *  line reads in whole seconds, so anything faster repaints for nothing. */
  const TICK_MS = 1000;

  let { clock = () => Date.now() }: {
    /** Where the elapsed clock reads from. A parameter so a test can pin
     *  the line without waiting real seconds out; nothing else passes it. */
    clock?: () => number;
  } = $props();

  /** Where this surface is: `idle` is the setup board (with the last run's
   *  card above it, if there is one), the other three are one run. */
  type Phase = "idle" | "preparing" | "running" | "finishing";

  let source = $state("");
  let dests = $state<string[]>([]);
  let para = $state("");
  let template = $state("");
  let plan = $state<IngestPlanOutcome | null>(null);
  /** The setup changed after `plan` was read: the counts on screen were
   *  true of a different job, so Start is refused until it is redone. */
  let planStale = $state(false);
  let planning = $state(false);
  /** The unfinished run Start would continue, set by the resume banner. */
  let resumeOf = $state<string | null>(null);

  let nodes = $state<ParaNodeRow[]>([]);
  let paraNotices = $state<string[]>([]);
  let unfinished = $state<UnfinishedRun[]>([]);
  let unfinishedNotices = $state<string[]>([]);
  let setupError = $state<string | null>(null);
  let setupFailureNotices = $state<string[]>([]);

  let phase = $state<Phase>("idle");
  /** The run this surface is watching, or null while it is still unnamed. */
  let runId = $state<string | null>(null);
  let finished = $state<FinishedIngest | null>(null);
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

  /** Every list read takes the next number, and an answer that is no longer
   *  current is dropped — the same rule the other surfaces follow. */
  let planSeq = 0;
  /** False once this component is gone, so the outcome poll below stops
   *  asking a backend nobody is listening to. Its own `onDestroy` rather
   *  than a line in the subscription effect's teardown: that effect reads
   *  nothing reactive today, but the day it does, its teardown would start
   *  running between re-runs and quietly kill a poll of a live run. */
  let alive = true;

  onDestroy(() => {
    alive = false;
  });

  /** Filing into an archived node files into somewhere nobody is looking. */
  let fileable = $derived(nodes.filter((node) => !node.archived));

  let summary = $derived(planSummary(plan));

  let canPlan = $derived(source !== "" && para !== "" && !planning);
  let canStart = $derived(
    source !== "" &&
      dests.length > 0 &&
      para !== "" &&
      plan !== null &&
      !planStale,
  );

  let copied = $derived(bytesDone(progress));
  let percent = $derived(barPercent(progress));
  let roots = $derived(destRoots(progress, dests));
  let elapsedMs = $derived(
    startedMs === null ? null : Math.max(0, nowMs - startedMs),
  );
  let timing = $derived(timingLine(elapsedMs, remainingMs(progress, elapsedMs)));

  $effect(() => {
    void loadNodes();
  });

  $effect(() => {
    void loadUnfinished();
  });

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

  async function loadNodes() {
    try {
      const outcome = await api.listPara();
      nodes = outcome.nodes;
      paraNotices = outcome.notices ?? [];
    } catch (failure) {
      setupError = errorMessage(failure);
      setupFailureNotices = errorNotices(failure);
    }
  }

  async function loadUnfinished() {
    try {
      const outcome = await api.listUnfinishedIngests();
      unfinished = outcome.runs;
      unfinishedNotices = outcome.notices ?? [];
    } catch (failure) {
      setupError = errorMessage(failure);
      setupFailureNotices = errorNotices(failure);
    }
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
   * One forwarded progress notification, filtered to the run this surface
   * is watching. While a run is being prepared there is no id to filter by
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
        void loadUnfinished();
        return;
      }
      await sleep(POLL_MS);
    }
  }

  /** Runs one folder picker and reports what was chosen, or null when the
   *  operator dismissed it or it refused. */
  async function pickFolder(): Promise<string | null> {
    setupError = null;
    setupFailureNotices = [];
    try {
      const picked = await open({ directory: true });
      return typeof picked === "string" ? picked : null;
    } catch (failure) {
      setupError = errorMessage(failure);
      setupFailureNotices = errorNotices(failure);
      return null;
    }
  }

  /**
   * An edit to the job itself: the plan on screen no longer describes it,
   * and neither does the unfinished run the banner offered to continue —
   * a resume is keyed to the source and destinations that run used.
   */
  function editJob() {
    if (plan !== null) planStale = true;
    resumeOf = null;
  }

  async function pickSource() {
    const picked = await pickFolder();
    if (picked === null) return;
    source = picked;
    editJob();
  }

  async function addDest() {
    const picked = await pickFolder();
    if (picked === null || dests.includes(picked)) return;
    dests = [...dests, picked];
    editJob();
  }

  function removeDest(dest: string) {
    dests = dests.filter((root) => root !== dest);
    editJob();
  }

  /** The node is the one thing the resume banner cannot fill in, so
   *  choosing it stales the plan without dropping the run being resumed. */
  function pickNode(id: string) {
    para = id;
    if (plan !== null) planStale = true;
  }

  function editTemplate(next: string) {
    template = next;
    editJob();
  }

  /** What the wire is sent: an untouched box means "the backend's default",
   *  which is `undefined` rather than this file's copy of that string. */
  function templateArg(): string | undefined {
    const typed = template.trim();
    return typed === "" ? undefined : typed;
  }

  async function makePlan() {
    const seq = ++planSeq;
    planning = true;
    setupError = null;
    setupFailureNotices = [];
    try {
      const outcome = await api.planIngest({
        source,
        para,
        template: templateArg(),
      });
      if (seq !== planSeq) return;
      plan = outcome;
      planStale = false;
    } catch (failure) {
      if (seq !== planSeq) return;
      // The failed read owns the panel: the counts on screen came from an
      // earlier plan and this call is the reason to doubt them.
      plan = null;
      planStale = false;
      setupError = errorMessage(failure);
      setupFailureNotices = errorNotices(failure);
    } finally {
      if (seq === planSeq) planning = false;
    }
  }

  /**
   * Hands the job to the backend and waits for the run's name. What that
   * wait covers is the run thread's own planning pass — a walk and a hash
   * of every file whose size matches something the catalog knows — which is
   * why the surface says "Preparing…" rather than drawing an empty bar.
   */
  async function start() {
    setupError = null;
    setupFailureNotices = [];
    finished = null;
    resetRun(null);
    phase = "preparing";
    try {
      const id = await api.startIngest({
        source,
        dests,
        para,
        template: templateArg(),
        resume: resumeOf ?? undefined,
      });
      // The answer names the run, and an event that beat it has already
      // adopted the same name: the backend holds one job slot, so there is
      // no second run whose events could have arrived in this window.
      runId = id;
      resumeOf = null;
    } catch (failure) {
      phase = "idle";
      runId = null;
      setupError = errorMessage(failure);
      setupFailureNotices = errorNotices(failure);
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

  /** The banner's Resume: everything the journal recorded, which is not the
   *  PARA node — see the WIRE GAP note at the top. */
  function resume(run: UnfinishedRun) {
    source = run.source;
    dests = [...run.destinations];
    plan = null;
    planStale = false;
    resumeOf = run.run_id;
  }

  function dismiss(run: UnfinishedRun) {
    unfinished = unfinished.filter((row) => row.run_id !== run.run_id);
  }

  /** What the run panel is titled. `finishing` had been drawn as "Copying",
   *  which is the one thing the run is provably no longer doing. */
  function headingFor(current: Phase): string {
    if (current === "preparing") return "Preparing…";
    if (current === "finishing") return "Finishing…";
    return "Copying";
  }

  /** "1 file" / "2 files" — every line that counts them says it the same
   *  way, so two of them cannot disagree about the plural. */
  function plural(count: number, noun: string): string {
    return `${count} ${noun}${count === 1 ? "" : "s"}`;
  }
</script>

<div class="surface ingest-surface">
  <!-- Always in the document, empty between reads: a `role="status"`
       element created together with its text is not reliably announced, so
       what changes has to be the contents. Same rule as every surface. -->
  <div role="status">
    <Notices notices={paraNotices} />
    <Notices notices={unfinishedNotices} />
  </div>

  <!-- Outside every phase branch on purpose. Both the mount-time state read
       and the end-of-run poll can fail, and both of them leave this surface
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

  <!-- Idle only. A banner calling a run unfinished, with a live Resume
       button, must not sit over that same run's progress. -->
  {#if unfinished.length > 0 && phase === "idle"}
    <!-- Keyed by run id, which is the journal's own key for a run. -->
    <ul class="ingest-resumes" aria-label="Unfinished runs">
      {#each unfinished as run (run.run_id)}
        <li class="ingest-resume">
          <p><b>Unfinished run {run.run_id}</b></p>
          <p>{run.placed} of {run.planned} files placed, from {run.source}.</p>
          <p class="empty">
            Resuming re-derives the plan, so it needs the PARA node again —
            the journal never recorded which one this run filed into.
          </p>
          <button class="ctl-btn" onclick={() => resume(run)}>
            Resume run {run.run_id}
          </button>
          <!-- Window-local: the journal still lists the run, and the next
               read of it brings the banner back. -->
          <button class="ctl-btn ingest-quiet" onclick={() => dismiss(run)}>
            Hide run {run.run_id} for now
          </button>
        </li>
      {/each}
    </ul>
  {/if}

  {#if finished !== null}
    {@const done = finished.status === "done" ? finished.run : null}
    {@const failure = finished.status === "failed" ? finished.error : null}
    {#if done !== null}
      {@const outcome = done.outcome}
      {@const bytes = outcome.placed.reduce((sum, file) => sum + file.size, 0)}
      <div class="ingest-panel" role="group" aria-label="Completed run">
        <h3 class="ingest-title">Run {done.run_id} — complete</h3>
        <!-- Every number here is the outcome's, never the events': the
             end-of-run sweep can demote a file already announced as
             placed, and only this struct knows it. -->
        <p class="ingest-counts">
          <b class="ingest-ok">{outcome.placed.length} placed</b>
          <span class:ingest-bad={outcome.failed.length > 0}>
            {outcome.failed.length} failed
          </span>
          <span>{fileSize(bytes)}</span>
          <span>{plural(outcome.skipped_duplicates.length, "duplicate")} skipped</span>
          <span>{outcome.skipped_resumed} already placed by an earlier run</span>
        </p>

        {#if done.generations.length > 0}
          <p class="count">MHL generation written per destination</p>
          <!-- Keyed by destination root: `generations` is a map from root to
               the generation written under it, one entry per root. -->
          <ul class="ingest-rows" aria-label="MHL generations">
            {#each done.generations as [root, generation] (root)}
              <li>
                <span class="ingest-path">{root}</span>
                <span class="ingest-stat">
                  generation {generation.generation} · {generation.path}
                </span>
              </li>
            {/each}
          </ul>
        {:else}
          <p class="empty">No MHL generation was written: this run placed nothing.</p>
        {/if}

        {#if outcome.failed.length > 0}
          <h4 class="ingest-sub">Failures — kept, named, actionable</h4>
          <ul class="ingest-rows" aria-label="Failed files">
            {#each outcome.failed as file}
              <li>
                <span class="ingest-path">{file.rel}</span>
                <span class="ingest-bad">{file.reason}</span>
              </li>
            {/each}
          </ul>
          <!-- A re-copy is the same job planned again: the files that did
               land are duplicates now and skip themselves. Disabled while
               that plan runs rather than replaced — re-walking a real card
               takes seconds to minutes, and a control that deletes itself
               mid-click takes the focus with it. The message below is for
               the other case entirely: a card drawn after a reload, with no
               source or node left on the board to plan from. -->
          {#if source === "" || para === ""}
            <p class="empty">
              Choose the source and the PARA node again below to re-copy these.
            </p>
          {:else}
            <button
              class="ctl-btn"
              disabled={!canPlan}
              onclick={() => void makePlan()}
            >
              {planning ? "Planning…" : "Re-copy failed…"}
            </button>
          {/if}
        {/if}

        {#if outcome.rejected.length > 0}
          <h4 class="ingest-sub">Rejected</h4>
          <ul class="ingest-rows" aria-label="Rejected by the run">
            {#each outcome.rejected as file}
              <li>
                <span class="ingest-path">{file.rel}</span>
                <span class="ingest-reason">{file.reason}</span>
              </li>
            {/each}
          </ul>
        {/if}

        {#if outcome.diagnostics.length > 0}
          <ul class="ingest-rows" aria-label="Diagnostics">
            {#each outcome.diagnostics as line}
              <li><span class="ingest-reason">{line}</span></li>
            {/each}
          </ul>
        {/if}

        <Notices notices={done.notices} />
      </div>
    {/if}
    {#if failure !== null}
      <div class="ingest-panel" role="group" aria-label="Failed run">
        <h3 class="ingest-title">The run failed</h3>
        <Notices notices={failure.notices} />
        <p class="error" role="alert">{failure.message}</p>
      </div>
    {/if}
  {/if}

  {#if phase === "idle"}
    <div class="ingest-board" role="group" aria-label="Setup">
      <div class="ingest-panel">
        <h3 class="ingest-title">Source</h3>
        {#if source === ""}
          <p class="empty">No source chosen yet.</p>
        {:else}
          <p class="ingest-path">{source}</p>
        {/if}
        <!-- What the plan actually walked, and only while that plan is
             current: a stale count of a folder nobody re-read is a claim
             about the card that may no longer be true. -->
        {#if plan !== null && !planStale}
          <p class="count">
            {plan.source_volume_label} · {summary.files} files · {fileSize(
              summary.bytes,
            )}
          </p>
        {/if}
        <button class="ctl-btn" onclick={() => void pickSource()}>
          Choose source…
        </button>
      </div>
      <div class="ingest-panel">
        <h3 class="ingest-title">Destinations</h3>
        {#if dests.length === 0}
          <p class="empty">
            No destination yet — a verified copy needs at least one.
          </p>
        {/if}
        <!-- Unkeyed: this list is the operator's own, and `addDest` refuses
             a root already in it, so removal by value is unambiguous. -->
        <ul class="ingest-rows" aria-label="Destinations">
          {#each dests as dest}
            <li>
              <span class="ingest-path">{dest}</span>
              <button
                class="ctl-btn ingest-quiet"
                onclick={() => removeDest(dest)}
              >
                Remove {dest}
              </button>
            </li>
          {/each}
        </ul>
        <button class="ctl-btn" onclick={() => void addDest()}>
          + Add destination
        </button>
      </div>
    </div>

    <div class="ingest-panel">
      <h3 class="ingest-title">File as</h3>
      <div class="ctl-actions">
        <!-- Keyed by node id, the catalog's own key for a node. -->
        <select
          class="ctl-input"
          aria-label="PARA node"
          value={para}
          onchange={(event) => pickNode(event.currentTarget.value)}
        >
          <option value="">Choose a PARA node…</option>
          {#each fileable as node (node.id)}
            <option value={node.id}>{node.kind}/{node.name}</option>
          {/each}
        </select>
        <input
          class="ctl-input"
          type="text"
          aria-label="Subfolder template"
          placeholder={DEFAULT_TEMPLATE}
          value={template}
          oninput={(event) => editTemplate(event.currentTarget.value)}
        />
      </div>
      {#if plan !== null && !planStale}
        <p class="count">Subfolder under every destination: {plan.subdir}</p>
      {/if}
      {#if resumeOf !== null}
        <p class="count">Start will continue run {resumeOf}.</p>
      {/if}
    </div>

    <div class="ingest-panel">
      <h3 class="ingest-title">Plan</h3>
      <div class="ctl-actions">
        <button
          class="ctl-btn"
          disabled={!canPlan}
          onclick={() => void makePlan()}
        >
          {plan === null ? "Plan" : "Plan again"}
        </button>
        <button
          class="ctl-btn ingest-primary"
          disabled={!canStart}
          onclick={() => void start()}
        >
          Start verified copy
        </button>
      </div>
      {#if planning}
        <p class="empty">Planning…</p>
      {/if}
      {#if plan !== null}
        {#if planStale}
          <p class="ingest-stale">
            The job changed — plan again before starting.
          </p>
        {/if}
        <p class="ingest-counts">
          <b>{summary.toCopy} to copy</b>
          <span>{fileSize(summary.copyBytes)}</span>
          <span>{plural(summary.duplicates, "duplicate")} skipped</span>
          <span>{summary.rejects.length} rejected</span>
        </p>
        {#if summary.rejects.length > 0}
          <!-- Rejects are rows, not errors: the same polarity every other
               surface keeps for what the service would not do. -->
          <ul class="ingest-rows" aria-label="Rejected by the plan">
            {#each summary.rejects as file}
              <li>
                <span class="ingest-path">{file.rel}</span>
                <span class="ingest-reason">{file.reason}</span>
              </li>
            {/each}
          </ul>
        {/if}
        <Notices notices={plan.notices} />
      {:else if !planning}
        <p class="empty">Nothing is copied before a plan is on screen.</p>
      {/if}
      {#if setupError}
        <Notices notices={setupFailureNotices} />
        <p class="error" role="alert">{setupError}</p>
      {/if}
    </div>
  {:else}
    <div class="ingest-panel" role="group" aria-label="Run">
      <h3 class="ingest-title">{headingFor(phase)}</h3>
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
</div>

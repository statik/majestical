<script lang="ts">
  /**
   * The Ingest surface (mockups: `ingest.html`, and
   * `2026-09-14-phase7f/ingest-path-entry.html` frame 1 for the paths):
   * one verified copy job in three honest states — setup with a plan on
   * screen before anything runs, a live run, and a completion card drawn
   * from the run's own outcome. The run itself is
   * `IngestRunPanel.svelte`; what is here is the board and the card.
   *
   * Three rules this surface is built around, all of them the backend's:
   *
   * - Nothing copies before the plan is visible. Start is refused until a
   *   source, at least one destination and a PARA node are set AND the plan
   *   on screen is current; any edit stales it back to "Plan again".
   * - The BACKEND owns the run. It outlives this component: leaving the
   *   surface does not cancel anything, and coming back (or reloading)
   *   reconstructs the state from `ingest_state`.
   * - The finished `IngestRun` — never the progress events the run panel
   *   accumulated — is the authority on what a run placed. The end-of-run
   *   sweep can demote a file already announced as `file_placed`, and that
   *   demotion appears in the outcome only.
   *
   * Source and every destination are typed as well as browsed: the native
   * dialog reaches what it can mount, and a server share or a path out of
   * a shot list is typed. A typed path gets no existence check here —
   * `plan_ingest` is what validates it, and its refusal already renders in
   * this same board.
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
   *   `IngestRun` carries no timing, and the run panel's elapsed clock
   *   belongs to the window that watched the run — a card drawn after a reload never
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
  import { open } from "@tauri-apps/plugin-dialog";
  import { api, errorMessage, errorNotices } from "./api";
  import type {
    FinishedIngest,
    IngestPlanOutcome,
    ParaNodeRow,
    UnfinishedRun,
  } from "./api";
  import { fileSize, plural } from "./format";
  import { planSummary } from "./ingest-plan";
  import type { Phase } from "./ingest-progress";
  import IngestRunPanel from "./IngestRunPanel.svelte";
  import Notices from "./Notices.svelte";

  /**
   * What the layout template box shows when it is left empty. Display only
   * — an untouched box sends no `template` at all, so the value that
   * actually applies is `commands::DEFAULT_INGEST_TEMPLATE`, and this
   * string never reaches the wire.
   */
  const DEFAULT_TEMPLATE = "{date}/{source-label}";

  let { clock = () => Date.now() }: {
    /** Where the run panel's elapsed clock reads from. A parameter so a
     *  test can pin the line without waiting real seconds out; nothing
     *  else passes it. */
    clock?: () => number;
  } = $props();

  let source = $state("");
  let dests = $state<string[]>([]);
  let para = $state("");
  let template = $state("");
  /** The destination being typed, and why the last one was refused. */
  let destDraft = $state("");
  let destError = $state<string | null>(null);
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

  /** The run panel, which owns everything about a run in flight. Bound
   *  back here because the board is drawn while it is `idle` and the card
   *  below is drawn from the outcome it fetched. */
  let phase = $state<Phase>("idle");
  let finished = $state<FinishedIngest | null>(null);
  let runPanel = $state<ReturnType<typeof IngestRunPanel>>();

  /** Every list read takes the next number, and an answer that is no longer
   *  current is dropped — the same rule the other surfaces follow. */
  let planSeq = 0;

  /** Filing into an archived node files into somewhere nobody is looking. */
  let fileable = $derived(nodes.filter((node) => !node.archived));

  let summary = $derived(planSummary(plan));

  /** The source as the wire sees it: trimmed here only (the field keeps what was typed). */
  let sourceArg = $derived(source.trim());
  let canPlan = $derived(sourceArg !== "" && para !== "" && !planning);
  let canStart = $derived(
    sourceArg !== "" && dests.length > 0 && para !== "" && plan !== null && !planStale,
  );

  $effect(() => {
    void loadNodes();
  });

  $effect(() => {
    void loadUnfinished();
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

  /** A typed or browsed source, kept exactly as typed (trimmed only at the
   *  wire, so a path with an inner space is never fought while it is being
   *  typed) and committed on every keystroke: Plan enables as the operator
   *  types. A blur-only commit would leave their first click on the still-
   *  disabled Plan button dead — Chrome and WebKit do not blur the field
   *  for a click on a disabled control. */
  function setSource(next: string) {
    if (next === source) return;
    source = next;
    editJob();
  }

  /** Adds one destination root; a root already in the list is refused with
   *  the message shown, so a paste that changed nothing does not look like
   *  it did. Shared by the typed field and the folder picker. */
  function pushDest(root: string): boolean {
    destError = null;
    if (dests.includes(root)) {
      destError = `${root} is already a destination.`;
      return false;
    }
    dests = [...dests, root];
    editJob();
    return true;
  }

  function addTypedDest() {
    const typed = destDraft.trim();
    if (typed === "") return;
    if (pushDest(typed)) destDraft = "";
  }

  async function pickSource() {
    const picked = await pickFolder();
    if (picked !== null) setSource(picked);
  }

  async function addDest() {
    const picked = await pickFolder();
    if (picked !== null) pushDest(picked);
  }

  function removeDest(dest: string) {
    dests = dests.filter((root) => root !== dest);
    destError = null;
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
        source: sourceArg,
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
    // `bind:this` is set at mount, and `<IngestRunPanel>` is mounted
    // unconditionally — so a null here means this component never mounted,
    // not a state Start should quietly do nothing for.
    if (runPanel === undefined) throw new Error("run panel not mounted");
    setupError = null;
    setupFailureNotices = [];
    runPanel.beginRun();
    try {
      const id = await api.startIngest({
        source: sourceArg,
        dests,
        para,
        template: templateArg(),
        resume: resumeOf ?? undefined,
      });
      runPanel.nameRun(id);
      resumeOf = null;
    } catch (failure) {
      runPanel.dropRun();
      setupError = errorMessage(failure);
      setupFailureNotices = errorNotices(failure);
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
</script>

<div class="surface ingest-surface">
  <!-- Always in the document, empty between reads: a `role="status"`
       element created together with its text is not reliably announced, so
       what changes has to be the contents. Same rule as every surface. -->
  <div role="status">
    <Notices notices={paraNotices} />
    <Notices notices={unfinishedNotices} />
  </div>

  <!-- The run, when there is one — and a run state this window could not
       read even when there is not: both the mount-time read and the
       end-of-run poll can fail, and either leaves the surface sitting on
       the idle board, where nothing else would mention it. -->
  <IngestRunPanel
    bind:this={runPanel}
    bind:phase
    bind:finished
    {dests}
    {clock}
    onended={() => void loadUnfinished()}
  />

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
          {#if sourceArg === "" || para === ""}
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
        <div class="ctl-actions">
          <input
            class="ctl-input"
            type="text"
            aria-label="Source path"
            placeholder="/Volumes/CARD_01 or any folder"
            value={source}
            oninput={(event) => setSource(event.currentTarget.value)}
          />
          <button
            class="ctl-btn"
            aria-label="Browse for source"
            onclick={() => void pickSource()}
          >
            Browse…
          </button>
        </div>
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
      </div>
      <div class="ingest-panel">
        <h3 class="ingest-title">Destinations</h3>
        {#if dests.length === 0}
          <p class="empty">
            No destination yet — a verified copy needs at least one.
          </p>
        {/if}
        <!-- Unkeyed: this list is the operator's own, and `pushDest`
             refuses a root already in it, so removal by value is
             unambiguous. -->
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
        <div class="ctl-actions">
          <input
            class="ctl-input"
            type="text"
            aria-label="Destination path"
            aria-invalid={destError !== null}
            aria-describedby={destError === null ? undefined : "dest-error"}
            placeholder="/Volumes/SHUTTLE_A"
            value={destDraft}
            oninput={(event) => {
              destDraft = event.currentTarget.value;
              destError = null;
            }}
            onkeydown={(event) => {
              if (event.key === "Enter") addTypedDest();
            }}
          />
          <button
            class="ctl-btn"
            aria-label="Add destination"
            onclick={addTypedDest}
          >
            Add
          </button>
          <button
            class="ctl-btn"
            aria-label="Browse for destination"
            onclick={() => void addDest()}
          >
            Browse…
          </button>
        </div>
        {#if destError !== null}
          <p id="dest-error" class="error" role="alert">{destError}</p>
        {/if}
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
  {/if}
</div>

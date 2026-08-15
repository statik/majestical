<script lang="ts">
  /**
   * The bar a grid raises over a multi-selection (mockup: `organize.html`'s
   * third frame). Organize manages the taxonomy; putting assets *into* it
   * happens where the assets are, so this bar lives in Browse and Search —
   * one component, because a grid that tagged differently from the other
   * grid would be a second thing to learn.
   *
   * It holds no selection of its own: the surface owns the set, hands it
   * down, and `onclear` asks the surface to empty it. Both actions are
   * catalog metadata only — `assign_tags` and `file_assets` emit events; no
   * file moves.
   *
   * A finished action shuts its picker but keeps the selection: one set is
   * often worth two actions (tag these, then file these), and rebuilding a
   * set of thirty cards to do the second one is work the user already did.
   * Clear is what empties it.
   */
  import { api, errorMessage, errorNotices } from "./api";
  import type {
    AssignFailure,
    AssignOutcome,
    ParaNodeRow,
    TagRow,
  } from "./api";
  import Notices from "./Notices.svelte";

  let {
    selected,
    onclear,
  }: {
    /** The assets the grid has selected, in the order it selected them. */
    selected: string[];
    /** Empty the grid's set; the bar goes with it. */
    onclear: () => void;
  } = $props();

  /**
   * Below two selected assets there is no bulk anything to do, and the
   * inspector already describes the one asset a plain click chose — so the
   * bar is absent rather than present and pointless.
   */
  const MINIMUM = 2;

  /** Which picker is open, if either. */
  let picker = $state<"tags" | "nodes" | null>(null);
  let tags = $state<TagRow[]>([]);
  let nodes = $state<ParaNodeRow[]>([]);
  /** The existing tags picked out of the list, in the order they were
   *  picked — what the assignment sends, ahead of anything typed. */
  let picked = $state<string[]>([]);
  let typed = $state("");
  /** Whether the open picker's list has answered. Nothing is claimed about
   *  what the catalog holds until it has — "No tags yet" before the read
   *  lands is a claim this component is in no position to make — and a read
   *  that failed leaves it false, so the rows of the read before it cannot
   *  sit under the error looking current. */
  let loaded = $state(false);
  /** Notices from the list the open picker is drawing. */
  let listNotices = $state<string[]>([]);
  /** What the last action did, as its own line under the bar. */
  let result = $state<string | null>(null);
  let failed = $state<AssignFailure[]>([]);
  let resultNotices = $state<string[]>([]);
  let error = $state<string | null>(null);
  let failureNotices = $state<string[]>([]);
  /** An action is in flight: a second click would assign twice. */
  let busy = $state(false);
  /**
   * Every list read takes the next number. Opening the other picker while
   * the first one's list is still coming must not fill the new picker with
   * the old one's rows.
   */
  let listSeq = 0;

  /** Filing into an archived node files into somewhere nobody is looking. */
  let fileable = $derived(nodes.filter((node) => !node.archived));

  /**
   * Everything the last action left on screen. An outcome describes the set
   * it ran over ("Tagged 3 assets", and which of those three were refused),
   * so it can only stay while that set does.
   */
  function forgetOutcome() {
    result = null;
    failed = [];
    resultNotices = [];
    error = null;
    failureNotices = [];
  }

  /**
   * The surface keeps this component mounted whether or not there is a bar
   * to draw, so a selection falling below the threshold takes the picker and
   * the outcome down with it. Otherwise the next two cards the user picks
   * would raise a bar already carrying the last set's count and its failure
   * rows, attributed to assets that were never part of it — and a picker
   * nobody opened, over a list read for a different selection.
   */
  $effect(() => {
    if (selected.length >= MINIMUM) return;
    picker = null;
    forgetOutcome();
  });

  /** Opens a picker and reads what it offers. */
  async function open(which: "tags" | "nodes") {
    picker = which;
    picked = [];
    typed = "";
    loaded = false;
    listNotices = [];
    // A failed read belongs to the picker that failed; the last action's
    // result line stays, because that action really happened.
    error = null;
    failureNotices = [];
    const seq = ++listSeq;
    try {
      if (which === "tags") {
        const outcome = await api.listTags();
        if (seq !== listSeq) return;
        tags = outcome.tags;
        listNotices = outcome.notices ?? [];
        loaded = true;
      } else {
        const outcome = await api.listPara();
        if (seq !== listSeq) return;
        nodes = outcome.nodes;
        listNotices = outcome.notices ?? [];
        loaded = true;
      }
    } catch (failure) {
      if (seq !== listSeq) return;
      error = errorMessage(failure);
      failureNotices = errorNotices(failure);
    }
  }

  /**
   * Shuts the open picker and takes its error with it: an alert about a list
   * that is no longer on screen has nothing left to point at. With no picker
   * open there is nothing to shut — an assignment's own message is not
   * Escape's to dismiss.
   */
  function closePicker() {
    if (picker === null) return;
    picker = null;
    error = null;
    failureNotices = [];
  }

  function toggle(tag: string) {
    picked = picked.includes(tag)
      ? picked.filter((name) => name !== tag)
      : [...picked, tag];
  }

  /** "1 asset" / "3 assets" — every line that counts them says it the same
   *  way, so two of them cannot disagree about the plural. */
  function assets(count: number): string {
    return `${count} asset${count === 1 ? "" : "s"}`;
  }

  /**
   * Runs one assignment and reports what came back. A refusal leaves the
   * picker open with everything still picked: that is what a retry needs.
   */
  async function run(
    call: () => Promise<AssignOutcome>,
    line: (outcome: AssignOutcome) => string,
  ) {
    busy = true;
    forgetOutcome();
    try {
      const outcome = await call();
      result = line(outcome);
      // Per-asset failures are the service's own words for what it would
      // not do, and they are the only record of it.
      failed = outcome.failed;
      resultNotices = outcome.notices ?? [];
      picker = null;
    } catch (failure) {
      error = errorMessage(failure);
      failureNotices = errorNotices(failure);
    } finally {
      busy = false;
      // Clear can empty the set while the assignment is in flight; what came
      // back describes a selection that no longer exists, and must not sit
      // in wait for the next one.
      if (selected.length < MINIMUM) forgetOutcome();
    }
  }

  /** The tags picked from the list, plus whatever was typed into the box —
   *  `assign_tags` creates a tag it has not seen, so the box is the create
   *  path and needs no verb of its own. */
  async function applyTags() {
    const chosen = [...picked];
    const fresh = typed.trim();
    if (fresh !== "" && !chosen.includes(fresh)) chosen.push(fresh);
    if (chosen.length === 0) {
      // The same clean slate a real attempt starts from: "Tagged 3 assets"
      // above "pick a tag…" would credit this click with the last one's work.
      forgetOutcome();
      error = "pick a tag from the list or type a new one";
      return;
    }
    await run(
      () => api.assignTags(selected, chosen),
      (outcome) => `Tagged ${assets(outcome.applied)}`,
    );
  }

  /** By id rather than `<kind>/<name>`: an id resolves whatever the name is
   *  doing, including after two machines have raced on it. */
  async function fileTo(node: ParaNodeRow) {
    await run(
      () => api.fileAssets(selected, node.id),
      (outcome) => `Filed ${assets(outcome.applied)} to ${node.name}`,
    );
  }
</script>

<!-- Escape shuts the open picker, the same way it dismisses the archive
     modal. No focus trap here either — and unlike that modal this is a
     popover over the grid, not a dialog: the grid behind it stays live,
     because picking more cards mid-thought is the point of the bar. -->
<svelte:window
  onkeydown={(event) => {
    if (event.key === "Escape") closePicker();
  }}
/>

{#if selected.length >= MINIMUM}
  <div class="sel-bar" role="group" aria-label="Selection">
    <div class="sel-row">
      <b class="sel-count">{selected.length} selected</b>
      <button
        class="ctl-btn"
        aria-expanded={picker === "tags"}
        disabled={busy}
        onclick={() => void open("tags")}
      >
        Tag…
      </button>
      <button
        class="ctl-btn"
        aria-expanded={picker === "nodes"}
        disabled={busy}
        onclick={() => void open("nodes")}
      >
        File to node…
      </button>
      <!-- Clears the set, not the inspector: the asset the inspector is
           describing was chosen by a plain click, which is a different
           selection and not this button's to drop. Deliberately NOT gated
           on `busy` (unlike the two action buttons): Clear must always be
           an escape hatch, at the cost that clearing mid-assignment hides
           the bar before the "Tagged N" confirmation renders — the
           assignment itself still lands. -->
      <button class="ctl-btn sel-quiet" onclick={() => onclear()}>Clear</button>
    </div>

    {#if picker === "tags"}
      <div class="sel-picker" role="group" aria-label="Tag picker">
        <Notices notices={listNotices} />
        <!-- Nothing about the vocabulary until the read says so, and nothing
             at all once it has failed: the error below is the whole story,
             and the rows of an earlier read would look like this one's. -->
        {#if loaded}
          {#if tags.length === 0}
            <p class="empty">No tags yet — the box below makes the first.</p>
          {/if}
          <!-- Keyed by tag: `tags_list` tallies into a map keyed by the tag
               name, so one name appears once. -->
          <ul class="sel-options">
            {#each tags as row (row.tag)}
              <li>
                <button
                  class="ctl-btn"
                  aria-pressed={picked.includes(row.tag)}
                  onclick={() => toggle(row.tag)}
                >
                  {row.tag}
                </button>
              </li>
            {/each}
          </ul>
        {:else if error === null}
          <p class="empty">Reading the tags…</p>
        {/if}
        <div class="ctl-actions">
          <input
            class="ctl-input"
            type="text"
            aria-label="New tag"
            placeholder="New tag"
            bind:value={typed}
          />
          <button class="ctl-btn" disabled={busy} onclick={() => void applyTags()}>
            Apply tags
          </button>
        </div>
      </div>
    {/if}

    {#if picker === "nodes"}
      <div class="sel-picker" role="group" aria-label="Node picker">
        <Notices notices={listNotices} />
        <!-- Same rule as the tag picker: no claim about the structure before
             the read lands, and no rows from an earlier one after it fails. -->
        {#if loaded}
          {#if fileable.length === 0}
            <p class="empty">No PARA nodes to file into — Organize makes them.</p>
          {/if}
          <!-- Keyed by node id, the catalog's own key for a node. One click
               files: there is one node to choose, so choosing it is the
               whole action. -->
          <ul class="sel-options">
            {#each fileable as node (node.id)}
              <li>
                <button
                  class="ctl-btn"
                  disabled={busy}
                  onclick={() => void fileTo(node)}
                >
                  {node.kind}/{node.name}
                </button>
              </li>
            {/each}
          </ul>
        {:else if error === null}
          <p class="empty">Reading the PARA nodes…</p>
        {/if}
      </div>
    {/if}

    <!-- Always in the document, empty until an action has answered: a
         `role="status"` element created together with its text is not
         reliably announced. Same rule, and the same reason, as the
         surfaces' count lines. -->
    <div role="status">
      {#if result}
        <p class="count">{result}</p>
        <!-- Keyed by asset: `AssignOutcome.failed` carries one row per
             asset it would not touch. -->
        <ul class="sel-fails">
          {#each failed as fail (fail.asset)}
            <li>{fail.asset} — {fail.reason}</li>
          {/each}
        </ul>
        <Notices notices={resultNotices} />
      {/if}
    </div>

    {#if error}
      <Notices notices={failureNotices} />
      <p class="error" role="alert">{error}</p>
    {/if}
  </div>
{/if}

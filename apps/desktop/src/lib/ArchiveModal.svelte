<script lang="ts">
  /**
   * The archive dry-run preview, the GUI's counterpart of the MCP dry-run
   * default: archiving a PARA node can move real directories, so this
   * describes state read from disk first and only a second, explicit click
   * executes it.
   *
   * Mounted when the modal opens and destroyed when it closes, so reopening
   * plans afresh — a preview left over from a node the user has already
   * dismissed cannot be what a confirm acts on.
   */
  import { api, errorMessage, errorNotices } from "./api";
  import type { ArchiveOutcome, MoveStatus, ParaNodeRow } from "./api";
  import Notices from "./Notices.svelte";

  let {
    node,
    onclose,
    onarchived,
  }: {
    node: ParaNodeRow;
    /** Dismissed with nothing archived. */
    onclose: () => void;
    /** The archive ran: the catalog has changed, so the node list is stale. */
    onarchived: () => void;
  } = $props();

  /** Keyed by the union, so a new `MoveStatus` cannot be added without a
   *  word for it in the preview. */
  const MOVE_LABELS: Record<MoveStatus, string> = {
    moved: "moved",
    already_archived: "already archived",
    planned: "planned",
  };

  /** The roots the preview was planned against — the same list the confirm
   *  must be run with, or it would archive against something else. */
  let roots = $state<string[]>([]);
  let preview = $state<ArchiveOutcome | null>(null);
  let error = $state<string | null>(null);
  let failureNotices = $state<string[]>([]);
  let busy = $state(false);
  let cancelButton = $state<HTMLButtonElement | null>(null);

  $effect(() => {
    void plan();
  });

  /**
   * Opening the modal moves focus into it, and onto the safe half of the
   * choice: Cancel, not the button that moves directories. No focus trap —
   * the cheap half of modal behaviour (focus in, Escape out, focus back to
   * the trigger) is what this first `aria-modal` in the app commits to;
   * trapping Tab needs a shared helper, and there is one modal to share it
   * between.
   */
  $effect(() => {
    cancelButton?.focus();
  });

  /**
   * The candidate roots are the volumes mounted right now: nothing in the
   * catalog records where a node was materialized, and a directory can only
   * be moved on a volume that is plugged in.
   */
  async function plan() {
    try {
      const mounted = await api.listMountedRoots();
      roots = mounted.map((root) => root.path);
      preview = await api.archiveNode(node.id, roots, true);
    } catch (failure) {
      error = errorMessage(failure);
      failureNotices = errorNotices(failure);
    }
  }

  /**
   * The second, explicit click. A failure keeps the modal open: a
   * multi-root run that failed partway through has already moved real
   * directories, and its `moved <from> -> <to>` notices are the only record
   * of that — closing over them would throw the record away. The catalog is
   * untouched in that case (the archive event is emitted only after every
   * root has moved), and the same confirm re-run converges: a root moved by
   * the failed attempt reports `already archived` rather than failing
   * again.
   */
  async function confirm() {
    busy = true;
    error = null;
    failureNotices = [];
    try {
      await api.archiveNode(node.id, roots, false);
    } catch (failure) {
      error = errorMessage(failure);
      failureNotices = errorNotices(failure);
      await replan();
      busy = false;
      return;
    }
    onarchived();
  }

  /**
   * The plan is stale the moment a root has moved, so a failed confirm is
   * followed by a fresh dry run: the root that did move comes back as
   * `already archived` instead of sitting there as `planned` above a
   * `moved …` line saying otherwise. One plan, one confirm — the refreshed
   * plan is what the next confirm would act on.
   */
  async function replan() {
    try {
      preview = await api.archiveNode(node.id, roots, true);
    } catch {
      // A refresh that fails must not displace the confirm's own message or
      // its `moved …` lines: those record directories that really moved.
      // The stale plan goes instead — rows saying `planned` for a root
      // already moved would be this modal lying about the disk — which also
      // takes the confirm button with it. Reopening plans afresh.
      preview = null;
    }
  }

  /** "1 mounted root" / "2 mounted roots" — shared by both lines that count
   *  them, so the two can never disagree about the plural. */
  function rootCount(mounted: number): string {
    return `${mounted} mounted root${mounted === 1 ? "" : "s"}`;
  }

  /**
   * What a preview with no moves means. Not "nothing to do": archiving a
   * node IS the `para_node_archive` event, and that happens whether or not
   * a directory goes with it. Only the disk half is absent — either because
   * nothing is mounted to look at, or because this node was never
   * materialized on the volumes that are.
   */
  function eventOnly(mounted: number): string {
    if (mounted === 0) {
      return (
        "No volumes are mounted, so there is nothing on disk to move — " +
        "this node archives by event only."
      );
    }
    return (
      `No materialized directory for this node on the ${rootCount(mounted)}` +
      " — it archives by event only, and nothing on disk moves."
    );
  }
</script>

<!-- `!busy`: dismissing mid-confirm would unmount this component before the
     rejection lands, and a partial archive's `moved …` notices are the only
     record that real directories moved. The confirm settles first. -->
<svelte:window
  onkeydown={(event) => {
    if (event.key === "Escape" && !busy) onclose();
  }}
/>

<div class="org-overlay">
  <div
    class="org-modal"
    role="dialog"
    aria-modal="true"
    aria-label="Archive {node.kind}/{node.name}?"
  >
    <h3 class="org-modal-title">Archive {node.kind}/{node.name}?</h3>

    {#if preview === null && error === null}
      <p class="org-modal-sub">Planning the archive…</p>
    {/if}

    {#if preview}
      {#if preview.moves.length > 0}
        <p class="org-modal-sub">Dry run against {rootCount(roots.length)}:</p>
      {:else}
        <p class="org-modal-sub">{eventOnly(roots.length)}</p>
      {/if}
      <!-- The event is a row of the same list as the moves, and the last one
           (mockup: organize.html's preview frame): it is the part of the
           archive that always happens, listed beside the parts that depend
           on what is on disk. -->
      <ul class="org-moves">
        {#each preview.moves as move}
          <li>
            <i class="org-move-status">{MOVE_LABELS[move.status]}</i>
            <span class="org-move-path">{move.from}</span>
            <span aria-hidden="true">→</span>
            <span class="org-move-path">{move.to}</span>
          </li>
        {/each}
        <li>
          <i class="org-move-status">event</i>
          <span class="org-move-path">
            para_node_archive · the node archives by event; asset history kept
          </span>
        </li>
      </ul>
      <Notices notices={preview.notices} />
    {/if}

    {#if error}
      <!-- A partial archive's completed moves arrive here, and they are the
           only record that real directories moved — first, and in full. -->
      <Notices notices={failureNotices} />
      <p class="error" role="alert">{error}</p>
    {/if}

    <div class="org-modal-row">
      <!-- Disabled for the same reason Escape is inert while busy: the
           record of what moved has to land before this can close. -->
      <button
        class="ctl-btn"
        bind:this={cancelButton}
        disabled={busy}
        onclick={() => onclose()}
      >
        Cancel
      </button>
      <!-- Nothing to confirm without a plan: a failed preview leaves the
           modal with Cancel alone rather than an archive run blind. -->
      {#if preview}
        <button class="ctl-btn ctl-warn" disabled={busy} onclick={() => void confirm()}>
          Archive node
        </button>
      {/if}
    </div>
  </div>
</div>

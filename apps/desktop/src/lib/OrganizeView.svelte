<script lang="ts">
  /**
   * The Organize surface: the PARA structure on the left, the tag
   * vocabulary on the right. Both halves are the catalog's taxonomy, and a
   * rename on one side is often prompted by what the other side says — so
   * they share a surface rather than two sidebar entries.
   *
   * Putting assets INTO a node or a tag happens where the assets are (the
   * selection bar in Browse and Search); this surface only manages the
   * structure itself.
   *
   * Archive is the one action here that touches real directories, so it
   * runs as a dry run first: the modal shows the moves the service planned
   * against the volumes mounted right now, and a second, explicit click
   * executes them.
   *
   * WIRE GAP: the mockup's node rows and node detail card carry an asset
   * count ("1,204 assets") and the roots the node is materialized at.
   * `ParaNodeRow` carries neither — `list_para` returns id, kind, name and
   * archived, full stop — so neither is drawn. Both need a field on the
   * Rust row before this surface can show them; a number computed here
   * would be a number the catalog never said.
   */
  import { api, errorMessage, errorNotices } from "./api";
  import type { ParaNodeRow, TagRenameOutcome, TagRow } from "./api";
  import ArchiveModal from "./ArchiveModal.svelte";
  import { isoDay } from "./format";
  import Notices from "./Notices.svelte";
  import { nearDuplicates } from "./organize-tags";

  /**
   * The four PARA kinds and the heading each group carries, in the order
   * the mockup lists them. The kind strings are `para::parse_kind`'s own
   * spellings, and `ParaKind` is closed to exactly these four — a node of
   * some other kind cannot reach this list.
   */
  const KINDS: { kind: string; heading: string }[] = [
    { kind: "project", heading: "Projects" },
    { kind: "area", heading: "Areas" },
    { kind: "resource", heading: "Resources" },
    { kind: "archive", heading: "Archive" },
  ];

  /** Null until `list_para` has answered — an empty array is a catalog with
   *  no nodes in it, which is a different thing to say. */
  let nodes = $state<ParaNodeRow[] | null>(null);
  let paraNotices = $state<string[]>([]);
  let paraError = $state<string | null>(null);
  let paraFailureNotices = $state<string[]>([]);
  let selectedNode = $state<ParaNodeRow | null>(null);
  let nodeRename = $state("");
  let newKind = $state("project");
  let newName = $state("");

  let tags = $state<TagRow[]>([]);
  let tagNotices = $state<string[]>([]);
  let tagError = $state<string | null>(null);
  let tagFailureNotices = $state<string[]>([]);
  /** What the last rename or merge rewrote, as a line under the list. */
  let tagOutcome = $state<string | null>(null);
  let selectedTag = $state<TagRow | null>(null);
  let tagRename = $state("");
  let mergeInto = $state("");
  let filter = $state("");

  /** The node the archive modal is open on, or null when it is closed. */
  let archiving = $state<ParaNodeRow | null>(null);
  /** The button that opened the modal, so focus can go back to it. */
  let archiveTrigger = $state<HTMLButtonElement | null>(null);

  /**
   * Every list read takes the next number, and a response whose number is
   * no longer current is dropped — two mutations confirmed in quick
   * succession each re-read the list, and the older answer must not land
   * on top of the newer one. Same rule the search and browse surfaces
   * follow, for the same reason.
   */
  let paraSeq = 0;
  let tagSeq = 0;

  let hints = $derived(nearDuplicates(tags));
  /** Lowered once per keystroke rather than once per row. */
  let needle = $derived(filter.trim().toLowerCase());
  let shown = $derived(
    tags.filter((row) => row.tag.toLowerCase().includes(needle)),
  );
  /** The tags a merge could target: every one but the tag being merged. */
  let mergeTargets = $derived(
    tags.filter((row) => row.tag !== selectedTag?.tag),
  );

  $effect(() => {
    void loadPara();
  });

  $effect(() => {
    void loadTags();
  });

  async function loadPara() {
    const seq = ++paraSeq;
    try {
      const outcome = await api.listPara();
      if (seq !== paraSeq) return;
      nodes = outcome.nodes;
      paraNotices = outcome.notices ?? [];
      paraError = null;
      paraFailureNotices = [];
      // Re-resolved by id, not kept: a rename changes the row this
      // selection was holding, and a node someone else archived is gone.
      const id = selectedNode?.id;
      selectedNode = outcome.nodes.find((node) => node.id === id) ?? null;
    } catch (failure) {
      if (seq !== paraSeq) return;
      // The failed read owns the column: the tree on screen came from an
      // earlier read and this call is the reason to doubt it.
      nodes = null;
      paraNotices = [];
      selectedNode = null;
      paraError = errorMessage(failure);
      paraFailureNotices = errorNotices(failure);
    }
  }

  async function loadTags() {
    const seq = ++tagSeq;
    try {
      const outcome = await api.listTags();
      if (seq !== tagSeq) return;
      tags = outcome.tags;
      tagNotices = outcome.notices ?? [];
      tagError = null;
      tagFailureNotices = [];
      const name = selectedTag?.tag;
      selectedTag = tags.find((row) => row.tag === name) ?? null;
    } catch (failure) {
      if (seq !== tagSeq) return;
      tags = [];
      tagNotices = [];
      selectedTag = null;
      tagError = errorMessage(failure);
      tagFailureNotices = errorNotices(failure);
    }
  }

  /**
   * Dismissed with nothing archived: focus goes back to the button it came
   * from. The archived path does not do this — the node is archived by
   * then, so the "Archive…" button is gone from the detail card with it.
   */
  function closeArchive() {
    archiving = null;
    archiveTrigger?.focus();
  }

  function selectNode(node: ParaNodeRow) {
    selectedNode = node;
    nodeRename = "";
  }

  function selectTag(row: TagRow) {
    selectedTag = row;
    tagRename = "";
    mergeInto = "";
  }

  /**
   * Runs one PARA mutation and re-reads the list on success. Answers
   * whether it worked, so a caller can clear the input it came from;
   * a failure leaves that input alone and the message on screen.
   */
  async function mutatePara(call: () => Promise<unknown>): Promise<boolean> {
    paraError = null;
    paraFailureNotices = [];
    try {
      await call();
    } catch (failure) {
      paraError = errorMessage(failure);
      paraFailureNotices = errorNotices(failure);
      return false;
    }
    await loadPara();
    return true;
  }

  async function addNode() {
    const name = newName.trim();
    if (name === "") {
      paraError = "a new node needs a name";
      paraFailureNotices = [];
      return;
    }
    // The created id is not kept: the list is what says the node exists,
    // and it is re-read either way.
    if (await mutatePara(() => api.addParaNode(newKind, name))) newName = "";
  }

  async function renameNode(node: ParaNodeRow) {
    const name = nodeRename.trim();
    if (name === "") {
      paraError = "a rename needs a new name";
      paraFailureNotices = [];
      return;
    }
    // By id rather than `<kind>/<name>`: an id resolves whatever the name
    // is doing, including after two machines have raced on it.
    if (await mutatePara(() => api.renameParaNode(node.id, name))) {
      nodeRename = "";
    }
  }

  /**
   * Runs one tag mutation, reports what it rewrote, and re-reads the
   * vocabulary — a rename or a merge retires the tag it started from.
   * Answers whether it worked, on the same terms as `mutatePara`: a failed
   * rename leaves the name the user typed in the box to fix.
   */
  async function mutateTag(
    call: () => Promise<TagRenameOutcome>,
  ): Promise<boolean> {
    tagError = null;
    tagFailureNotices = [];
    tagOutcome = null;
    try {
      const outcome = await call();
      tagOutcome = `Rewrote ${outcome.rewritten} assets`;
    } catch (failure) {
      tagError = errorMessage(failure);
      tagFailureNotices = errorNotices(failure);
      return false;
    }
    await loadTags();
    return true;
  }

  async function renameSelectedTag(row: TagRow) {
    const to = tagRename.trim();
    if (to === "") {
      tagError = "a rename needs a new name";
      tagFailureNotices = [];
      return;
    }
    if (await mutateTag(() => api.renameTag(row.tag, to))) tagRename = "";
  }

  async function mergeSelectedTag(row: TagRow) {
    if (mergeInto === "") {
      tagError = "pick the tag to merge into";
      tagFailureNotices = [];
      return;
    }
    await mutateTag(() => api.mergeTags(row.tag, mergeInto));
  }

</script>

<div class="surface org-surface">
  <section class="org-col" aria-label="PARA structure">
    <p class="org-label">PARA structure</p>

    <!-- The live region is always in the document, empty between reads: a
         `role="status"` element created together with its text is not
         reliably announced, so what changes has to be the contents. Same
         rule, and the same reason, as the search surface. -->
    <div role="status">
      <Notices notices={paraNotices} />
    </div>
    {#if paraError}
      <Notices notices={paraFailureNotices} />
      <p class="error" role="alert">{paraError}</p>
    {/if}

    {#if nodes !== null && nodes.length === 0 && paraError === null}
      <p class="empty">No PARA nodes yet — the box below makes the first.</p>
    {/if}
    <!-- Keyed by kind: `KINDS` is a fixed four-entry table. -->
    {#each KINDS as group (group.kind)}
      {@const rows = (nodes ?? []).filter((node) => node.kind === group.kind)}
      {#if rows.length > 0}
        <p class="org-kind">{group.heading}</p>
        <!-- Keyed by node id, which is the catalog's own key for a node. -->
        <ul class="org-nodes" aria-label={group.heading}>
          {#each rows as node (node.id)}
            <li>
              <button
                class="org-node"
                aria-current={selectedNode?.id === node.id ? "true" : undefined}
                onclick={() => selectNode(node)}
              >
                <span class="org-name">{node.name}</span>
                {#if node.archived}
                  <span class="org-chip">archived</span>
                {/if}
              </button>
            </li>
          {/each}
        </ul>
      {/if}
    {/each}

    {#if selectedNode}
      <!-- Bound once: `selectedNode` is mutable `$state`, which TypeScript
           will not narrow inside the handlers below. -->
      {@const node = selectedNode}
      <div class="ctl-detail" role="group" aria-label="Selected node">
        <!-- The node id is on the title rather than in the card: it is what
             support asks for and nothing a user reads, so it stays out of
             the line that says what state the node is in. -->
        <h3 class="ctl-detail-title" title={node.id}>
          {node.kind}/{node.name}
        </h3>
        <p class="ctl-detail-sub">{node.archived ? "archived" : "active"}</p>
        <div class="ctl-actions">
          <input
            class="ctl-input"
            type="text"
            aria-label="New name for this node"
            placeholder="New name"
            bind:value={nodeRename}
          />
          <button class="ctl-btn" onclick={() => void renameNode(node)}>
            Rename
          </button>
          <!-- The service refuses to archive an archived node, so this is
               absent rather than present and rejected. -->
          {#if !node.archived}
            <button
              class="ctl-btn ctl-warn"
              bind:this={archiveTrigger}
              onclick={() => (archiving = node)}
            >
              Archive…
            </button>
          {/if}
        </div>
      </div>
    {/if}

    <div class="ctl-actions">
      <select
        class="ctl-input"
        aria-label="Kind of the new node"
        bind:value={newKind}
      >
        {#each KINDS as group (group.kind)}
          <option value={group.kind}>{group.kind}</option>
        {/each}
      </select>
      <input
        class="ctl-input"
        type="text"
        aria-label="Name of the new node"
        placeholder="Name"
        bind:value={newName}
      />
      <button class="ctl-btn" onclick={() => void addNode()}>+ New node</button>
    </div>
  </section>

  <section class="org-col" aria-label="Tags">
    <p class="org-label">Tags · {tags.length}</p>
    <input
      class="ctl-input"
      type="search"
      aria-label="Filter tags"
      placeholder="Filter tags…"
      bind:value={filter}
    />

    <div role="status">
      <Notices notices={tagNotices} />
      {#if tagOutcome}
        <p class="count">{tagOutcome}</p>
      {/if}
    </div>
    {#if tagError}
      <Notices notices={tagFailureNotices} />
      <p class="error" role="alert">{tagError}</p>
    {/if}

    <!-- Keyed by tag: `tags_list` tallies into a map keyed by the tag name,
         so one name appears once. -->
    <ul class="org-taglist">
      {#each shown as row (row.tag)}
        {@const hint = hints.get(row.tag)}
        <li>
          <button
            class="org-tagrow"
            aria-current={selectedTag?.tag === row.tag ? "true" : undefined}
            onclick={() => selectTag(row)}
          >
            <span class="org-tagpill">{row.tag}</span>
            {#if hint !== undefined}
              <span class="org-dupe">≈ {hint}</span>
            {/if}
            <small class="org-num">{row.count}</small>
            <small class="org-num">{isoDay(row.last_used_ms)}</small>
          </button>
        </li>
      {/each}
    </ul>

    {#if selectedTag}
      {@const row = selectedTag}
      <div class="ctl-detail" role="group" aria-label="Selected tag">
        <h3 class="ctl-detail-title">{row.tag}</h3>
        <p class="ctl-detail-sub">
          {row.count} assets · last used {isoDay(row.last_used_ms)}
        </p>
        <div class="ctl-actions">
          <input
            class="ctl-input"
            type="text"
            aria-label="New name for this tag"
            placeholder="New name"
            bind:value={tagRename}
          />
          <button class="ctl-btn" onclick={() => void renameSelectedTag(row)}>
            Rename
          </button>
        </div>
        <div class="ctl-actions">
          <select class="ctl-input" aria-label="Merge into" bind:value={mergeInto}>
            <option value="">Merge into…</option>
            {#each mergeTargets as target (target.tag)}
              <option value={target.tag}>{target.tag}</option>
            {/each}
          </select>
          <button class="ctl-btn" onclick={() => void mergeSelectedTag(row)}>
            Merge
          </button>
        </div>
      </div>
    {/if}
  </section>
</div>

{#if archiving}
  <ArchiveModal
    node={archiving}
    onclose={() => closeArchive()}
    onarchived={() => {
      archiving = null;
      void loadPara();
    }}
  />
{/if}

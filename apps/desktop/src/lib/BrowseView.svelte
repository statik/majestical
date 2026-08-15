<script lang="ts">
  /**
   * The Browse surface: the catalog's folder tree on the left, the selected
   * node's assets on the right. Browse reads the catalog, never the disks —
   * an offline volume browses exactly like a mounted one, badged and
   * nothing more.
   *
   * Selecting any node lists its *whole subtree* by default (the Drilldown);
   * the flatten chip turns that off to show the folder's direct children
   * alone.
   *
   * WIRE GAP: the mockup's video cards carry a duration chip
   * (`browse.html:238`). `SearchHit` has no duration — nothing on the browse
   * row says how long a clip runs — so these cards do not draw one. It needs
   * a field on the Rust row before it can be rendered at all.
   */
  import { api, errorMessage, errorNotices } from "./api";
  import type {
    BrowseKind,
    BrowseSort,
    BrowseTreeOutcome,
    BrowseVolume,
    SearchHit,
  } from "./api";
  import { childPath, crumbPath, folderAt, nodeKey } from "./browse-paths";
  import Filmstrip from "./Filmstrip.svelte";
  import { fileSize } from "./format";
  import Notices from "./Notices.svelte";
  import OnlineBadge from "./OnlineBadge.svelte";
  import type { SelectionState } from "./selection";
  import { EMPTY_SELECTION, clickSelection, reconcileSelection } from "./selection";
  import SelectionBar from "./SelectionBar.svelte";
  import { thumbUrl } from "./thumb";

  let {
    onselect,
    inspectorOpen,
  }: {
    /** Show this asset in the inspector. Only a plain click asks for it:
     *  the modified clicks build this surface's own multi-selection, which
     *  is a different selection and none of the inspector's business. */
    onselect: (assetId: string) => void;
    inspectorOpen: boolean;
  } = $props();

  /**
   * The tree is a fourth column, and at a 13" window with the inspector open
   * there is no room for all four — below this width the tree gives up its
   * labels rather than the grid giving up a column of thumbnails.
   */
  const NARROW = "(max-width: 1099px)";

  /** The sort chip's cycle, in the order it cycles. "captured" is the
   *  service's own default, and reaches the wire as no sort at all (see
   *  `list`), so the default itself stays in `majestical_services::browse`. */
  const SORTS: BrowseSort[] = ["captured", "name", "size"];
  /** Keyed by the union, so a new sort value cannot be added without a label
   *  for it — the direction wording is `browse.rs`'s. */
  const SORT_LABELS: Record<BrowseSort, string> = {
    captured: "Captured ↓",
    name: "Name ↑",
    size: "Size ↓",
  };
  /** The kind chip's cycle; `undefined` is every kind. */
  const KINDS: (BrowseKind | undefined)[] = [
    undefined,
    "image",
    "video",
    "audio",
    "pdf",
    "other",
  ];

  let tree = $state<BrowseTreeOutcome | null>(null);
  let volume = $state<BrowseVolume | null>(null);
  let path = $state("");
  let flatten = $state(true);
  let sort = $state<BrowseSort>("captured");
  /** Undefined is every kind. */
  let kind = $state<BrowseKind>();
  let rows = $state<SearchHit[]>([]);
  let count = $state(0);
  let folderCount = $state(0);
  let listNotices = $state<string[]>([]);
  let loading = $state(false);
  /** A failed listing: the tree is still standing, so this stays in the
   *  grid's pane where the assets it failed to fetch would have gone. */
  let error = $state<string | null>(null);
  let failureNotices = $state<string[]>([]);
  /** A failed tree read: there is nothing to browse at all, so it takes the
   *  pane over. */
  let treeError = $state<string | null>(null);
  let treeFailureNotices = $state<string[]>([]);
  let narrow = $state(false);
  /**
   * The keys (see `nodeKey`) of every open branch. A catalog of a dozen
   * drives unfolded at once is not a tree anyone can read, so a branch is
   * shut until something opens it: its caret, or a selection landing inside
   * it. An array rather than a `Set` because `$state` proxies arrays, and
   * this list is a handful of strings long.
   */
  let expanded = $state<string[]>([]);
  /** Same rule as the search surface: a listing the user has already clicked
   *  past must not land on the grid however late it arrives. */
  let requestSeq = 0;
  /** The cards picked out for a bulk action — this surface's own selection,
   *  which the inspector's single one knows nothing about (`selection.ts`). */
  let selection = $state<SelectionState>(EMPTY_SELECTION);
  /** Those cards in the order the grid draws them: the order the assignment
   *  verbs are handed them in. */
  let picked = $derived(
    rows.filter((hit) => selection.selected.has(hit.asset)).map((h) => h.asset),
  );

  let crumbs = $derived(path === "" ? [] : path.split("/"));
  /**
   * Whether there is a listing to draw. A failed first page has nothing to
   * show but its error, and a "0 items" line under that error would be this
   * surface inventing a count the backend never gave it. A failed *later*
   * page is the other way round: the pages already loaded are still true, so
   * they stay on screen beneath the error, and `count` still says how many
   * more there are.
   */
  let showing = $derived(
    volume !== null && treeError === null && (error === null || rows.length > 0),
  );

  $effect(() => {
    void loadTree();
  });

  $effect(() => {
    const query = globalThis.matchMedia(NARROW);
    narrow = query.matches;
    const onChange = (event: MediaQueryListEvent) => {
      narrow = event.matches;
    };
    query.addEventListener("change", onChange);
    return () => query.removeEventListener("change", onChange);
  });

  async function loadTree() {
    try {
      const outcome = await api.browseTree();
      tree = outcome;
      // The first volume opens with the surface: a tree of shut drives says
      // nothing about what this catalog holds.
      const first = outcome.volumes[0];
      if (first !== undefined) expanded = [nodeKey(first.id, "")];
    } catch (failure) {
      treeError = errorMessage(failure);
      treeFailureNotices = errorNotices(failure);
    }
  }

  function isExpanded(volumeId: string, at: string): boolean {
    return expanded.includes(nodeKey(volumeId, at));
  }

  function toggleExpanded(volumeId: string, at: string) {
    const key = nodeKey(volumeId, at);
    expanded = expanded.includes(key)
      ? expanded.filter((open) => open !== key)
      : [...expanded, key];
  }

  /** Opens `at` and every folder above it, so a selection is never somewhere
   *  the tree cannot show — the breadcrumb and the tree agree on where you
   *  are, whatever was shut when you got there. */
  function expandTo(volumeId: string, at: string) {
    const keys = [nodeKey(volumeId, "")];
    let prefix = "";
    for (const segment of at === "" ? [] : at.split("/")) {
      prefix = childPath(prefix, segment);
      keys.push(nodeKey(volumeId, prefix));
    }
    expanded = [...expanded, ...keys.filter((key) => !expanded.includes(key))];
  }

  function select(vol: BrowseVolume, at: string) {
    volume = vol;
    path = at;
    expandTo(vol.id, at);
    void list(0);
  }

  /** `offset` 0 starts the listing over; anything else is the next page,
   *  which appends. */
  async function list(offset: number) {
    const vol = volume;
    if (vol === null) return;
    const seq = ++requestSeq;
    loading = true;
    error = null;
    failureNotices = [];
    try {
      const outcome = await api.browseList({
        volume: vol.id,
        path,
        flatten,
        // "captured" is the service's default, so it travels as no sort at
        // all: the GUI keeps the name it shows on the chip without keeping
        // a second copy of what the default sorts by.
        sort: sort === "captured" ? undefined : sort,
        kind,
        offset,
      });
      if (seq !== requestSeq) return;
      setRows(offset === 0 ? outcome.results : appended(outcome.results));
      count = outcome.count;
      folderCount = outcome.folder_count;
      listNotices = outcome.notices ?? [];
    } catch (failure) {
      if (seq !== requestSeq) return;
      if (offset === 0) {
        // A failed first page owns the pane: leaving the previous folder's
        // grid under the error would attribute those assets to this folder.
        setRows([]);
        count = 0;
        folderCount = 0;
        listNotices = [];
      }
      // A failed *later* page keeps everything already loaded — those pages
      // are still what the catalog said — so the error appears above a grid
      // that is still true, and `count` still offers the rest.
      error = errorMessage(failure);
      failureNotices = errorNotices(failure);
    } finally {
      if (seq === requestSeq) loading = false;
    }
  }

  /** Every path that replaces the rows goes through here, so a selection
   *  can never outlive the cards it was made over. */
  function setRows(next: SearchHit[]) {
    rows = next;
    selection = reconcileSelection(selection, next.map((hit) => hit.asset));
  }

  /** A click on a card. Which asset the inspector shows and which assets the
   *  bar acts on are two selections, and `clickSelection` is where the
   *  modifier keys decide between them. */
  function clickCard(assetId: string, event: MouseEvent) {
    const order = rows.map((hit) => hit.asset);
    const click = clickSelection(selection, order, assetId, event);
    selection = click.state;
    if (click.inspect !== null) onselect(click.inspect);
  }

  /**
   * The next page's rows, minus any asset the grid is already showing.
   * Pagination is over a listing computed per request, so an asset seen or
   * forgotten between two pages shifts every row after it and can hand page
   * two something page one already had. The grid's `{#each}` is keyed by
   * asset id, and a duplicate key is a thrown error that takes the whole
   * surface down — so the duplicate is dropped here, where it costs one
   * card, rather than there, where it costs the pane.
   */
  function appended(next: SearchHit[]): SearchHit[] {
    const known = new Set(rows.map((hit) => hit.asset));
    return [...rows, ...next.filter((hit) => !known.has(hit.asset))];
  }

  function toggleFlatten() {
    flatten = !flatten;
    void list(0);
  }

  function cycleSort() {
    // `?? "captured"`: the modulo cannot walk off the end, but indexing an
    // array says it might.
    sort = SORTS[(SORTS.indexOf(sort) + 1) % SORTS.length] ?? "captured";
    void list(0);
  }

  function cycleKind() {
    kind = KINDS[(KINDS.indexOf(kind) + 1) % KINDS.length];
    void list(0);
  }

  function titled(word: string): string {
    return `${word.slice(0, 1).toUpperCase()}${word.slice(1)}`;
  }

  /** Whether every copy of this asset is on a volume nobody has plugged in.
   *  One mounted copy is enough to reach the bytes, so the card is only
   *  marked offline when none of them is. */
  function isOffline(hit: SearchHit): boolean {
    return hit.volumes.length > 0 && hit.volumes.every((vol) => !vol.online);
  }

  /**
   * `{kind} · {size}`, or `{kind} · {volume}` for an asset whose every copy
   * is offline: a size you cannot open is worth less on that card than the
   * name of the drive to go and plug in. Browse populates kind and size, but
   * the row type is shared with search, which populates neither, so an
   * absent one is left out rather than printed as "undefined".
   */
  function subline(hit: SearchHit): string {
    const parts: string[] = [];
    if (hit.kind !== undefined) parts.push(hit.kind);
    if (isOffline(hit)) {
      parts.push(hit.volumes.map((vol) => vol.label).join(", "));
    } else if (hit.size !== undefined) {
      parts.push(fileSize(hit.size));
    }
    return parts.join(" · ");
  }
</script>

<!-- The thumbnail and the offline mark both come out of the catalog, which
     is why an unplugged drive's assets still have a card worth looking at:
     the picture is here, the bytes are not. -->
{#snippet thumb(hit: SearchHit)}
  <span class="browse-thumb">
    <img src={thumbUrl(hit.asset)} alt="" loading="lazy" />
    {#if isOffline(hit)}
      <span class="browse-offthumb">offline</span>
    {/if}
  </span>
{/snippet}

<!-- The disclosure control, beside the node rather than inside it: the node
     itself is a button, and a button cannot hold another one. A leaf keeps
     the same slot empty so its label still lines up with its siblings'. -->
{#snippet caret(vol: BrowseVolume, at: string, name: string, leaf: boolean)}
  {#if leaf}
    <span class="browse-caret" aria-hidden="true"></span>
  {:else}
    <button
      class="browse-caret"
      aria-expanded={isExpanded(vol.id, at)}
      aria-label="{name} folders"
      onclick={() => toggleExpanded(vol.id, at)}
    >
      {isExpanded(vol.id, at) ? "▾" : "▸"}
    </button>
  {/if}
{/snippet}

<!-- Recursive: a folder renders its own children the same way, addressed by
     the path they hang off. -->
{#snippet branches(vol: BrowseVolume, parent: string)}
  {@const folder = folderAt(vol, parent)}
  {#if folder !== null && folder.children.length > 0 && isExpanded(vol.id, parent)}
    <ul>
      <!-- Keyed by name: `BrowseFolder.children` is a `BTreeSet` on the Rust
           side, so a folder cannot list the same child twice. -->
      {#each folder.children as name (name)}
        {@const at = childPath(parent, name)}
        {@const below = folderAt(vol, at)}
        <li>
          <span class="browse-row">
            {@render caret(
              vol,
              at,
              name,
              below === null || below.children.length === 0,
            )}
            <button
              class="browse-node"
              aria-current={volume?.id === vol.id && path === at
                ? "true"
                : undefined}
              onclick={() => select(vol, at)}
            >
              <span class="browse-label">{name}</span>
            </button>
          </span>
          {@render branches(vol, at)}
        </li>
      {/each}
    </ul>
  {/if}
{/snippet}

<div class="surface browse-surface">
  <nav
    class="browse-tree"
    class:browse-tree-collapsed={inspectorOpen && narrow}
    aria-label="Catalog folders"
  >
    <ul>
      <!-- Keyed by id: `volumes` comes from the catalog's volumes table,
           whose id is its PRIMARY KEY. -->
      {#each tree?.volumes ?? [] as vol (vol.id)}
        {@const root = folderAt(vol, "")}
        <li>
          <span class="browse-row">
            {@render caret(
              vol,
              "",
              vol.label,
              root === null || root.children.length === 0,
            )}
            <button
              class="browse-node browse-vol"
              aria-current={volume?.id === vol.id && path === ""
                ? "true"
                : undefined}
              onclick={() => select(vol, "")}
            >
              <span class="browse-label">{vol.label}</span>
              <!-- No `label`: the node already prints it, and the badge's
                   own `aria-label` still joins it in the button's name
                   ("Talon-2024 offline"). -->
              <OnlineBadge online={vol.online} />
            </button>
          </span>
          {@render branches(vol, "")}
        </li>
      {/each}
    </ul>
  </nav>

  <div class="browse-main">
    <!-- Tree-read degradation notices render here, not in the tree pane:
         the collapsed tree is overflow-hidden at 36px, and a notice must
         never be clipped away (the standing rule in app.css's notice
         comment). -->
    <Notices notices={tree?.notices} />
    {#if treeError}
      <Notices notices={treeFailureNotices} />
      <p class="error" role="alert">{treeError}</p>
    {:else if volume === null}
      <p class="empty">Pick a volume or folder to list what it holds.</p>
    {:else}
      <!-- Bound once: `volume` is a mutable `$state`, which TypeScript will
           not narrow inside the click handlers below. -->
      {@const vol = volume}
      <div class="browse-crumbs">
        <button class="browse-crumb" onclick={() => select(vol, "")}>
          {vol.label}
        </button>
        {#each crumbs as name, index}
          <span class="browse-crumb-sep" aria-hidden="true">›</span>
          {#if index === crumbs.length - 1}
            <b class="browse-crumb-here">{name}</b>
          {:else}
            <button
              class="browse-crumb"
              onclick={() => select(vol, crumbPath(crumbs, index))}
            >
              {name}
            </button>
          {/if}
        {/each}
      </div>

      <div class="chips">
        <button
          class="chip"
          aria-pressed={flatten}
          onclick={() => toggleFlatten()}
        >
          Flatten subfolders
        </button>
        <button class="chip" onclick={() => cycleSort()}>
          Sort: {SORT_LABELS[sort]}
        </button>
        <button class="chip" onclick={() => cycleKind()}>
          Kind: {kind === undefined ? "All" : titled(kind)}
        </button>
      </div>

      {#if error}
        <Notices notices={failureNotices} />
        <p class="error" role="alert">{error}</p>
      {/if}
    {/if}

    <!-- The live region is always in the document, empty between listings: a
         `role="status"` element created together with its text is not
         reliably announced, so what changes has to be the region's
         contents. Same rule, and the same reason, as the search surface. -->
    <div role="status">
      {#if showing}
        <p class="count">{count} items across {folderCount} folders</p>
        <Notices notices={listNotices} />
      {/if}
    </div>

    {#if showing}
      <ul class="grid">
        <!-- Keyed by asset id: `browse_list` dedupes its rows by asset, so
             one id appears once however many instances it has, and
             `appended` keeps that true across pages. -->
        {#each rows as hit (hit.asset)}
          <li>
            <!-- `aria-pressed` only on the cards in the set: an unpressed
                 toggle on every card would announce a selection state this
                 grid does not have until one is made. -->
            <button
              class="card"
              class:card-picked={selection.selected.has(hit.asset)}
              aria-pressed={selection.selected.has(hit.asset)
                ? "true"
                : undefined}
              onclick={(event) => clickCard(hit.asset, event)}
            >
              {#if hit.kind === "video"}
                <Filmstrip assetId={hit.asset}>{@render thumb(hit)}</Filmstrip>
              {:else}
                {@render thumb(hit)}
              {/if}
              <span class="name">{hit.name}</span>
              <span class="browse-sub">{subline(hit)}</span>
            </button>
          </li>
        {/each}
      </ul>

      {#if rows.length < count}
        <button
          class="chip browse-more"
          disabled={loading}
          onclick={() => void list(rows.length)}
        >
          Load more
        </button>
      {/if}
    {/if}

    <!-- Outside the listing: the bar draws itself only when two or more
         cards are picked, and the set it counts is already reconciled
         against whatever the grid is showing. -->
    <SelectionBar
      selected={picked}
      onclear={() => (selection = EMPTY_SELECTION)}
    />
  </div>
</div>

/**
 * What a click on a grid card does to that grid's selection. Browse and
 * Search each own their own selection — the sets are per surface, and
 * switching surfaces drops them with the surface — but the *rules* are one
 * copy, here, because two grids that disagreed about what shift-click does
 * would be two grids the same user has to learn twice.
 *
 * Two selections coexist on a grid, and this module is where they are kept
 * apart:
 *
 * - The inspector's, which is one asset. A plain click sets it.
 * - The bar's, which is a set. ⌘/Ctrl and shift build it, and never touch
 *   the inspector's — a click that is building a set is not a request to
 *   describe some particular asset.
 *
 * A plain click does both: it selects that one asset (set of one, which is
 * below the bar's threshold, so single-select looks exactly as it did before
 * multi-select existed) and hands it to the inspector. Starting the set at
 * the plainly-clicked asset is what makes "click A, ⌘-click B" select both,
 * as it does everywhere else.
 */

/** A grid's selection: the set, and where a shift-range would start. */
export interface SelectionState {
  readonly selected: ReadonlySet<string>;
  /** The last plainly-clicked asset — null before there has been one. */
  readonly anchor: string | null;
}

/** A selection, plus what the click asked of the inspector. */
export interface SelectionClick {
  /** The selection the grid should hold from now on. */
  readonly state: SelectionState;
  /** The asset the inspector should show, or null to leave it alone. */
  readonly inspect: string | null;
}

/** The selection a grid starts with, and the one `Clear` puts it back to. */
export const EMPTY_SELECTION: SelectionState = {
  selected: new Set(),
  anchor: null,
};

/** The modifiers a click carries; a `MouseEvent` satisfies it. */
export interface SelectionModifiers {
  readonly metaKey: boolean;
  readonly ctrlKey: boolean;
  readonly shiftKey: boolean;
}

/**
 * The selection after clicking `assetId` in a grid rendering `order`.
 *
 * `order` is the row order on screen right now, which is what a shift-range
 * has to run over: the user is picking out a run of cards they can see, not
 * a run of whatever the catalog last sorted.
 */
export function clickSelection(
  state: SelectionState,
  order: readonly string[],
  assetId: string,
  event: SelectionModifiers,
): SelectionClick {
  if (event.shiftKey) {
    const run = range(order, state.anchor, assetId);
    const selected = new Set(state.selected);
    for (const id of run.ids) selected.add(id);
    return { state: { selected, anchor: run.anchor }, inspect: null };
  }
  if (event.metaKey || event.ctrlKey) {
    const selected = new Set(state.selected);
    // `delete` answers whether it was there, so the toggle is one lookup.
    if (!selected.delete(assetId)) selected.add(assetId);
    // The anchor stays where the last plain click put it: ⌘ picks assets
    // out one at a time, and a shift-click after one still means "from
    // where I last landed to here".
    return { state: { selected, anchor: state.anchor }, inspect: null };
  }
  return {
    state: { selected: new Set([assetId]), anchor: assetId },
    inspect: assetId,
  };
}

/**
 * Drops from `state` everything `order` no longer holds. A grid replaces its
 * rows wholesale — a new folder, a new search, a chip that re-queries — and
 * a set still holding the last folder's assets would file assets the user
 * cannot see and did not mean.
 */
export function reconcileSelection(
  state: SelectionState,
  order: readonly string[],
): SelectionState {
  const present = new Set(order);
  const selected = new Set<string>();
  for (const id of state.selected) {
    if (present.has(id)) selected.add(id);
  }
  const anchor =
    state.anchor !== null && present.has(state.anchor) ? state.anchor : null;
  return { selected, anchor };
}

/**
 * The contiguous run of rows between the anchor and the clicked asset, and
 * the anchor to keep. With no anchor on screen there is no run to draw, so
 * the click takes that asset alone and anchors there — the next shift-click
 * has a run to make.
 */
function range(
  order: readonly string[],
  anchor: string | null,
  assetId: string,
): { ids: readonly string[]; anchor: string } {
  if (anchor === null) return { ids: [assetId], anchor: assetId };
  const to = order.indexOf(assetId);
  const from = order.indexOf(anchor);
  if (to === -1 || from === -1) return { ids: [assetId], anchor: assetId };
  const [first, last] = from <= to ? [from, to] : [to, from];
  return { ids: order.slice(first, last + 1), anchor };
}

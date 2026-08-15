// What a click on a card does to a grid's selection. Browse and Search both
// route their card clicks through `clickSelection`, so these rules are pinned
// once here rather than twice over two surfaces that could drift apart.
import { expect, test } from "vitest";
import type { SelectionState } from "./selection";
import { EMPTY_SELECTION, clickSelection, reconcileSelection } from "./selection";

/** The rendered row order every case below clicks around in. */
const ORDER = ["a", "b", "c", "d", "e"];

/** A click with no modifier held. */
const PLAIN = { metaKey: false, ctrlKey: false, shiftKey: false };
const META = { metaKey: true, ctrlKey: false, shiftKey: false };
const CTRL = { metaKey: false, ctrlKey: true, shiftKey: false };
const SHIFT = { metaKey: false, ctrlKey: false, shiftKey: true };

/** The state a run of clicks leaves behind, as a plain object to assert on. */
function after(
  clicks: [string, typeof PLAIN][],
  start: SelectionState = EMPTY_SELECTION,
): { selected: string[]; anchor: string | null } {
  let state = start;
  for (const [assetId, event] of clicks) {
    state = clickSelection(state, ORDER, assetId, event).state;
  }
  return { selected: [...state.selected], anchor: state.anchor };
}

test("a plain click selects one asset and hands it to the inspector", () => {
  const click = clickSelection(EMPTY_SELECTION, ORDER, "c", PLAIN);

  expect([...click.state.selected]).toEqual(["c"]);
  expect(click.state.anchor).toBe("c");
  expect(click.inspect).toBe("c");
});

test("a plain click drops whatever the set held before it", () => {
  const built = after([
    ["a", PLAIN],
    ["c", META],
  ]);
  expect(built.selected).toEqual(["a", "c"]);

  const click = clickSelection(
    { selected: new Set(built.selected), anchor: built.anchor },
    ORDER,
    "e",
    PLAIN,
  );
  expect([...click.state.selected]).toEqual(["e"]);
});

test("a ⌘-click adds to the set and leaves the inspector alone", () => {
  const click = clickSelection(
    { selected: new Set(["a"]), anchor: "a" },
    ORDER,
    "c",
    META,
  );

  expect([...click.state.selected]).toEqual(["a", "c"]);
  // Null, not "c": the inspector describes one asset, and building a set of
  // several is not a statement about which one that is.
  expect(click.inspect).toBeNull();
});

test("a second ⌘-click on the same asset takes it back out", () => {
  expect(
    after([
      ["a", PLAIN],
      ["c", META],
      ["c", META],
    ]).selected,
  ).toEqual(["a"]);
});

test("a Ctrl-click toggles exactly as ⌘ does", () => {
  // The same set on the keyboards that have no ⌘.
  expect(
    after([
      ["a", PLAIN],
      ["c", CTRL],
    ]).selected,
  ).toEqual(["a", "c"]);
});

test("a ⌘-click never moves the anchor a plain click set", () => {
  // So a shift-click after one still runs from where the user last landed.
  const state = after([
    ["b", PLAIN],
    ["d", META],
  ]);
  expect(state.anchor).toBe("b");
});

test("a shift-click takes the whole run from the anchor to it", () => {
  const click = clickSelection(
    { selected: new Set(["b"]), anchor: "b" },
    ORDER,
    "d",
    SHIFT,
  );

  expect([...click.state.selected]).toEqual(["b", "c", "d"]);
  expect(click.state.anchor).toBe("b");
  expect(click.inspect).toBeNull();
});

test("a shift-click above the anchor takes the run upwards", () => {
  expect(
    after([
      ["d", PLAIN],
      ["b", SHIFT],
    ]).selected,
  ).toEqual(["d", "b", "c"]);
});

test("a shift-click keeps what ⌘ picked outside the run", () => {
  // The range extends the selection rather than replacing it: a set built
  // one ⌘-click at a time is work, and a shift-click is not a request to
  // throw it away.
  expect(
    after([
      ["a", PLAIN],
      ["e", META],
      ["c", SHIFT],
    ]).selected,
  ).toEqual(["a", "e", "b", "c"]);
});

test("a shift-click with nothing anchored takes that asset alone", () => {
  const click = clickSelection(EMPTY_SELECTION, ORDER, "c", SHIFT);

  expect([...click.state.selected]).toEqual(["c"]);
  // Anchored where it landed, so the next shift-click has a run to make.
  expect(click.state.anchor).toBe("c");
});

test("a shift-click whose anchor has left the rows takes that asset alone", () => {
  // The rows are re-queried under the selection all the time (a new folder,
  // a new search); a range from a row that is no longer drawn is not a range
  // anyone can see.
  const click = clickSelection(
    { selected: new Set(["z"]), anchor: "z" },
    ORDER,
    "c",
    SHIFT,
  );

  expect([...click.state.selected]).toEqual(["z", "c"]);
  expect(click.state.anchor).toBe("c");
});

test("replacing the rows drops the selected assets that are gone", () => {
  const state = reconcileSelection(
    { selected: new Set(["a", "z", "c"]), anchor: "a" },
    ORDER,
  );

  expect([...state.selected]).toEqual(["a", "c"]);
  expect(state.anchor).toBe("a");
});

test("replacing the rows drops an anchor that went with them", () => {
  const state = reconcileSelection(
    { selected: new Set(["c"]), anchor: "z" },
    ORDER,
  );

  expect([...state.selected]).toEqual(["c"]);
  expect(state.anchor).toBeNull();
});

test("rows that hold everything selected reconcile to the same selection", () => {
  const state = reconcileSelection(
    { selected: new Set(["b", "d"]), anchor: "b" },
    ORDER,
  );

  expect([...state.selected]).toEqual(["b", "d"]);
  expect(state.anchor).toBe("b");
});

// The multi-selection, over both grids that have one. Browse and Search
// apply the same rules (`selection.ts`) and raise the same bar
// (`SelectionBar.svelte`), so these cases are written once and run against
// both surfaces: a grid that stops honouring one of them fails here, rather
// than waiting for someone to notice that the two behave differently.
//
// What a modified click means is pinned in `selection.test.ts`, and what the
// bar does once it is up in `SelectionBar.test.ts`. This file is the wiring
// between them — the rendered card order a range runs over, the ids the bar
// is handed, and what replacing the rows does to a set made over the old
// ones.
import type { InvokeArgs } from "@tauri-apps/api/core";
import { clearMocks, mockConvertFileSrc } from "@tauri-apps/api/mocks";
import { render, screen, waitFor } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import type { SearchHit } from "./api";
import BrowseView from "./BrowseView.svelte";
import { browseTree, row } from "./browse-test-support";
import SearchView from "./SearchView.svelte";
import { clickWith, mockCommands, stubMatchMedia } from "./test-support";

/** The four cards every case starts from, in the order they are drawn. */
const FOUR: SearchHit[] = [
  row("a.mov", "video", 10),
  row("b.mov", "video", 20),
  row("c.jpg", "image", 30),
  row("d.jpg", "image", 40),
];
/** What a re-query answers with: the middle card is gone. */
const TWO: SearchHit[] = [row("a.mov", "video", 10), row("c.jpg", "image", 30)];

/** What the bar was sent, recorded per test. */
let sent: InvokeArgs[] = [];

/** The commands the bar reaches from either grid. */
function barHandlers() {
  return {
    list_tags: () => ({
      tags: [{ tag: "b-roll", count: 1, last_used_ms: 1_754_000_000_000 }],
    }),
    assign_tags: (args?: InvokeArgs) => {
      sent.push(args ?? {});
      return { applied: 2, failed: [] };
    },
  };
}

/** Answers `pages` in order, the last one repeating. */
function pager(pages: SearchHit[][]): () => SearchHit[] {
  let asked = 0;
  return () => {
    const page = pages[Math.min(asked, pages.length - 1)] ?? [];
    asked += 1;
    return page;
  };
}

/** One grid under test: how to get those cards on screen, and how to make it
 *  replace them with the next page. */
interface Grid {
  name: string;
  open: (
    pages: SearchHit[][],
    onselect: (assetId: string) => void,
  ) => Promise<void>;
  requery: () => Promise<void>;
}

const GRIDS: Grid[] = [
  {
    name: "Browse",
    open: async (pages, onselect) => {
      const next = pager(pages);
      mockCommands({
        browse_tree: () => browseTree,
        browse_list: () => {
          const results = next();
          return { count: results.length, folder_count: 1, results };
        },
        ...barHandlers(),
      });
      render(BrowseView, { onselect, inspectorOpen: false });
      await userEvent.click(
        await screen.findByRole("button", { name: "ProjectX folders" }),
      );
      await userEvent.click(
        await screen.findByRole("button", { name: "B-Roll" }),
      );
      await waitFor(() => expect(cardFor("a.mov")).toBeTruthy());
    },
    // Another folder lists another set of rows over the same grid.
    requery: () => userEvent.click(screen.getByRole("button", { name: "Campaigns" })),
  },
  {
    name: "Search",
    open: async (pages, onselect) => {
      const next = pager(pages);
      mockCommands({
        list_saved_searches: () => ({ saved: [] }),
        search_assets: () => {
          const results = next();
          return { count: results.length, results };
        },
        ...barHandlers(),
      });
      render(SearchView, { onselect });
      await userEvent.type(screen.getByRole("searchbox"), "clip");
      await waitFor(() => expect(cardFor("a.mov")).toBeTruthy());
    },
    // Another query does the same to this one.
    requery: () => userEvent.type(screen.getByRole("searchbox"), " two"),
  },
];

/** The card whose name starts with this file's — the accessible name carries
 *  the card's own detail after it, and the two grids carry different detail. */
function cardFor(name: string): HTMLElement {
  return screen.getByRole("button", {
    name: (accessible: string) => accessible.startsWith(name),
  });
}

function picked(name: string): string | null {
  return cardFor(name).getAttribute("aria-pressed");
}

beforeEach(() => {
  sent = [];
  mockConvertFileSrc("macos");
  // Browse asks how wide the window is; jsdom implements no media queries.
  stubMatchMedia(false);
});
afterEach(() => {
  clearMocks();
  vi.unstubAllGlobals();
});

test.each(GRIDS)(
  "$name: ⌘-clicking a second card raises the bar and leaves the inspector be",
  async (grid) => {
    const chosen: string[] = [];
    await grid.open([FOUR], (asset) => chosen.push(asset));

    await userEvent.click(cardFor("a.mov"));
    await clickWith(cardFor("c.jpg"), "Meta");

    expect(screen.getByText("2 selected")).toBeTruthy();
    expect(picked("c.jpg")).toBe("true");
    // The inspector still describes the plainly-clicked card: building a
    // set is not a request to describe some particular asset.
    expect(chosen).toEqual(["xxh3:a.mov"]);
  },
);

test.each(GRIDS)(
  "$name: Ctrl-clicking toggles the same way, on the keyboards without ⌘",
  async (grid) => {
    await grid.open([FOUR], () => {});

    await userEvent.click(cardFor("a.mov"));
    await clickWith(cardFor("c.jpg"), "Control");
    await clickWith(cardFor("c.jpg"), "Control");

    // Back to one card, so the bar is gone with it.
    expect(screen.queryByRole("group", { name: "Selection" })).toBeNull();
    expect(picked("c.jpg")).toBeNull();
  },
);

test.each(GRIDS)(
  "$name: shift-clicking takes the whole run of cards as they are drawn",
  async (grid) => {
    const chosen: string[] = [];
    await grid.open([FOUR], (asset) => chosen.push(asset));

    await userEvent.click(cardFor("a.mov"));
    await clickWith(cardFor("c.jpg"), "Shift");

    expect(screen.getByText("3 selected")).toBeTruthy();
    // The card in the middle of the run, which nobody clicked.
    expect(picked("b.mov")).toBe("true");
    expect(picked("d.jpg")).toBeNull();
    expect(chosen).toEqual(["xxh3:a.mov"]);
  },
);

test.each(GRIDS)(
  "$name: a plain click puts the grid back to one card and one inspector",
  async (grid) => {
    const chosen: string[] = [];
    await grid.open([FOUR], (asset) => chosen.push(asset));

    await userEvent.click(cardFor("a.mov"));
    await clickWith(cardFor("c.jpg"), "Meta");
    await userEvent.click(cardFor("d.jpg"));

    expect(screen.queryByRole("group", { name: "Selection" })).toBeNull();
    expect(picked("a.mov")).toBeNull();
    expect(picked("d.jpg")).toBe("true");
    expect(chosen).toEqual(["xxh3:a.mov", "xxh3:d.jpg"]);
  },
);

test.each(GRIDS)(
  "$name: Clear takes the bar down without disturbing the inspector",
  async (grid) => {
    const chosen: string[] = [];
    await grid.open([FOUR], (asset) => chosen.push(asset));

    await userEvent.click(cardFor("a.mov"));
    await clickWith(cardFor("c.jpg"), "Meta");
    await userEvent.click(screen.getByRole("button", { name: "Clear" }));

    expect(screen.queryByRole("group", { name: "Selection" })).toBeNull();
    expect(picked("a.mov")).toBeNull();
    // The inspector's asset was never this button's to drop.
    expect(chosen).toEqual(["xxh3:a.mov"]);
  },
);

test.each(GRIDS)(
  "$name: new rows drop the selected cards they do not hold",
  async (grid) => {
    // A grid replaces its rows wholesale, and a set still holding the last
    // listing's assets would tag assets the user cannot see. The cards
    // that *are* still drawn keep their place in the selection.
    await grid.open([FOUR, TWO], () => {});

    await userEvent.click(cardFor("a.mov"));
    await clickWith(cardFor("b.mov"), "Meta");
    await clickWith(cardFor("c.jpg"), "Meta");
    expect(screen.getByText("3 selected")).toBeTruthy();

    await grid.requery();

    expect(await screen.findByText("2 selected")).toBeTruthy();
    expect(
      screen.queryByRole("button", {
        name: (accessible: string) => accessible.startsWith("b.mov"),
      }),
    ).toBeNull();
  },
);

test.each(GRIDS)(
  "$name: a selection does not come back when its rows do",
  async (grid) => {
    // Dropping the vanished ids is what makes this true: a set that merely
    // hid them behind the rows on screen would light them up again the
    // moment the same listing came back, hours and two folders later.
    await grid.open([FOUR, TWO, FOUR], () => {});

    await userEvent.click(cardFor("a.mov"));
    await clickWith(cardFor("b.mov"), "Meta");
    await clickWith(cardFor("d.jpg"), "Meta");

    await grid.requery();
    await waitFor(() => expect(screen.queryByText("b.mov")).toBeNull());
    await grid.requery();
    await waitFor(() => expect(screen.getByText("b.mov")).toBeTruthy());

    expect(screen.queryByRole("group", { name: "Selection" })).toBeNull();
    expect(picked("b.mov")).toBeNull();
    expect(picked("d.jpg")).toBeNull();
  },
);

test.each(GRIDS)(
  "$name: tagging sends exactly the cards the grid has selected",
  async (grid) => {
    await grid.open([FOUR], () => {});

    // Clicked bottom-up, so the order sent can only come from the rows.
    await userEvent.click(cardFor("d.jpg"));
    await clickWith(cardFor("b.mov"), "Meta");
    await userEvent.click(screen.getByRole("button", { name: "Tag…" }));
    await userEvent.click(
      await screen.findByRole("button", { name: "b-roll" }),
    );
    await userEvent.click(screen.getByRole("button", { name: "Apply tags" }));

    await waitFor(() => expect(sent).toHaveLength(1));
    // In the order the grid draws them, not the order they were clicked.
    expect(sent[0]).toEqual({
      assetIds: ["xxh3:b.mov", "xxh3:d.jpg"],
      tags: ["b-roll"],
    });
    expect(await screen.findByText("Tagged 2 assets")).toBeTruthy();
  },
);

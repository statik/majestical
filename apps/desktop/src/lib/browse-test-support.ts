// The catalog the browse suites browse, and the two moves every one of them
// starts with. Shared by `BrowseView.test.ts` (the tree and what selecting a
// node asks for) and `BrowseView.grid.test.ts` (what comes back and how the
// grid draws it) — one fixture, so a change to the tree's shape cannot leave
// half the suite describing a catalog the other half does not have.
import { render, screen, waitFor } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { expect } from "vitest";
import type { BrowseListOutcome, BrowseTreeOutcome, SearchHit } from "./api";
import BrowseView from "./BrowseView.svelte";
import { mockCommands } from "./test-support";

/** Two volumes, one of them offline, and a three-deep folder tree. */
export const browseTree: BrowseTreeOutcome = {
  volumes: [
    {
      id: "label:SSD-A",
      label: "SSD-A",
      online: true,
      folders: [
        { path: "", children: ["Campaigns", "ProjectX"], recursive_count: 5 },
        { path: "Campaigns", children: [], recursive_count: 1 },
        { path: "ProjectX", children: ["B-Roll"], recursive_count: 4 },
        { path: "ProjectX/B-Roll", children: [], recursive_count: 4 },
      ],
    },
    {
      id: "label:Talon-2024",
      label: "Talon-2024",
      online: false,
      folders: [
        { path: "", children: ["Cards"], recursive_count: 2 },
        { path: "Cards", children: [], recursive_count: 2 },
      ],
    },
  ],
};

/** One browse row: the catalog's own summary of a file on a mounted volume. */
export function row(name: string, kind: string, size: number): SearchHit {
  return {
    asset: `xxh3:${name}`,
    score: 0,
    known: true,
    name,
    volumes: [{ id: "label:SSD-A", label: "SSD-A", online: true }],
    tags: [],
    para: null,
    size,
    mtime_ms: 1_700_000_000_000,
    kind,
  };
}

/** The same row, but every copy of it is on a volume nobody has plugged in. */
export function offlineRow(
  name: string,
  kind: string,
  size: number,
): SearchHit {
  return {
    ...row(name, kind, size),
    volumes: [{ id: "label:Talon-2024", label: "Talon-2024", online: false }],
  };
}

export const oneClip: BrowseListOutcome = {
  count: 1,
  folder_count: 1,
  results: [row("A012_C004.braw", "video", 4_400_000_000)],
};

/** The `browse_list` arguments a call is asserted against. */
export interface ListArgs {
  volume: string;
  path?: string;
  flatten?: boolean;
  sort?: string;
  kind?: string;
  offset?: number;
}

/** Records every `browse_list` request and answers each with `pages[n]`, the
 *  last page repeating for any call past the end. */
export function mockBrowse(pages: BrowseListOutcome[]): ListArgs[] {
  const calls: ListArgs[] = [];
  mockCommands({
    browse_tree: () => browseTree,
    browse_list: (args) => {
      calls.push(args as unknown as ListArgs);
      return pages[Math.min(calls.length - 1, pages.length - 1)];
    },
  });
  return calls;
}

export function renderBrowse(onselect: (assetId: string) => void = () => {}) {
  return render(BrowseView, { onselect, inspectorOpen: false });
}

/**
 * Renders the surface and clicks into `SSD-A › ProjectX › B-Roll`. ProjectX
 * is opened by its caret rather than by selecting it: selecting would list
 * it too, and the request counts these suites assert are of the requests the
 * test itself asked for.
 */
export async function openBRoll(onselect: (assetId: string) => void = () => {}) {
  const view = renderBrowse(onselect);
  await userEvent.click(
    await screen.findByRole("button", { name: "ProjectX folders" }),
  );
  await userEvent.click(await screen.findByRole("button", { name: "B-Roll" }));
  return view;
}

/** A listing that never settles until the test says so, with every later
 *  call answering at once — the shape both staleness tests need. */
export function mockPendingFirst(): {
  settle: { resolve: (value: BrowseListOutcome) => void; reject: (failure: unknown) => void };
  count: () => number;
} {
  const settle = {} as {
    resolve: (value: BrowseListOutcome) => void;
    reject: (failure: unknown) => void;
  };
  let calls = 0;
  mockCommands({
    browse_tree: () => browseTree,
    browse_list: () => {
      calls += 1;
      if (calls === 1) {
        return new Promise<BrowseListOutcome>((resolve, reject) => {
          settle.resolve = resolve;
          settle.reject = reject;
        });
      }
      return {
        count: 1,
        folder_count: 1,
        results: [row("second.mov", "video", 10)],
      };
    },
  });
  return { settle, count: () => calls };
}

/** Selects B-Roll, leaves that listing hanging, then selects Campaigns and
 *  waits for the grid to be showing the second folder's row. */
export async function leaveFirstListingBehind(count: () => number) {
  await openBRoll();
  await waitFor(() => expect(count()).toBe(1));
  await userEvent.click(screen.getByRole("button", { name: "Campaigns" }));
  await waitFor(() => expect(count()).toBe(2));
  await screen.findByText("second.mov");
}

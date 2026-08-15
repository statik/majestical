// What a selected node lists: the toolbar chips that shape the request, the
// count line, the cards, paging, and the failures. The tree that made the
// selection is `BrowseView.test.ts`.
import { clearMocks, mockConvertFileSrc } from "@tauri-apps/api/mocks";
import { fireEvent, screen, waitFor, within } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import {
  browseTree,
  renderBrowse,
  mockBrowse,
  offlineRow,
  oneClip,
  openBRoll,
  row,
} from "./browse-test-support";
import {
  mockCommands,
  rejectCommand,
  stubManifest,
  stubMatchMedia,
} from "./test-support";

beforeEach(() => {
  mockConvertFileSrc("macos");
  stubMatchMedia(false);
});
afterEach(() => {
  clearMocks();
  vi.unstubAllGlobals();
});

test("the flatten chip re-asks for the folder's direct children only", async () => {
  const calls = mockBrowse([oneClip]);
  await openBRoll();
  await waitFor(() => expect(calls).toHaveLength(1));

  const chip = screen.getByRole("button", { name: "Flatten subfolders" });
  expect(chip.getAttribute("aria-pressed")).toBe("true");
  await userEvent.click(chip);

  await waitFor(() => expect(calls).toHaveLength(2));
  expect(calls[1]?.flatten).toBe(false);
  expect(calls[1]?.path).toBe("ProjectX/B-Roll");
  expect(chip.getAttribute("aria-pressed")).toBe("false");
});

test("the sort chip cycles captured, name, size and re-asks each time", async () => {
  const calls = mockBrowse([oneClip]);
  await openBRoll();
  await waitFor(() => expect(calls).toHaveLength(1));
  // Captured is the service's own default, so the first request names no sort.
  expect(calls[0]?.sort).toBeUndefined();

  await userEvent.click(screen.getByRole("button", { name: /^Sort:/u }));
  await waitFor(() => expect(calls).toHaveLength(2));
  expect(calls[1]?.sort).toBe("name");
  expect(screen.getByRole("button", { name: "Sort: Name ↑" })).toBeTruthy();

  await userEvent.click(screen.getByRole("button", { name: /^Sort:/u }));
  await waitFor(() => expect(calls).toHaveLength(3));
  expect(calls[2]?.sort).toBe("size");

  await userEvent.click(screen.getByRole("button", { name: /^Sort:/u }));
  await waitFor(() => expect(calls).toHaveLength(4));
  expect(calls[3]?.sort).toBeUndefined();
  expect(screen.getByRole("button", { name: "Sort: Captured ↓" })).toBeTruthy();
});

test("the kind chip filters to one media kind", async () => {
  const calls = mockBrowse([oneClip]);
  await openBRoll();
  await waitFor(() => expect(calls).toHaveLength(1));
  expect(calls[0]?.kind).toBeUndefined();

  await userEvent.click(screen.getByRole("button", { name: "Kind: All" }));

  await waitFor(() => expect(calls).toHaveLength(2));
  expect(calls[1]?.kind).toBe("image");
  expect(screen.getByRole("button", { name: "Kind: Image" })).toBeTruthy();
});

test("the count line names the items and the folders they came from", async () => {
  mockBrowse([{ count: 142, folder_count: 11, results: [] }]);
  await openBRoll();

  expect(await screen.findByText("142 items across 11 folders")).toBeTruthy();
});

test("what a listing found is announced, not just drawn", async () => {
  const notice = "volume 'Talon-2024' is offline";
  mockBrowse([{ count: 2, folder_count: 1, results: [], notices: [notice] }]);
  renderBrowse();

  // The live region is in the document before any folder is selected: one
  // created together with its text is not reliably announced.
  const live = screen.getByRole("status");
  expect(live.textContent).toBe("");

  await userEvent.click(
    await screen.findByRole("button", { name: "ProjectX folders" }),
  );
  await userEvent.click(await screen.findByRole("button", { name: "B-Roll" }));

  await waitFor(() =>
    expect(within(live).getByText("2 items across 1 folders")).toBeTruthy(),
  );
  expect(within(live).getByText(notice)).toBeTruthy();
});

test("a listing's notices render verbatim under its count", async () => {
  const notice = "volume 'Talon-2024' is offline — paths are from the catalog";
  mockBrowse([{ count: 1, folder_count: 1, results: [], notices: [notice] }]);
  await openBRoll();

  await screen.findByText(notice);
});

test("a card names its kind and size, and selects its asset when clicked", async () => {
  const chosen: string[] = [];
  const events: MouseEvent[] = [];
  mockBrowse([oneClip]);
  await openBRoll((asset, event) => {
    chosen.push(asset);
    events.push(event);
  });

  const card = await screen.findByRole("button", { name: /A012_C004/u });
  expect(within(card).getByText("video · 4.1 GB")).toBeTruthy();

  // One `setup()` session, so the held modifier is still held at the click —
  // the direct API starts a fresh session per call and would drop it.
  const user = userEvent.setup();
  await user.keyboard("{Meta>}");
  await user.click(card);
  await user.keyboard("{/Meta}");

  expect(chosen).toEqual(["xxh3:A012_C004.braw"]);
  // The click itself is handed over, not just the id: which modifier was
  // held is the caller's to read, and only the event carries it.
  expect(events[0]?.metaKey).toBe(true);
});

test("a card whose every copy is offline says so instead of its size", async () => {
  // The catalog holds the thumbnail either way; what it cannot promise is
  // the bytes. Naming the drive is what says how to get them.
  mockBrowse([
    {
      count: 1,
      folder_count: 1,
      results: [offlineRow("drone_pass_04.mov", "video", 1_200_000_000)],
    },
  ]);
  const { container } = await openBRoll();

  const card = await screen.findByRole("button", { name: /drone_pass_04/u });
  expect(within(card).getByText("video · Talon-2024")).toBeTruthy();
  expect(within(card).queryByText(/GB/u)).toBeNull();
  expect(container.querySelector(".browse-offthumb")).not.toBeNull();
});

test("a card on a mounted volume keeps its size and stays unmarked", async () => {
  mockBrowse([oneClip]);
  const { container } = await openBRoll();

  const card = await screen.findByRole("button", { name: /A012_C004/u });
  expect(within(card).getByText("video · 4.1 GB")).toBeTruthy();
  expect(container.querySelector(".browse-offthumb")).toBeNull();
});

test("a page short of the count offers the next one and appends it", async () => {
  const calls = mockBrowse([
    {
      count: 3,
      folder_count: 1,
      results: [row("a.mov", "video", 10), row("b.mov", "video", 20)],
    },
    { count: 3, folder_count: 1, results: [row("c.jpg", "image", 30)] },
  ]);
  await openBRoll();

  await userEvent.click(
    await screen.findByRole("button", { name: "Load more" }),
  );

  await screen.findByText("c.jpg");
  expect(calls[1]?.offset).toBe(2);
  // The first page is still there: page two is appended, not swapped in.
  expect(screen.getByText("a.mov")).toBeTruthy();
  expect(screen.getByText("b.mov")).toBeTruthy();
  expect(screen.queryByRole("button", { name: "Load more" })).toBeNull();
});

test("a failed next page leaves the pages already loaded standing", async () => {
  // Those pages are still what the catalog said. The button stays too:
  // `count` still says there is more, and it is this attempt that failed.
  const message = "catalog is locked by another process";
  let calls = 0;
  mockCommands({
    browse_tree: () => browseTree,
    browse_list: () => {
      calls += 1;
      if (calls === 1) {
        return {
          count: 3,
          folder_count: 1,
          results: [row("a.mov", "video", 10), row("b.mov", "video", 20)],
        };
      }
      return rejectCommand(message);
    },
  });
  await openBRoll();

  await userEvent.click(
    await screen.findByRole("button", { name: "Load more" }),
  );

  expect((await screen.findByRole("alert")).textContent).toBe(message);
  expect(screen.getByText("a.mov")).toBeTruthy();
  expect(screen.getByText("b.mov")).toBeTruthy();
  expect(screen.getByText("3 items across 1 folders")).toBeTruthy();
  expect(screen.getByRole("button", { name: "Load more" })).toBeTruthy();
});

test("a next page repeating an asset draws it once, not twice", async () => {
  // Pagination is per request: an asset seen or forgotten between two pages
  // shifts every row after it, so page two can hold a row page one had. The
  // grid is keyed by asset id, and a duplicate key throws the pane away.
  mockBrowse([
    {
      count: 3,
      folder_count: 1,
      results: [row("a.mov", "video", 10), row("b.mov", "video", 20)],
    },
    {
      count: 3,
      folder_count: 1,
      results: [row("b.mov", "video", 20), row("c.jpg", "image", 30)],
    },
  ]);
  await openBRoll();

  await userEvent.click(
    await screen.findByRole("button", { name: "Load more" }),
  );

  await screen.findByText("c.jpg");
  expect(screen.getAllByText("b.mov")).toHaveLength(1);
  expect(screen.queryByRole("alert")).toBeNull();
});

test("a rejected listing reports its whole message chain", async () => {
  const message = "unknown volume 'label:SSD-A' — run `maj volumes list`";
  const notice = "warning: skipped 1 corrupt event log line(s) in /x/events";
  mockCommands({
    browse_tree: () => browseTree,
    browse_list: () => rejectCommand(message, [notice]),
  });
  await openBRoll();

  const alert = await screen.findByRole("alert");
  expect(alert.textContent).toBe(message);
  expect(screen.getByText(notice)).toBeTruthy();
});

test("hover-scrubbing a video card never renames the card", async () => {
  // The filmstrip redraws per pixel of pointer travel; if any of it reached
  // the accessibility tree, the card would be renamed just as often.
  const manifest = { model_tag: "scene-v1", detected: 2, timestamps: [0, 60_000] };
  stubManifest(200, JSON.stringify(manifest));
  mockBrowse([oneClip]);
  const { container } = await openBRoll();

  const card = await screen.findByRole("button", {
    name: "A012_C004.braw video · 4.1 GB",
  });
  const film = container.querySelector(".browse-film") as HTMLElement;
  vi.spyOn(film, "getBoundingClientRect").mockReturnValue({
    left: 0,
    width: 100,
  } as DOMRect);

  await fireEvent.pointerEnter(film);
  await fireEvent.pointerMove(film, { clientX: 80 });
  await waitFor(() =>
    expect(container.querySelector(".browse-frame")).not.toBeNull(),
  );

  // Same card, same name, with a keyframe and its timecode drawn over it.
  expect(screen.getByText("@1m00s")).toBeTruthy();
  expect(
    screen.getByRole("button", { name: "A012_C004.braw video · 4.1 GB" }),
  ).toBe(card);
});

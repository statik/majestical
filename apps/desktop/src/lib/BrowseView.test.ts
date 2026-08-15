// The tree pane: what it draws, what opens and shuts it, and what selecting
// a node asks the backend for. What comes back — the grid, its chips, its
// paging and its failures — is `BrowseView.grid.test.ts`.
import { clearMocks, mockConvertFileSrc } from "@tauri-apps/api/mocks";
import { render, screen, waitFor, within } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import BrowseView from "./BrowseView.svelte";
import {
  leaveFirstListingBehind,
  mockBrowse,
  mockPendingFirst,
  oneClip,
  openBRoll,
  renderBrowse,
  row,
} from "./browse-test-support";
import {
  mockCommands,
  rejectCommand,
  settlingTime,
  stubMatchMedia,
} from "./test-support";

beforeEach(() => {
  mockConvertFileSrc("macos");
  // A window wide enough for all four columns; the two tests about the
  // collapsed tree answer the query the other way themselves.
  stubMatchMedia(false);
});
afterEach(() => {
  clearMocks();
  vi.unstubAllGlobals();
});

test("the tree lists every volume with its state and nests its folders", async () => {
  mockBrowse([oneClip]);
  renderBrowse();

  const projectX = await screen.findByRole("button", { name: "ProjectX" });
  expect(screen.getByRole("button", { name: "SSD-A online" })).toBeTruthy();
  await userEvent.click(
    screen.getByRole("button", { name: "ProjectX folders" }),
  );
  // Nested, not a sibling: B-Roll lives inside ProjectX's own list item.
  const branch = projectX.closest("li") as HTMLElement;
  expect(within(branch).getByRole("button", { name: "B-Roll" })).toBeTruthy();
  // An offline volume browses identically — it is only badged.
  expect(
    screen.getByRole("button", { name: "Talon-2024 offline" }),
  ).toBeTruthy();
});

test("the first volume is open and the rest are closed", async () => {
  mockBrowse([oneClip]);
  renderBrowse();

  // The tree opens on something rather than on a row of shut drives, but on
  // one volume's worth: a catalog of a dozen must not unfold at once.
  expect(await screen.findByRole("button", { name: "ProjectX" })).toBeTruthy();
  expect(screen.queryByRole("button", { name: "Cards" })).toBeNull();
  expect(
    screen
      .getByRole("button", { name: "SSD-A folders" })
      .getAttribute("aria-expanded"),
  ).toBe("true");
  const shut = screen.getByRole("button", { name: "Talon-2024 folders" });
  expect(shut.getAttribute("aria-expanded")).toBe("false");

  await userEvent.click(shut);

  expect(await screen.findByRole("button", { name: "Cards" })).toBeTruthy();
  expect(shut.getAttribute("aria-expanded")).toBe("true");
});

test("a caret closes the branch it opened", async () => {
  mockBrowse([oneClip]);
  renderBrowse();

  const caret = await screen.findByRole("button", { name: "ProjectX folders" });
  expect(caret.getAttribute("aria-expanded")).toBe("false");

  await userEvent.click(caret);
  expect(screen.getByRole("button", { name: "B-Roll" })).toBeTruthy();

  await userEvent.click(caret);
  expect(screen.queryByRole("button", { name: "B-Roll" })).toBeNull();
  expect(caret.getAttribute("aria-expanded")).toBe("false");
});

test("a folder with nothing under it has no caret to open", async () => {
  mockBrowse([oneClip]);
  renderBrowse();

  await screen.findByRole("button", { name: "Campaigns" });
  expect(
    screen.queryByRole("button", { name: "Campaigns folders" }),
  ).toBeNull();
});

test("selecting a node opens the branch it sits on", async () => {
  mockBrowse([oneClip]);
  const { container } = await openBRoll();

  // Shut the branch the selection sits on: B-Roll is out of the tree.
  await userEvent.click(
    screen.getByRole("button", { name: "ProjectX folders" }),
  );
  expect(screen.queryByRole("button", { name: "B-Roll" })).toBeNull();

  // Walking back up to ProjectX from the breadcrumb selects it, and a
  // selection is never somewhere the tree cannot show.
  const crumbs = container.querySelector(".browse-crumbs") as HTMLElement;
  await userEvent.click(
    within(crumbs).getByRole("button", { name: "ProjectX" }),
  );

  expect(await screen.findByRole("button", { name: "B-Roll" })).toBeTruthy();
  expect(
    screen
      .getByRole("button", { name: "ProjectX folders" })
      .getAttribute("aria-expanded"),
  ).toBe("true");
});

test("selecting a folder lists its whole subtree on that volume", async () => {
  const calls = mockBrowse([oneClip]);
  await openBRoll();

  await waitFor(() => expect(calls).toHaveLength(1));
  expect(calls[0]).toEqual({
    volume: "label:SSD-A",
    path: "ProjectX/B-Roll",
    flatten: true,
    sort: undefined,
    kind: undefined,
    offset: 0,
  });
  expect(
    (await screen.findByRole("button", { name: "B-Roll" })).getAttribute(
      "aria-current",
    ),
  ).toBe("true");
});

test("selecting a volume lists it from its root", async () => {
  const calls = mockBrowse([oneClip]);
  renderBrowse();

  await userEvent.click(
    await screen.findByRole("button", { name: "SSD-A online" }),
  );

  await waitFor(() => expect(calls).toHaveLength(1));
  expect(calls[0]?.volume).toBe("label:SSD-A");
  expect(calls[0]?.path).toBe("");
});

test("a stale listing never overwrites a newer folder's rows", async () => {
  const { settle, count } = mockPendingFirst();
  await leaveFirstListingBehind(count);

  settle.resolve({
    count: 1,
    folder_count: 1,
    results: [row("stale.mov", "video", 10)],
  });
  await settlingTime();

  expect(screen.queryByText("stale.mov")).toBeNull();
  expect(screen.getByText("second.mov")).toBeTruthy();
});

test("a stale failure never puts its error over a newer folder's rows", async () => {
  const { settle, count } = mockPendingFirst();
  await leaveFirstListingBehind(count);

  settle.reject({ message: "unknown volume 'label:SSD-A'" });
  await settlingTime();

  expect(screen.queryByRole("alert")).toBeNull();
  expect(screen.getByText("second.mov")).toBeTruthy();
});

test("a breadcrumb walks back up to any folder above the selection", async () => {
  const calls = mockBrowse([oneClip]);
  const { container } = await openBRoll();
  await waitFor(() => expect(calls).toHaveLength(1));

  const crumbs = container.querySelector(".browse-crumbs") as HTMLElement;
  expect(crumbs.textContent).toContain("SSD-A");
  expect(crumbs.textContent).toContain("ProjectX");
  expect(crumbs.textContent).toContain("B-Roll");

  await userEvent.click(
    within(crumbs).getByRole("button", { name: "ProjectX" }),
  );

  await waitFor(() => expect(calls).toHaveLength(2));
  expect(calls[1]?.path).toBe("ProjectX");
});

test("a rejected tree leaves the surface saying why", async () => {
  const message = "no catalog selected yet — initialize or choose one first";
  mockCommands({ browse_tree: () => rejectCommand(message) });
  renderBrowse();

  expect((await screen.findByRole("alert")).textContent).toBe(message);
});

test("a narrow window with the inspector open collapses the tree", async () => {
  stubMatchMedia(true);
  mockBrowse([oneClip]);
  const { container } = render(BrowseView, {
    onselect: () => {},
    inspectorOpen: true,
  });

  await screen.findByRole("button", { name: "ProjectX" });
  expect(container.querySelector(".browse-tree-collapsed")).not.toBeNull();
});

test("a narrow window on its own leaves the tree open", async () => {
  stubMatchMedia(true);
  mockBrowse([oneClip]);
  const { container } = renderBrowse();

  await screen.findByRole("button", { name: "ProjectX" });
  expect(container.querySelector(".browse-tree-collapsed")).toBeNull();
});

// The bar a grid raises over a multi-selection: when it appears, what it
// counts, what its two pickers offer, and what they send. What the backend's
// answer then looks like on screen is `SelectionBar.outcome.test.ts`; which
// clicks build the selection is `grid-selection.test.ts`'s, over rules pinned
// in `selection.test.ts`. This suite starts with the selection already made.
import { clearMocks } from "@tauri-apps/api/mocks";
import { screen, waitFor, within } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, test } from "vitest";
import type { ParaOutcome, TagsListOutcome } from "./api";
import { tagsOutcome } from "./organize-test-support";
import {
  THREE,
  mockBar,
  openPicker,
  renderBar,
} from "./selection-bar-test-support";
import { deferred, rejectCommand } from "./test-support";

afterEach(() => clearMocks());

test("fewer than two selected assets raise no bar at all", async () => {
  // Single-select is the inspector's business, and looks exactly as it did
  // before the bar existed.
  mockBar();
  const { rerender } = renderBar([]);
  expect(screen.queryByRole("group", { name: "Selection" })).toBeNull();

  await rerender({ selected: ["xxh3:a"], onclear: () => {} });
  expect(screen.queryByRole("group", { name: "Selection" })).toBeNull();
});

test("the bar counts exactly the assets it was handed", async () => {
  mockBar();
  const { rerender } = renderBar(["xxh3:a", "xxh3:b"]);
  // Two is the whole threshold.
  expect(screen.getByText("2 selected")).toBeTruthy();

  await rerender({ selected: THREE, onclear: () => {} });
  const bar = screen.getByRole("group", { name: "Selection" });
  expect(within(bar).getByText("3 selected")).toBeTruthy();
});

test("the tag picker offers the vocabulary the catalog already has", async () => {
  mockBar();
  const picker = await openPicker("Tag…");

  expect(within(picker).getByRole("button", { name: "b-roll" })).toBeTruthy();
  expect(within(picker).getByRole("button", { name: "drone" })).toBeTruthy();
});

test("tagging sends the exact assets and the exact tags picked", async () => {
  const sent = mockBar();
  const picker = await openPicker("Tag…");

  await userEvent.click(await within(picker).findByRole("button", { name: "b-roll" }));
  await userEvent.click(within(picker).getByRole("button", { name: "drone" }));
  await userEvent.click(within(picker).getByRole("button", { name: "Apply tags" }));

  await waitFor(() => expect(sent).toHaveLength(1));
  expect(sent[0]).toEqual({ assetIds: THREE, tags: ["b-roll", "drone"] });
});

test("a tag picked twice is picked back off, not sent twice", async () => {
  const sent = mockBar();
  const picker = await openPicker("Tag…");

  const tag = await within(picker).findByRole("button", { name: "b-roll" });
  await userEvent.click(tag);
  await userEvent.click(tag);
  await userEvent.click(within(picker).getByRole("button", { name: "drone" }));
  await userEvent.click(within(picker).getByRole("button", { name: "Apply tags" }));

  await waitFor(() => expect(sent).toHaveLength(1));
  expect(sent[0]).toEqual({ assetIds: THREE, tags: ["drone"] });
});

test("a tag typed into the box is created by the assignment", async () => {
  // `assign_tags` creates whatever tag it is handed, so a vocabulary of one
  // new word needs no separate verb — the box is the create path.
  const sent = mockBar();
  const picker = await openPicker("Tag…");

  await userEvent.type(
    within(picker).getByRole("textbox", { name: "New tag" }),
    "client-x-2026",
  );
  await userEvent.click(within(picker).getByRole("button", { name: "Apply tags" }));

  await waitFor(() => expect(sent).toHaveLength(1));
  expect(sent[0]).toEqual({ assetIds: THREE, tags: ["client-x-2026"] });
});

test("a typed tag joins the ones picked from the list", async () => {
  const sent = mockBar();
  const picker = await openPicker("Tag…");

  await userEvent.click(await within(picker).findByRole("button", { name: "drone" }));
  await userEvent.type(
    within(picker).getByRole("textbox", { name: "New tag" }),
    "  golden  ",
  );
  await userEvent.click(within(picker).getByRole("button", { name: "Apply tags" }));

  await waitFor(() => expect(sent).toHaveLength(1));
  // Trimmed: the surrounding spaces are typing, not part of the tag.
  expect(sent[0]).toEqual({ assetIds: THREE, tags: ["drone", "golden"] });
});

test("an empty assignment is refused here rather than sent", async () => {
  const sent = mockBar();
  const picker = await openPicker("Tag…");

  await userEvent.click(within(picker).getByRole("button", { name: "Apply tags" }));

  expect((await screen.findByRole("alert")).textContent).toBe(
    "pick a tag from the list or type a new one",
  );
  expect(sent).toHaveLength(0);
});

test("the refusal to send nothing drops the last action's line", async () => {
  mockBar();
  const picker = await openPicker("Tag…");
  await userEvent.click(await within(picker).findByRole("button", { name: "b-roll" }));
  await userEvent.click(within(picker).getByRole("button", { name: "Apply tags" }));
  await screen.findByText("Tagged 3 assets");

  await userEvent.click(screen.getByRole("button", { name: "Tag…" }));
  await userEvent.click(await screen.findByRole("button", { name: "Apply tags" }));

  await screen.findByRole("alert");
  // "Tagged 3 assets" left above "pick a tag…" would credit this click with
  // the last one's work.
  expect(screen.queryByText("Tagged 3 assets")).toBeNull();
});

test("a picker claims nothing about the catalog until its list lands", async () => {
  const list = deferred<TagsListOutcome>();
  mockBar({ list_tags: () => list.promise });
  renderBar();

  await userEvent.click(screen.getByRole("button", { name: "Tag…" }));
  const picker = await screen.findByRole("group", { name: "Tag picker" });
  expect(within(picker).getByText("Reading the tags…")).toBeTruthy();
  // "No tags yet" before the read answers is a claim the bar cannot make.
  expect(within(picker).queryByText(/No tags yet/u)).toBeNull();

  list.settle({ tags: [] });

  expect(await within(picker).findByText(/No tags yet/u)).toBeTruthy();
});

test("the node picker holds the same line until its own list lands", async () => {
  const list = deferred<ParaOutcome>();
  mockBar({ list_para: () => list.promise });
  renderBar();

  await userEvent.click(screen.getByRole("button", { name: "File to node…" }));
  const picker = await screen.findByRole("group", { name: "Node picker" });
  expect(within(picker).getByText("Reading the PARA nodes…")).toBeTruthy();
  expect(within(picker).queryByText(/No PARA nodes/u)).toBeNull();

  list.settle({ nodes: [] });

  expect(await within(picker).findByText(/No PARA nodes/u)).toBeTruthy();
});

test("a failed re-read leaves no rows from the read before it", async () => {
  const message = "catalog is locked by another process";
  let reads = 0;
  mockBar({
    list_tags: () => {
      reads += 1;
      return reads === 1 ? tagsOutcome : rejectCommand(message);
    },
  });
  renderBar();

  await userEvent.click(screen.getByRole("button", { name: "Tag…" }));
  await screen.findByRole("button", { name: "b-roll" });
  await userEvent.keyboard("{Escape}");
  await userEvent.click(screen.getByRole("button", { name: "Tag…" }));

  expect((await screen.findByRole("alert")).textContent).toBe(message);
  // Rows this read could not confirm are rows the user would click believing
  // the catalog still says so.
  expect(screen.queryByRole("button", { name: "b-roll" })).toBeNull();
});

test("the node picker offers every node that is not archived", async () => {
  mockBar();
  const picker = await openPicker("File to node…");

  expect(
    await within(picker).findByRole("button", { name: "project/client-x" }),
  ).toBeTruthy();
  expect(
    within(picker).getByRole("button", { name: "resource/stock-library" }),
  ).toBeTruthy();
  // Filing into an archived node is filing into somewhere nobody is looking.
  expect(
    within(picker).queryByRole("button", { name: /talon-2024/u }),
  ).toBeNull();
});

test("a failed tag list leaves the picker saying why", async () => {
  const message = "no catalog selected yet — initialize or choose one first";
  mockBar({ list_tags: () => rejectCommand(message) });
  renderBar();

  await userEvent.click(screen.getByRole("button", { name: "Tag…" }));

  expect((await screen.findByRole("alert")).textContent).toBe(message);
  // Shutting the picker takes its alert with it: a message about a list that
  // is no longer on screen has nothing left to point at.
  await userEvent.keyboard("{Escape}");
  expect(screen.queryByRole("alert")).toBeNull();
});

test("opening one picker shuts the other", async () => {
  mockBar();
  const picker = await openPicker("Tag…");
  expect(picker.getAttribute("aria-label")).toBe("Tag picker");

  await userEvent.click(screen.getByRole("button", { name: "File to node…" }));

  const open = screen.getAllByRole("group", { name: /picker$/u });
  expect(open).toHaveLength(1);
  expect(open[0]?.getAttribute("aria-label")).toBe("Node picker");
});

test("Escape shuts a picker and leaves the selection standing", async () => {
  mockBar();
  await openPicker("Tag…");

  await userEvent.keyboard("{Escape}");

  expect(screen.queryByRole("group", { name: /picker$/u })).toBeNull();
  expect(screen.getByText("3 selected")).toBeTruthy();
});

test("Clear hands the surface back its own selection to empty", async () => {
  // The bar holds no selection of its own: the grid owns the set, so Clear
  // asks rather than empties.
  let cleared = 0;
  mockBar();
  renderBar(THREE, () => (cleared += 1));

  await userEvent.click(screen.getByRole("button", { name: "Clear" }));

  expect(cleared).toBe(1);
});

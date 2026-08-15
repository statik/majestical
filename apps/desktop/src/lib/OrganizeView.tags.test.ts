// The tag manager: the rows, the filter, the ≈ hint in place, and what
// renaming and merging ask the backend for. `organize-tags.test.ts` pins the
// hint's own rules; this suite is about the column that draws them.
import { clearMocks } from "@tauri-apps/api/mocks";
import { screen, waitFor, within } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, test } from "vitest";
import type { TagsListOutcome } from "./api";
import {
  callsTo,
  mockOrganize,
  renderOrganize,
  tagsOutcome,
} from "./organize-test-support";
import { rejectCommand } from "./test-support";

afterEach(() => clearMocks());

/** Renders the surface and selects the one tag row whose name matches. */
async function selectTag(name: RegExp) {
  renderOrganize();
  await userEvent.click(await screen.findByRole("button", { name }));
  return screen.findByRole("group", { name: "Selected tag" });
}

test("a tag row carries its count and the day it was last used", async () => {
  mockOrganize();
  renderOrganize();

  const row = await screen.findByRole("button", { name: /^b-roll/u });
  expect(row.textContent).toContain("412");
  expect(row.textContent).toContain("2025-07-31");
});

test("the filter narrows the list without asking the backend again", async () => {
  const calls = mockOrganize();
  renderOrganize();

  await screen.findByRole("button", { name: /^drone/u });
  await userEvent.type(
    screen.getByRole("searchbox", { name: "Filter tags" }),
    "golden",
  );

  expect(screen.queryByRole("button", { name: /^drone/u })).toBeNull();
  expect(screen.getByRole("button", { name: /^golden-hour/u })).toBeTruthy();
  expect(screen.getByRole("button", { name: /^goldenhour/u })).toBeTruthy();
  expect(callsTo(calls, "list_tags")).toHaveLength(1);
});

test("two spellings of one tag point at each other in the list", async () => {
  mockOrganize();
  renderOrganize();

  const row = await screen.findByRole("button", { name: /^golden-hour/u });
  expect(row.textContent).toContain("≈ goldenhour");
  expect(
    screen.getByRole("button", { name: /^goldenhour/u }).textContent,
  ).toContain("≈ golden-hour");
  // A tag with no near-duplicate carries no hint at all.
  expect(
    screen.getByRole("button", { name: /^interview/u }).textContent,
  ).not.toContain("≈");
});

test("selecting a tag fills the detail card", async () => {
  mockOrganize();
  const detail = await selectTag(/^golden-hour/u);

  expect(detail.textContent).toContain("golden-hour");
  expect(detail.textContent).toContain("67");
  expect(detail.textContent).toContain("2025-08-05");
});

test("renaming a tag re-reads the list and reports what was rewritten", async () => {
  const calls = mockOrganize();
  const detail = await selectTag(/^golden-hour/u);

  await userEvent.type(
    within(detail).getByRole("textbox", { name: "New name for this tag" }),
    "golden",
  );
  await userEvent.click(within(detail).getByRole("button", { name: "Rename" }));

  await waitFor(() => expect(callsTo(calls, "rename_tag")).toHaveLength(1));
  expect(callsTo(calls, "rename_tag")[0]?.args).toEqual({
    from: "golden-hour",
    to: "golden",
  });
  expect(await screen.findByText("Rewrote 67 assets")).toBeTruthy();
  await waitFor(() => expect(callsTo(calls, "list_tags")).toHaveLength(2));
});

test("merging sends the chosen target as the merge's `into` tag", async () => {
  const calls = mockOrganize();
  const detail = await selectTag(/^goldenhour/u);

  await userEvent.selectOptions(
    within(detail).getByRole("combobox", { name: "Merge into" }),
    "golden-hour",
  );
  await userEvent.click(within(detail).getByRole("button", { name: "Merge" }));

  await waitFor(() => expect(callsTo(calls, "merge_tags")).toHaveLength(1));
  // Tauri renames the Rust `into_tag` parameter to `intoTag` on the wire.
  expect(callsTo(calls, "merge_tags")[0]?.args).toEqual({
    from: "goldenhour",
    intoTag: "golden-hour",
  });
  expect(await screen.findByText("Rewrote 9 assets")).toBeTruthy();
  await waitFor(() => expect(callsTo(calls, "list_tags")).toHaveLength(2));
});

test("the merge picker never offers the tag being merged", async () => {
  mockOrganize();
  const detail = await selectTag(/^goldenhour/u);

  const options = within(detail)
    .getAllByRole("option")
    .map((option) => option.textContent);
  expect(options).not.toContain("goldenhour");
  expect(options).toContain("golden-hour");
});

test("a refused merge renders the service's own remedy, verbatim", async () => {
  const message =
    "no asset carries tag 'goldenhour' — nothing to merge; see `maj tags list`";
  mockOrganize({ merge_tags: () => rejectCommand(message) });
  const detail = await selectTag(/^goldenhour/u);

  await userEvent.selectOptions(
    within(detail).getByRole("combobox", { name: "Merge into" }),
    "golden-hour",
  );
  await userEvent.click(within(detail).getByRole("button", { name: "Merge" }));

  expect((await screen.findByRole("alert")).textContent).toBe(message);
});

test("a refused rename leaves the name that was typed in the box", async () => {
  mockOrganize({ rename_tag: () => rejectCommand("tag 'golden' already exists") });
  const detail = await selectTag(/^golden-hour/u);
  const box = within(detail).getByRole("textbox", {
    name: "New name for this tag",
  });

  await userEvent.type(box, "golden");
  await userEvent.click(within(detail).getByRole("button", { name: "Rename" }));

  await screen.findByRole("alert");
  // The name is what has to be fixed, so it is still there to fix.
  expect((box as HTMLInputElement).value).toBe("golden");
});

test("a failed tag list leaves the column saying why", async () => {
  const message = "no catalog selected yet — initialize or choose one first";
  mockOrganize({ list_tags: () => rejectCommand(message) });
  renderOrganize();

  expect((await screen.findByRole("alert")).textContent).toBe(message);
});

test("the notices a tag list carries are shown with it", async () => {
  mockOrganize({
    list_tags: (): TagsListOutcome => ({
      ...tagsOutcome,
      notices: ["1 event line was unreadable"],
    }),
  });
  renderOrganize();

  expect(await screen.findByText("1 event line was unreadable")).toBeTruthy();
});

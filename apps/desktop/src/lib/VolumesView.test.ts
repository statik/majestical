import { clearMocks } from "@tauri-apps/api/mocks";
import { render, screen, waitFor, within } from "@testing-library/svelte";
import { afterEach, expect, test } from "vitest";
import type { VolumeRow } from "./api";
import { mockCommands, rejectCommand } from "./test-support";
import VolumesView from "./VolumesView.svelte";

afterEach(clearMocks);

const card: VolumeRow = {
  id: "label:Card",
  label: "Card",
  last_seen_ms: 1_700_000_000_000,
  online: true,
  asset_count: 42,
  clock_suspect: false,
};

const archive: VolumeRow = {
  id: "uuid:0123-4567",
  label: "Archive",
  last_seen_ms: 1_700_086_400_000,
  online: false,
  asset_count: 1_204,
  clock_suspect: false,
};

test("every volume gets a row: label, state, asset count and last seen", async () => {
  mockCommands({ list_volumes: () => ({ volumes: [card, archive] }) });
  render(VolumesView);

  const rows = await screen.findAllByRole("row");
  // One header row plus one row per volume.
  expect(rows).toHaveLength(3);

  const first = within(rows[1] as HTMLElement);
  expect(first.getByText("Card")).toBeTruthy();
  expect(first.getByText("label:Card")).toBeTruthy();
  expect(first.getByText("42")).toBeTruthy();
  expect(first.getByText("2023-11-14")).toBeTruthy();

  const second = within(rows[2] as HTMLElement);
  expect(second.getByText("Archive")).toBeTruthy();
  expect(second.getByText("1204")).toBeTruthy();
  expect(second.getByText("2023-11-15")).toBeTruthy();
});

test("the online glyphs carry the word they stand for", async () => {
  mockCommands({ list_volumes: () => ({ volumes: [card, archive] }) });
  render(VolumesView);

  // The CLI's own glyphs, named for anyone who cannot see the difference
  // between a filled and a hollow circle.
  expect(await screen.findByRole("img", { name: "online" })).toBeTruthy();
  expect(screen.getByRole("img", { name: "offline" })).toBeTruthy();
  expect(screen.getByText("●")).toBeTruthy();
  expect(screen.getByText("○")).toBeTruthy();
});

test("a volume whose last-seen time outran the clock is marked suspect", async () => {
  mockCommands({
    list_volumes: () => ({
      volumes: [card, { ...archive, clock_suspect: true }],
    }),
  });
  render(VolumesView);

  const marker = await screen.findByText(/clock suspect/u);
  expect(marker.getAttribute("title")).toMatch(/clock/u);
  // Only the flagged volume is marked.
  expect(screen.getAllByText(/clock suspect/u)).toHaveLength(1);
});

test("notices render above the table, with nothing to dismiss them", async () => {
  const notice = "warning: skipped 1 corrupt event log line(s) in /x/events";
  mockCommands({
    list_volumes: () => ({ volumes: [card], notices: [notice] }),
  });
  const { container } = render(VolumesView);

  const rendered = await screen.findByText(notice);
  const table = container.querySelector("table");
  expect(table).not.toBeNull();
  // A notice under the table is a notice nobody reads before the rows.
  expect(
    // eslint-disable-next-line no-bitwise -- compareDocumentPosition is a bitmask.
    rendered.compareDocumentPosition(table as Node) &
      Node.DOCUMENT_POSITION_FOLLOWING,
  ).toBeTruthy();
  // Read-only surface: nothing here acts on a volume.
  expect(screen.queryAllByRole("button")).toEqual([]);
});

test("a failed listing reports the command's whole message chain", async () => {
  const message = "no catalog selected yet — initialize or choose one first";
  mockCommands({ list_volumes: () => rejectCommand(message) });
  render(VolumesView);

  const alert = await screen.findByRole("alert");
  expect(alert.textContent).toBe(message);
  await waitFor(() => expect(screen.queryByRole("table")).toBeNull());
});

test("a catalog with no volumes yet says so instead of showing an empty table", async () => {
  mockCommands({ list_volumes: () => ({ volumes: [] }) });
  const { container } = render(VolumesView);

  expect(await screen.findByText(/No volumes yet/u)).toBeTruthy();
  expect(container.querySelector("table")).toBeNull();
});

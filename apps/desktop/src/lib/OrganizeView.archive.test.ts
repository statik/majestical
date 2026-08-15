// The archive dry-run modal: what it previews, what it says when there is
// nothing on disk to move, and what confirming does. Archive is the one GUI
// action that moves real directories, so it is never run blind — the modal
// shows a dry run first and a second, explicit click executes it.
import { clearMocks } from "@tauri-apps/api/mocks";
import { screen, waitFor, within } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, test } from "vitest";
import type { ArchiveOutcome, MountedRoot } from "./api";
import {
  archivePreview,
  callsTo,
  isDryRun,
  mockOrganize,
  openArchive,
} from "./organize-test-support";
import { rejectCommand } from "./test-support";

afterEach(() => clearMocks());

test("opening the modal previews the move against every mounted root", async () => {
  const calls = mockOrganize();
  const dialog = await openArchive();

  await waitFor(() => expect(callsTo(calls, "archive_node")).toHaveLength(1));
  expect(callsTo(calls, "list_mounted_roots")).toHaveLength(1);
  expect(callsTo(calls, "archive_node")[0]?.args).toEqual({
    node: "01PROJECT",
    roots: ["/Volumes/SSD-A", "/"],
    dryRun: true,
  });
  const rows = within(dialog).getAllByRole("listitem");
  expect(rows[0]?.textContent).toContain("/Volumes/SSD-A/Projects/client-x");
  expect(rows[0]?.textContent).toContain("/Volumes/SSD-A/Archives/client-x");
  expect(rows[0]?.textContent).toContain("planned");
});

test("the event is the last row of the same list the moves are in", async () => {
  mockOrganize();
  const dialog = await openArchive();

  const event = await within(dialog).findByText(
    "para_node_archive · the node archives by event; asset history kept",
  );
  // The part of the archive that always happens, listed beside the parts
  // that depend on what is on disk — not a footnote under the list.
  const rows = within(dialog).getAllByRole("listitem");
  expect(rows).toHaveLength(2);
  expect(rows.at(-1)?.contains(event)).toBe(true);
  expect(rows.at(-1)?.textContent).toContain("event");
});

test("a preview with nothing to move still lists the event", async () => {
  mockOrganize({
    archive_node: (): ArchiveOutcome => ({ moves: [], executed: false }),
  });
  const dialog = await openArchive();

  const rows = await within(dialog).findAllByRole("listitem");
  expect(rows).toHaveLength(1);
  expect(rows[0]?.textContent).toContain("para_node_archive");
});

test("one mounted root is one root, not one roots", async () => {
  mockOrganize({
    list_mounted_roots: (): MountedRoot[] => [
      { volume: "label:root", label: "root", path: "/" },
    ],
  });
  const dialog = await openArchive();

  expect(
    await within(dialog).findByText("Dry run against 1 mounted root:"),
  ).toBeTruthy();
});

test("a preview with nothing to move says the node archives by event alone", async () => {
  mockOrganize({
    archive_node: (): ArchiveOutcome => ({ moves: [], executed: false }),
  });
  const dialog = await openArchive();

  // Not "nothing to do": the archive still happens, as an event. Only the
  // directory move is absent.
  expect(
    await within(dialog).findByText(
      "No materialized directory for this node on the 2 mounted roots — " +
        "it archives by event only, and nothing on disk moves.",
    ),
  ).toBeTruthy();
  expect(
    within(dialog).getByRole("button", { name: "Archive node" }),
  ).toBeTruthy();
});

test("no mounted volumes at all is a preview too, not a refusal", async () => {
  const calls = mockOrganize({
    list_mounted_roots: (): MountedRoot[] => [],
    archive_node: (): ArchiveOutcome => ({ moves: [], executed: false }),
  });
  const dialog = await openArchive();

  expect(
    await within(dialog).findByText(
      "No volumes are mounted, so there is nothing on disk to move — " +
        "this node archives by event only.",
    ),
  ).toBeTruthy();
  expect(callsTo(calls, "archive_node")[0]?.args).toEqual({
    node: "01PROJECT",
    roots: [],
    dryRun: true,
  });
});

test("two mounted roots are counted as two", async () => {
  mockOrganize();
  const dialog = await openArchive();

  expect(
    await within(dialog).findByText("Dry run against 2 mounted roots:"),
  ).toBeTruthy();
});

test("confirming runs the archive for real, closes and re-reads the list", async () => {
  const calls = mockOrganize();
  const dialog = await openArchive();

  await userEvent.click(
    await within(dialog).findByRole("button", { name: "Archive node" }),
  );

  await waitFor(() => expect(callsTo(calls, "archive_node")).toHaveLength(2));
  expect(callsTo(calls, "archive_node")[1]?.args).toEqual({
    node: "01PROJECT",
    roots: ["/Volumes/SSD-A", "/"],
    dryRun: false,
  });
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  await waitFor(() => expect(callsTo(calls, "list_para")).toHaveLength(2));
});

test("cancelling archives nothing", async () => {
  const calls = mockOrganize();
  const dialog = await openArchive();

  await within(dialog).findByRole("button", { name: "Archive node" });
  await userEvent.click(within(dialog).getByRole("button", { name: "Cancel" }));

  expect(screen.queryByRole("dialog")).toBeNull();
  // The dry run and nothing else — the catalog is untouched, so the node
  // list is not re-read either.
  expect(callsTo(calls, "archive_node")).toHaveLength(1);
  expect(callsTo(calls, "list_para")).toHaveLength(1);
});

test("a partial failure shows the moves that really happened", async () => {
  const message =
    "archiving root /Volumes/NAS-1: source directory does not exist";
  const calls = mockOrganize({
    archive_node: (args) =>
      isDryRun(args)
        ? archivePreview
        : rejectCommand(message, [
            "moved /Volumes/SSD-A/Projects/client-x -> " +
              "/Volumes/SSD-A/Archives/client-x",
          ]),
  });
  const dialog = await openArchive();

  await userEvent.click(
    await within(dialog).findByRole("button", { name: "Archive node" }),
  );

  const alert = await within(dialog).findByRole("alert");
  expect(alert.textContent).toBe(message);
  // Directories really moved before the failure; that line is the only
  // record of it, so it stays on screen with the modal open.
  const moved = within(dialog).getByText(
    "moved /Volumes/SSD-A/Projects/client-x -> " +
      "/Volumes/SSD-A/Archives/client-x",
  );
  // And above the error, not under it: what changed on disk is read before
  // the sentence explaining why the run stopped.
  expect(
    moved.compareDocumentPosition(alert) & Node.DOCUMENT_POSITION_FOLLOWING,
  ).toBe(Node.DOCUMENT_POSITION_FOLLOWING);
  expect(callsTo(calls, "archive_node").length).toBeGreaterThanOrEqual(2);
});

test("a failed confirm re-plans, so no row still claims to be planned", async () => {
  let dryRuns = 0;
  const calls = mockOrganize({
    archive_node: (args) => {
      if (!isDryRun(args)) {
        return rejectCommand("archiving root /Volumes/NAS-1: no such file", [
          "moved /Volumes/SSD-A/Projects/client-x -> " +
            "/Volumes/SSD-A/Archives/client-x",
        ]);
      }
      dryRuns += 1;
      // The second dry run reads the disk the failed confirm left behind:
      // SSD-A has already moved.
      return {
        moves: [
          {
            ...archivePreview.moves[0],
            status: dryRuns === 1 ? "planned" : "already_archived",
          },
        ],
        executed: false,
      };
    },
  });
  const dialog = await openArchive();

  await userEvent.click(
    await within(dialog).findByRole("button", { name: "Archive node" }),
  );

  await waitFor(() =>
    expect(within(dialog).getAllByRole("listitem")[0]?.textContent).toContain(
      "already archived",
    ),
  );
  const archiveCalls = callsTo(calls, "archive_node");
  expect(archiveCalls).toHaveLength(3);
  expect(isDryRun(archiveCalls[2]?.args)).toBe(true);
});

test("a re-plan that fails too takes the stale plan away with it", async () => {
  const calls = mockOrganize({
    archive_node: (args) =>
      isDryRun(args) && callsTo(calls, "archive_node").length === 1
        ? archivePreview
        : rejectCommand("the catalog is gone"),
  });
  const dialog = await openArchive();

  await userEvent.click(
    await within(dialog).findByRole("button", { name: "Archive node" }),
  );

  // Nothing left that could be confirmed against a plan nobody can refresh.
  await waitFor(() =>
    expect(
      within(dialog).queryByRole("button", { name: "Archive node" }),
    ).toBeNull(),
  );
  // The event row goes with the plan: with no preview there is no list.
  expect(within(dialog).queryAllByRole("listitem")).toHaveLength(0);
});

test("a failed preview offers nothing to confirm", async () => {
  const message = "PARA node '01PROJECT' has no recorded kind and name";
  mockOrganize({ archive_node: () => rejectCommand(message) });
  const dialog = await openArchive();

  expect((await within(dialog).findByRole("alert")).textContent).toBe(message);
  // Confirming would archive against a plan nobody has seen.
  expect(
    within(dialog).queryByRole("button", { name: "Archive node" }),
  ).toBeNull();
  expect(within(dialog).getByRole("button", { name: "Cancel" })).toBeTruthy();
});

test("the modal is named after the node it would archive", async () => {
  mockOrganize();
  const dialog = await openArchive();

  expect(dialog.getAttribute("aria-label")).toBe("Archive project/client-x?");
});

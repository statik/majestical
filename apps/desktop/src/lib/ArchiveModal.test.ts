// The modal's own manners: where focus lands when it opens, what dismisses
// it, and where focus goes when it closes. This is the app's first
// `aria-modal`, so these three are the precedent — the cheap half of modal
// behaviour, without a Tab trap there is one modal to share.
import { clearMocks } from "@tauri-apps/api/mocks";
import { screen, waitFor, within } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, test } from "vitest";
import {
  archivePreview,
  callsTo,
  isDryRun,
  mockOrganize,
  openArchive,
} from "./organize-test-support";

afterEach(() => clearMocks());

test("the modal opens with focus on the safe half of the choice", async () => {
  mockOrganize();
  const dialog = await openArchive();

  const cancel = within(dialog).getByRole("button", { name: "Cancel" });
  await waitFor(() => expect(document.activeElement).toBe(cancel));
});

test("Escape dismisses the modal without archiving", async () => {
  const calls = mockOrganize();
  await openArchive();

  await userEvent.keyboard("{Escape}");

  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  expect(callsTo(calls, "archive_node")).toHaveLength(1);
});

test("closing the modal puts focus back on the button that opened it", async () => {
  mockOrganize();
  const dialog = await openArchive();

  await userEvent.click(within(dialog).getByRole("button", { name: "Cancel" }));

  await waitFor(() =>
    expect(document.activeElement).toBe(
      screen.getByRole("button", { name: "Archive…" }),
    ),
  );
});

test("an in-flight confirm cannot be dismissed out from under its own record", async () => {
  let rejectConfirm: ((failure: unknown) => void) | undefined;
  mockOrganize({
    archive_node: (args) =>
      isDryRun(args)
        ? archivePreview
        : new Promise((_resolve, reject) => {
            rejectConfirm = reject;
          }),
  });
  const dialog = await openArchive();
  await userEvent.click(
    await within(dialog).findByRole("button", { name: "Archive node" }),
  );

  // Closing now would unmount the modal before the rejection lands, and a
  // partial archive's `moved …` notices are the only record that real
  // directories moved.
  const cancel = within(dialog).getByRole("button", {
    name: "Cancel",
  }) as HTMLButtonElement;
  await waitFor(() => expect(cancel.disabled).toBe(true));
  await userEvent.click(cancel);
  await userEvent.keyboard("{Escape}");
  expect(screen.getByRole("dialog")).toBeTruthy();

  const settle = rejectConfirm;
  if (settle === undefined) throw new Error("the confirm never reached the mock");
  settle({
    message: "archiving root /Volumes/NAS-1: no such file",
    notices: [
      "moved /Volumes/SSD-A/Projects/client-x -> " +
        "/Volumes/SSD-A/Archives/client-x",
    ],
  });

  // The record landed; now the modal is dismissible again, both ways.
  expect(
    await within(dialog).findByText(
      "moved /Volumes/SSD-A/Projects/client-x -> " +
        "/Volumes/SSD-A/Archives/client-x",
    ),
  ).toBeTruthy();
  await waitFor(() => expect(cancel.disabled).toBe(false));

  await userEvent.keyboard("{Escape}");
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
});

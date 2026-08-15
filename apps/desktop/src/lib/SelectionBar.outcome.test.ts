// What an assignment's answer looks like on screen: the count it applied, the
// per-asset rows it refused, the notices it carried, and the whole message
// chain of one refused outright. The bar those answers land in is
// `SelectionBar.test.ts`.
import { clearMocks } from "@tauri-apps/api/mocks";
import { screen, waitFor, within } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, test } from "vitest";
import type { AssignOutcome } from "./api";
import {
  THREE,
  mockBar,
  openPicker,
  renderBar,
} from "./selection-bar-test-support";
import { deferred, rejectCommand, settlingTime } from "./test-support";

afterEach(() => clearMocks());

test("a finished assignment says how many assets it tagged", async () => {
  mockBar();
  const picker = await openPicker("Tag…");

  await userEvent.click(await within(picker).findByRole("button", { name: "b-roll" }));
  await userEvent.click(within(picker).getByRole("button", { name: "Apply tags" }));

  expect(await screen.findByText("Tagged 3 assets")).toBeTruthy();
});

test("per-asset failures are listed under the count, never swallowed", async () => {
  mockBar({
    assign_tags: (): AssignOutcome => ({
      applied: 2,
      failed: [{ asset: "xxh3:c", reason: "no such asset in the catalog" }],
      notices: ["1 event line was unreadable"],
    }),
  });
  const picker = await openPicker("Tag…");

  await userEvent.click(await within(picker).findByRole("button", { name: "b-roll" }));
  await userEvent.click(within(picker).getByRole("button", { name: "Apply tags" }));

  expect(await screen.findByText("Tagged 2 assets")).toBeTruthy();
  expect(
    screen.getByText("xxh3:c — no such asset in the catalog"),
  ).toBeTruthy();
  expect(screen.getByText("1 event line was unreadable")).toBeTruthy();
});

test("a refused assignment reports its whole message chain", async () => {
  // Every asset failing arrives as a rejected command, not an outcome with a
  // list in it — so the message and its notices are what there is to show.
  const message = "catalog is locked by another process";
  const notice = "warning: skipped 1 corrupt event log line(s) in /x/events";
  mockBar({ assign_tags: () => rejectCommand(message, [notice]) });
  const picker = await openPicker("Tag…");

  await userEvent.click(await within(picker).findByRole("button", { name: "b-roll" }));
  await userEvent.click(within(picker).getByRole("button", { name: "Apply tags" }));

  expect((await screen.findByRole("alert")).textContent).toBe(message);
  expect(screen.getByText(notice)).toBeTruthy();
});

test("filing sends the node's id and names the node in the outcome", async () => {
  const sent = mockBar();
  const picker = await openPicker("File to node…");

  await userEvent.click(
    await within(picker).findByRole("button", { name: "project/client-x" }),
  );

  await waitFor(() => expect(sent).toHaveLength(1));
  // The id, not `<kind>/<name>`: an id resolves whatever the name is doing.
  expect(sent[0]).toEqual({ assetIds: THREE, node: "01PROJECT" });
  expect(await screen.findByText("Filed 3 assets to client-x")).toBeTruthy();
});

test("a refused filing reports the service's own remedy", async () => {
  const message = "unknown PARA node 'client-x' — see `maj para list`";
  mockBar({ file_assets: () => rejectCommand(message) });
  const picker = await openPicker("File to node…");

  await userEvent.click(
    await within(picker).findByRole("button", { name: "project/client-x" }),
  );

  expect((await screen.findByRole("alert")).textContent).toBe(message);
});

test("a finished action shuts its picker and keeps the selection", async () => {
  // The set is the user's work, and one set is often worth two actions
  // (tag it, then file it) — so the bar keeps counting until Clear.
  mockBar();
  const picker = await openPicker("Tag…");

  await userEvent.click(await within(picker).findByRole("button", { name: "b-roll" }));
  await userEvent.click(within(picker).getByRole("button", { name: "Apply tags" }));

  await screen.findByText("Tagged 3 assets");
  expect(screen.queryByRole("group", { name: /picker$/u })).toBeNull();
  expect(screen.getByText("3 selected")).toBeTruthy();
});

test("an outcome never reattaches itself to the next selection", async () => {
  // The bar outlives the selection it was raised over (the surface keeps it
  // mounted), and "Tagged 2 assets" plus a refusal naming one of those two
  // says nothing true about whatever the user picks next.
  mockBar({
    assign_tags: (): AssignOutcome => ({
      applied: 2,
      failed: [{ asset: "xxh3:c", reason: "no such asset in the catalog" }],
    }),
  });
  const { rerender } = renderBar();
  await userEvent.click(screen.getByRole("button", { name: "Tag…" }));
  await userEvent.click(await screen.findByRole("button", { name: "b-roll" }));
  await userEvent.click(screen.getByRole("button", { name: "Apply tags" }));
  await screen.findByText("Tagged 2 assets");

  // A plain click drops the set below the threshold, then two more cards
  // build a new one — different assets, nothing to do with what was tagged.
  await rerender({ selected: ["xxh3:a"], onclear: () => {} });
  await rerender({ selected: ["xxh3:d", "xxh3:e"], onclear: () => {} });

  expect(screen.getByText("2 selected")).toBeTruthy();
  expect(screen.queryByText("Tagged 2 assets")).toBeNull();
  expect(screen.queryByText(/no such asset in the catalog/u)).toBeNull();
});

test("an assignment landing after Clear leaves nothing behind either", async () => {
  // Clear is deliberately not gated on `busy`, so an answer can arrive after
  // the set it ran over is gone — and it describes that set, not the next.
  const assignment = deferred<AssignOutcome>();
  mockBar({ assign_tags: () => assignment.promise });
  const { rerender } = renderBar();
  await userEvent.click(screen.getByRole("button", { name: "Tag…" }));
  await userEvent.click(await screen.findByRole("button", { name: "b-roll" }));
  await userEvent.click(screen.getByRole("button", { name: "Apply tags" }));

  await rerender({ selected: [], onclear: () => {} });
  assignment.settle({ applied: 3, failed: [] });
  // The answer lands while the bar is down, so asserting it never comes back
  // is not just asserting it has not yet.
  await settlingTime();
  await rerender({ selected: ["xxh3:d", "xxh3:e"], onclear: () => {} });

  expect(screen.getByText("2 selected")).toBeTruthy();
  expect(screen.queryByText("Tagged 3 assets")).toBeNull();
});

import { clearMocks } from "@tauri-apps/api/mocks";
import { cleanup, screen, waitFor, within } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, test } from "vitest";
import type { IngestPlanOutcome, IngestState } from "./api";
import {
  callsTo,
  emitProgress,
  ingestRun,
  mockIngest,
  planOutcome,
  renderIngest,
  RUN,
  runPanel,
  runStarted,
  startRun,
} from "./ingest-test-support";
import type { CommandHandler } from "./test-support";
import { deferred, rejectCommand } from "./test-support";

// How a run ends, and how it reads afterwards — including on a surface that
// mounted after the fact, which is the same card from the same struct.
//
// How a run ends. `run_stopped` is the copy loop stopping, not the run: the
// missing-file sweep, the ASC MHL generation per destination and the catalog
// events all land after it, and the `IngestRun` they produce — never the
// events this surface accumulated — is what the completion card says.

// Unmounted before the Tauri internals go: a surface's `listen` handle
// unregisters itself on destroy, and `clearMocks` takes the registry away.
afterEach(() => {
  cleanup();
  clearMocks();
});

const doneState: IngestState = {
  busy: false,
  finished: { status: "done", run: ingestRun },
};

/** `run_stopped` first, then a state read that is still busy, then the
 *  finished one — the sweep, the MHL generation and the catalog events all
 *  land after the copy loop ends. */
function settlingState(finished: IngestState): CommandHandler {
  let reads = 0;
  return () => {
    reads += 1;
    // The mount read, before anything has been started.
    if (reads === 1) return { busy: false };
    // The first poll after `run_stopped`: the copy loop has ended but the
    // sweep, the MHL generations and the catalog events have not landed.
    if (reads === 2) return { busy: true, running: RUN };
    return finished;
  };
}

test("run_stopped keeps the progress up until ingest_state says the run is done", async () => {
  await startRun({ ingest_state: settlingState(doneState) });
  await runStarted(2, 1000);
  const panel = await runPanel();

  await emitProgress(RUN, { type: "file_started", rel: "a.mov", size: 400 });
  await emitProgress(RUN, { type: "file_placed", rel: "a.mov" });
  await emitProgress(RUN, { type: "file_started", rel: "b.mov", size: 600 });
  await emitProgress(RUN, { type: "file_placed", rel: "b.mov" });
  await waitFor(() => expect(panel.textContent).toContain("2 / 2 files"));

  await emitProgress(RUN, { type: "run_stopped", cancelled: false });

  // Still on screen, and still saying what the events said, while the
  // outcome is being assembled — but no longer titled "Copying", which is
  // the one thing the run is provably no longer doing.
  await waitFor(() => expect(panel.textContent).toContain("waiting for the sweep"));
  expect(panel.textContent).toContain("2 / 2 files");
  expect(
    within(panel).getByRole("heading", { name: "Finishing…" }),
  ).toBeTruthy();

  // The outcome is the authority: the end-of-run sweep demoted one of the
  // two files the events already announced as placed, and only the card
  // knows it.
  const card = await screen.findByRole("group", { name: "Completed run" });
  expect(card.textContent).toContain("1 placed");
  expect(card.textContent).toContain("1 failed");
  expect(screen.queryByRole("group", { name: "Run" })).toBeNull();
});

test("the completion card names the MHL generation, the failures and the rejects", async () => {
  await startRun({ ingest_state: settlingState(doneState) });
  await runStarted(1, 1000);
  await emitProgress(RUN, { type: "run_stopped", cancelled: true });

  const card = await screen.findByRole("group", { name: "Completed run" });
  expect(card.textContent).toContain("MHL generation written per destination");
  expect(
    within(card).getByRole("list", { name: "MHL generations" }).textContent,
  ).toContain("generation 1");
  expect(
    within(card).getByRole("list", { name: "Failed files" }).textContent,
  ).toContain("DCIM/d.mov");
  expect(
    within(card).getByRole("list", { name: "Rejected by the run" }).textContent,
  ).toContain("unreadable: permission denied");
  expect(
    within(card).getByRole("list", { name: "Diagnostics" }).textContent,
  ).toContain("queue lock poisoned");
  expect(screen.getByText("a warning the ingest run collected")).toBeTruthy();
});

test("a state read that fails while the run is finishing keeps the run's id on screen", async () => {
  const message = "the ingest job state is not managed";
  let reads = 0;
  await startRun({
    ingest_state: () => {
      reads += 1;
      // The mount read, then the poll `run_stopped` set going.
      return reads === 1
        ? { busy: false }
        : rejectCommand(message, ["one run journal could not be read"]);
    },
  });
  await runStarted(1, 1000);
  await emitProgress(RUN, { type: "run_stopped", cancelled: false });

  // Without this the run panel would simply vanish: no card, no error, and
  // an operator with no idea what became of the copy.
  const alert = await screen.findByRole("alert");
  expect(alert.textContent).toBe(message);
  expect(screen.getByText("one run journal could not be read")).toBeTruthy();
  const block = screen.getByRole("group", { name: "Run state" });
  expect(block.textContent).toContain(RUN);
  expect(screen.queryByRole("group", { name: "Completed run" })).toBeNull();
});

test("a run whose slot frees with no outcome in it says so rather than vanishing", async () => {
  let reads = 0;
  await startRun({
    ingest_state: () => {
      reads += 1;
      // The mount read, then the poll — free both times, and with nothing
      // to hand over the second.
      return { busy: false };
    },
  });
  await runStarted(1, 1000);
  await emitProgress(RUN, { type: "run_stopped", cancelled: false });

  const alert = await screen.findByRole("alert");
  expect(alert.textContent).toContain(RUN);
  expect(alert.textContent).toContain("no outcome for it");
  expect(screen.queryByRole("group", { name: "Completed run" })).toBeNull();
  expect(reads).toBe(2);
});

test("Re-copy failed plans the same source again, and keeps its place while it does", async () => {
  const gate = deferred<IngestPlanOutcome>();
  let plans = 0;
  const calls = await startRun({
    ingest_state: settlingState(doneState),
    plan_ingest: () => {
      plans += 1;
      return plans === 1 ? planOutcome : gate.promise;
    },
  });
  await runStarted(1, 1000);
  await emitProgress(RUN, { type: "run_stopped", cancelled: false });

  await screen.findByRole("group", { name: "Completed run" });
  await userEvent.click(screen.getByRole("button", { name: "Re-copy failed…" }));

  // Re-walking a real card takes seconds to minutes. The button is
  // disabled for that stretch, not replaced by a message — a control that
  // deletes itself mid-click takes the focus with it.
  const button = await screen.findByRole<HTMLButtonElement>("button", {
    name: "Planning…",
  });
  expect(button.disabled).toBe(true);
  gate.settle(planOutcome);

  await waitFor(() => expect(callsTo(calls, "plan_ingest")).toHaveLength(2));
  // The same job: the files that did land are duplicates now and skip
  // themselves, so what is left to copy is exactly the failures.
  expect(callsTo(calls, "plan_ingest")[1]?.args).toEqual(
    callsTo(calls, "plan_ingest")[0]?.args,
  );
  expect(
    await screen.findByRole("button", { name: "Re-copy failed…" }),
  ).toBeTruthy();
});

test("a second run starts from zero rather than from the last run's tallies", async () => {
  const calls = await startRun({
    ingest_state: settlingState(doneState),
    start_ingest: () => RUN,
  });
  await runStarted(2, 1000);
  await emitProgress(RUN, { type: "file_started", rel: "a.mov", size: 400 });
  await emitProgress(RUN, { type: "file_placed", rel: "a.mov" });
  await emitProgress(RUN, { type: "run_stopped", cancelled: false });
  await screen.findByRole("group", { name: "Completed run" });

  await userEvent.click(screen.getByRole("button", { name: "Plan again" }));
  await waitFor(() => expect(callsTo(calls, "plan_ingest")).toHaveLength(2));
  await userEvent.click(
    screen.getByRole("button", { name: "Start verified copy" }),
  );
  await runStarted(2, 1000);

  const panel = await runPanel();
  await waitFor(() => expect(panel.textContent).toContain("0 / 2 files"));
  expect(screen.getByRole("progressbar").getAttribute("aria-valuenow")).toBe("0");
});

test("mounting after a run has finished draws that run's card", async () => {
  mockIngest({ ingest_state: () => doneState });
  renderIngest();

  const card = await screen.findByRole("group", { name: "Completed run" });
  expect(card.textContent).toContain(RUN);
  expect(card.textContent).toContain("1 placed");
  expect(card.textContent).toContain("1 failed");
  expect(card.textContent).toContain("generation 1");
  expect(
    screen.getByRole("list", { name: "Failed files" }).textContent,
  ).toContain("/Volumes/SSD-A: read-back mismatch");
  expect(screen.getByText("a warning the ingest run collected")).toBeTruthy();
});

test("a failed run is a card of its own, error and notices intact", async () => {
  mockIngest({
    ingest_state: () => ({
      busy: false,
      finished: {
        status: "failed",
        error: {
          message: "no PARA node matches project/gone",
          notices: ["one run journal could not be read"],
        },
      },
    }),
  });
  renderIngest();

  const alert = await screen.findByRole("alert");
  expect(alert.textContent).toBe("no PARA node matches project/gone");
  expect(screen.getByText("one run journal could not be read")).toBeTruthy();
});

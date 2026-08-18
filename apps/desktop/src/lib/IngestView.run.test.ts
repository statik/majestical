import { clearMocks } from "@tauri-apps/api/mocks";
import { cleanup, screen, waitFor, within } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, test } from "vitest";
import {
  callsTo,
  DEST_A,
  DEST_B,
  emitProgress,
  OTHER_RUN,
  RUN,
  runPanel,
  runStarted,
  startRun,
  unmountIngest,
} from "./ingest-test-support";
import { deferred, settlingTime } from "./test-support";

// One run while it is going: the preparing window, the progress stream, the
// elapsed clock, and cancellation. How a run ENDS — the settle poll and
// everything the completion card is drawn from — is the outcome suite next
// door, because the authority is different there: events here, the run's own
// `Outcome` there.

// Unmounted before the Tauri internals go: a surface's `listen` handle
// unregisters itself on destroy, and `clearMocks` takes the registry away.
afterEach(() => {
  cleanup();
  clearMocks();
});

test("Start opens a preparing window that lasts until the run says it started", async () => {
  const gate = deferred<string>();
  await startRun({ start_ingest: () => gate.promise });

  // The run thread re-plans before it names itself: on a real card that is
  // seconds of hashing with no events at all, and the surface says so
  // rather than drawing an empty progress bar.
  const panel = await runPanel();
  expect(panel.textContent).toContain("Preparing…");
  expect(panel.textContent).toContain("naming the run");
  expect(screen.queryByRole("progressbar")).toBeNull();

  gate.settle(RUN);

  await waitFor(() =>
    expect(panel.textContent).toContain(`run ${RUN} — resumable`),
  );
  // Named, but still preparing: `start_ingest` resolves before the first
  // byte, so nothing is claimed about progress yet.
  expect(panel.textContent).toContain("Preparing…");

  await runStarted(2, 1000);

  await waitFor(() => expect(panel.textContent).toContain("Copying"));
  expect(panel.textContent).not.toContain("Preparing…");
});

test("a first event that beats start_ingest's answer names the run all the same", async () => {
  const gate = deferred<string>();
  await startRun({ start_ingest: () => gate.promise });

  await runPanel();
  await runStarted(2, 1000);

  const bar = await screen.findByRole("progressbar");
  expect(bar.getAttribute("aria-valuenow")).toBe("0");
  gate.settle(RUN);

  // The events keep applying to the same run once the promise catches up.
  await emitProgress(RUN, { type: "file_started", rel: "a.mov", size: 400 });
  await emitProgress(RUN, { type: "bytes_copied", rel: "a.mov", bytes_done: 200 });

  await waitFor(() => expect(bar.getAttribute("aria-valuenow")).toBe("20"));
});

test("the bar and the counters are the bytes and files the run measured", async () => {
  await startRun();
  await runStarted(2, 1000);

  const panel = await runPanel();
  const bar = await screen.findByRole("progressbar");
  await emitProgress(RUN, { type: "file_started", rel: "a.mov", size: 400 });
  await emitProgress(RUN, { type: "bytes_copied", rel: "a.mov", bytes_done: 200 });

  await waitFor(() => expect(bar.getAttribute("aria-valuenow")).toBe("20"));
  const now = screen.getByRole("list", { name: "Files in flight" });
  expect(now.textContent).toContain("a.mov");
  expect(within(now).getByText("200 B of 400 B · 50%")).toBeTruthy();

  // A placed file has been read end to end, whatever its last chunk said.
  await emitProgress(RUN, { type: "file_placed", rel: "a.mov" });

  await waitFor(() => expect(bar.getAttribute("aria-valuenow")).toBe("40"));
  expect(panel.textContent).toContain("1 / 2 files");
  expect(within(now).queryByText(/a\.mov/u)).toBeNull();
});

test("measured bytes past the plan's total clamp the bar rather than overflow it", async () => {
  await startRun();
  await runStarted(1, 1000);
  const bar = await screen.findByRole("progressbar");

  // `bytes_total` is a plan-time sum and every `bytes_copied` is measured at
  // copy time, so a source that grew between the two legitimately overshoots.
  await emitProgress(RUN, { type: "file_started", rel: "a.mov", size: 1500 });
  await emitProgress(RUN, { type: "bytes_copied", rel: "a.mov", bytes_done: 1500 });

  await waitFor(() => expect(bar.getAttribute("aria-valuenow")).toBe("100"));
});

test("every destination keeps its own tally, and a failure reddens without stopping the run", async () => {
  await startRun();
  await runStarted(2, 1000);
  const panel = await runPanel();

  await emitProgress(RUN, { type: "file_started", rel: "a.mov", size: 400 });
  await emitProgress(RUN, { type: "file_verified", rel: "a.mov", dest_root: DEST_A });
  await emitProgress(RUN, { type: "file_verified", rel: "a.mov", dest_root: DEST_B });
  await emitProgress(RUN, { type: "file_placed", rel: "a.mov" });
  await emitProgress(RUN, { type: "file_started", rel: "b.mov", size: 600 });
  await emitProgress(RUN, { type: "file_verified", rel: "b.mov", dest_root: DEST_A });
  await emitProgress(RUN, {
    type: "file_failed",
    rel: "b.mov",
    reason: "/Volumes/NAS-1: read-back mismatch",
  });

  const tallies = await screen.findByRole("list", { name: "Destination tallies" });
  await waitFor(() =>
    expect(within(tallies).getByText(`${DEST_A}`).parentElement?.textContent).toContain(
      "2 verified",
    ),
  );
  expect(
    within(tallies).getByText(`${DEST_B}`).parentElement?.textContent,
  ).toContain("1 verified");

  // Per-item failures are rows; the run keeps going and the counter reddens.
  expect(panel.textContent).toContain("2 / 2 files");
  expect(panel.textContent).toContain("1 failed");
  expect(panel.querySelector(".ingest-bad")).not.toBeNull();
  expect(
    screen.getByRole("list", { name: "Failures so far" }).textContent,
  ).toContain("/Volumes/NAS-1: read-back mismatch");
  expect(screen.queryByRole("button", { name: "Start verified copy" })).toBeNull();
});

test("an event stamped with another run is not this run's to count", async () => {
  await startRun();
  await runStarted(2, 1000);
  const panel = await runPanel();

  await emitProgress(RUN, { type: "file_started", rel: "a.mov", size: 400 });
  await emitProgress(RUN, { type: "file_placed", rel: "a.mov" });
  await waitFor(() => expect(panel.textContent).toContain("1 / 2 files"));

  await emitProgress(OTHER_RUN, { type: "file_started", rel: "z.mov", size: 400 });
  await emitProgress(OTHER_RUN, { type: "file_placed", rel: "z.mov" });
  await emitProgress(OTHER_RUN, {
    type: "run_started",
    files_total: 99,
    bytes_total: 99_000,
  });

  expect(panel.textContent).toContain("1 / 2 files");
  expect(panel.textContent).not.toContain("99");
  expect(screen.getByRole("list", { name: "Files in flight" }).textContent).not.toContain(
    "z.mov",
  );
});

test("the counters carry an elapsed clock, and an estimate only once there is a rate", async () => {
  let ms = 1000;
  await startRun({}, { clock: () => ms });

  ms = 10_000;
  await runStarted(2, 1000);
  const panel = await runPanel();

  ms = 20_000;
  await emitProgress(RUN, { type: "file_started", rel: "a.mov", size: 400 });

  // Ten seconds in and not a byte reported: there is no rate to estimate
  // from, so the estimate is absent rather than invented.
  await waitFor(() => expect(panel.textContent).toContain("elapsed 00:10"));
  expect(panel.textContent).not.toContain("left");

  await emitProgress(RUN, { type: "bytes_copied", rel: "a.mov", bytes_done: 250 });

  // 250 of 1000 bytes in ten seconds: 750 to go at that rate is thirty.
  await waitFor(() =>
    expect(panel.textContent).toContain("elapsed 00:10 · about 00:30 left"),
  );
});

test("leaving the surface does not cancel the run", async () => {
  const calls = await startRun();
  await runStarted(2, 1000);
  await runPanel();

  unmountIngest();
  await settlingTime();

  // The run is the backend's and outlives this window; the sidebar's
  // marker is what keeps it visible after the surface is gone.
  expect(callsTo(calls, "cancel_ingest")).toHaveLength(0);
});

test("Stop asks the run to end after the file it is on", async () => {
  const calls = await startRun();
  await runStarted(2, 1000);

  const stop = await screen.findByRole("button", {
    name: "Stop after current file",
  });
  await userEvent.click(stop);

  await waitFor(() => expect(callsTo(calls, "cancel_ingest")).toHaveLength(1));
  expect(
    await screen.findByRole("button", {
      name: "Stopping after the current file…",
    }),
  ).toBeTruthy();
});

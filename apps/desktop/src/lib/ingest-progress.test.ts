import { expect, test } from "vitest";
import type { ProgressEvent } from "./api";
import {
  applyProgress,
  barPercent,
  bytesDone,
  destRoots,
  filePercent,
  noProgress,
  remainingMs,
} from "./ingest-progress";

/** The state after one run of events, in order. */
function after(events: ProgressEvent[]) {
  return events.reduce((state, event) => applyProgress(state, event), noProgress());
}

test("nothing has been heard about a run that has not started", () => {
  const state = noProgress();

  expect(state.totalsKnown).toBe(false);
  expect(bytesDone(state)).toBe(0);
  // Not "0 of 0 is 100%": a bar with no denominator is at its start.
  expect(barPercent(state)).toBe(0);
});

test("run_started is the only place the totals come from", () => {
  const state = after([{ type: "run_started", files_total: 3, bytes_total: 900 }]);

  expect(state.totalsKnown).toBe(true);
  expect(state.filesTotal).toBe(3);
  expect(state.bytesTotal).toBe(900);
});

test("a file in flight contributes the bytes its last chunk reported", () => {
  const state = after([
    { type: "run_started", files_total: 2, bytes_total: 1000 },
    { type: "file_started", rel: "a.mov", size: 400 },
    { type: "bytes_copied", rel: "a.mov", bytes_done: 100 },
    { type: "bytes_copied", rel: "a.mov", bytes_done: 250 },
  ]);

  // Cumulative per file, not additive: the second chunk replaces the first.
  expect(bytesDone(state)).toBe(250);
  expect(barPercent(state)).toBe(25);
  expect(state.copying).toEqual([{ rel: "a.mov", size: 400, done: 250 }]);
});

test("a placed file settles at its whole size and leaves the in-flight rows", () => {
  const state = after([
    { type: "run_started", files_total: 2, bytes_total: 1000 },
    { type: "file_started", rel: "a.mov", size: 400 },
    { type: "bytes_copied", rel: "a.mov", bytes_done: 250 },
    { type: "file_placed", rel: "a.mov" },
  ]);

  // The last `bytes_copied` before the finish can be a whole buffer short;
  // a file that was placed was read end to end all the same.
  expect(bytesDone(state)).toBe(400);
  expect(state.copying).toEqual([]);
  expect(state.placed).toBe(1);
});

test("a failed file keeps only the bytes it managed, and its reason", () => {
  const state = after([
    { type: "run_started", files_total: 2, bytes_total: 1000 },
    { type: "file_started", rel: "b.mov", size: 600 },
    { type: "bytes_copied", rel: "b.mov", bytes_done: 120 },
    { type: "file_failed", rel: "b.mov", reason: "source read error" },
  ]);

  expect(bytesDone(state)).toBe(120);
  expect(state.failed).toBe(1);
  expect(state.failures).toEqual([
    { rel: "b.mov", reason: "source read error" },
  ]);
  expect(state.copying).toEqual([]);
});

test("bytes measured past the plan's total clamp the bar", () => {
  const state = after([
    { type: "run_started", files_total: 1, bytes_total: 1000 },
    { type: "file_started", rel: "a.mov", size: 1500 },
    { type: "bytes_copied", rel: "a.mov", bytes_done: 1500 },
  ]);

  expect(bytesDone(state)).toBe(1500);
  expect(barPercent(state)).toBe(100);
});

test("every destination is tallied by the files it verified", () => {
  const state = after([
    { type: "file_verified", rel: "a.mov", dest_root: "/ssd" },
    { type: "file_verified", rel: "a.mov", dest_root: "/nas" },
    { type: "file_verified", rel: "b.mov", dest_root: "/ssd" },
  ]);

  expect(state.verified).toEqual({ "/ssd": 2, "/nas": 1 });
  // Chosen roots first, then roots only the events know about — which is
  // all a surface that joined a run mid-flight has.
  expect(destRoots(state, ["/ssd"])).toEqual(["/ssd", "/nas"]);
  expect(destRoots(state, [])).toEqual(["/ssd", "/nas"]);
});

test("a file nobody saw start contributes no bytes rather than a guess", () => {
  const state = after([
    { type: "run_started", files_total: 1, bytes_total: 1000 },
    { type: "file_placed", rel: "joined-late.mov" },
  ]);

  expect(bytesDone(state)).toBe(0);
  expect(state.placed).toBe(1);
});

test("run_stopped changes no number: it is a phase, not a tally", () => {
  const events: ProgressEvent[] = [
    { type: "run_started", files_total: 1, bytes_total: 1000 },
    { type: "file_started", rel: "a.mov", size: 400 },
  ];

  expect(after([...events, { type: "run_stopped", cancelled: true }])).toEqual(
    after(events),
  );
});

test("a file in flight reports how far through itself it is", () => {
  expect(filePercent({ rel: "a.mov", size: 400, done: 200 })).toBe(50);
  // A file whose `file_started` announced no size has no fraction to be
  // at, and dividing by it would be worse than saying nothing.
  expect(filePercent({ rel: "a.mov", size: 0, done: 0 })).toBe(0);
  expect(filePercent({ rel: "a.mov", size: 400, done: 900 })).toBe(100);
});

test("the estimate is the rate so far applied to what is left", () => {
  const state = after([
    { type: "run_started", files_total: 2, bytes_total: 1000 },
    { type: "file_started", rel: "a.mov", size: 400 },
    { type: "bytes_copied", rel: "a.mov", bytes_done: 250 },
  ]);

  // A quarter in ten seconds: thirty seconds for the other three quarters.
  expect(remainingMs(state, 10_000)).toBe(30_000);
});

test("there is no estimate without a total, a rate, or time to measure one", () => {
  const started = after([
    { type: "run_started", files_total: 1, bytes_total: 1000 },
  ]);

  expect(remainingMs(started, null)).toBeNull();
  expect(remainingMs(started, 0)).toBeNull();
  // Elapsed time but nothing copied yet: no rate to project.
  expect(remainingMs(started, 10_000)).toBeNull();
  // A run this surface joined mid-flight knows no total at all.
  expect(remainingMs(noProgress(), 10_000)).toBeNull();
  // And a run already past its plan-time total has nothing left to wait for.
  const over = after([
    { type: "run_started", files_total: 1, bytes_total: 1000 },
    { type: "file_started", rel: "a.mov", size: 1500 },
    { type: "bytes_copied", rel: "a.mov", bytes_done: 1500 },
  ]);
  expect(remainingMs(over, 10_000)).toBeNull();
});

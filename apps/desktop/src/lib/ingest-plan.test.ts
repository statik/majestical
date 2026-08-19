import { expect, test } from "vitest";
import { planSummary } from "./ingest-plan";
import { planOutcome } from "./ingest-test-support";

test("a plan is counted once, into every number the board shows", () => {
  const summary = planSummary(planOutcome);

  // Every file the walk found and its bytes — what the source panel says
  // the card holds, which is not the same as what will be copied.
  expect(summary.files).toBe(3);
  expect(summary.bytes).toBe(1024 + 2048 + 4096);
  expect(summary.toCopy).toBe(1);
  expect(summary.copyBytes).toBe(1024);
  expect(summary.duplicates).toBe(1);
  // Rejects are rows carrying the planner's own reason, not a tally.
  expect(summary.rejects).toEqual([
    { rel: "DCIM/c.mov", reason: "unreadable: permission denied" },
  ]);
});

test("no plan summarizes to zeroes rather than to a shape of its own", () => {
  expect(planSummary(null)).toEqual({
    files: 0,
    bytes: 0,
    toCopy: 0,
    copyBytes: 0,
    duplicates: 0,
    rejects: [],
  });
});

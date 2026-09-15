import { expect, test } from "vitest";
import schedulerState from "./fixtures/scheduler_state.json";
import schedulerStateHeld from "./fixtures/scheduler_state_held.json";
import type { SchedulerStateOutcome } from "./api-alwayson";
import { statusLine } from "./scheduler-status";

const auto = schedulerState as SchedulerStateOutcome;
const held = schedulerStateHeld as SchedulerStateOutcome;

const cases: { label: string; state: SchedulerStateOutcome; expected: string }[] = [
  {
    label: "run_full with a plural pending count",
    state: { ...auto, decision: { mode: "run_full" }, pending_items: 42 },
    expected: "Indexing — 42 items pending",
  },
  {
    label: "run_full with a singular pending count",
    state: { ...auto, decision: { mode: "run_full" }, pending_items: 1 },
    expected: "Indexing — 1 item pending",
  },
  {
    label: "run_low",
    state: { ...auto, decision: { mode: "run_low" }, pending_items: 3 },
    expected: "Indexing slowly — 3 items pending",
  },
  {
    label: "hold/paused",
    state: { ...held, decision: { mode: "hold", hold_reason: "paused" } },
    expected: "Paused",
  },
  {
    label: "hold/low_power_mode",
    state: {
      ...auto,
      decision: { mode: "hold", hold_reason: "low_power_mode" },
    },
    expected: "Paused (Low Power Mode)",
  },
  {
    label: "hold/no_pending_work",
    state: {
      ...auto,
      decision: { mode: "hold", hold_reason: "no_pending_work" },
      pending_items: 0,
    },
    expected: "Idle",
  },
  {
    label: "no decision yet",
    state: { ...auto, decision: null },
    expected: "Starting…",
  },
];

test.each(cases)("statusLine reads $expected for $label", ({ state, expected }) => {
  expect(statusLine(state)).toBe(expected);
});

import { describe, expect, it } from "vitest";
import type {
  CheckStatus,
  DoctorOutcome,
  SchedulerDecision,
  SchedulerStateOutcome,
} from "./api-alwayson";
import doctorOutcome from "./fixtures/doctor_outcome.json";
import schedulerState from "./fixtures/scheduler_state.json";
import schedulerStateHeld from "./fixtures/scheduler_state_held.json";

// `DoctorCheck.status` is a string-literal union (`CheckStatus`); JSON
// module inference widens it to `string`, same reason `AssetDetail` in
// `fixtures.test.ts` needs a cast rather than a plain assignment.
const typedDoctorOutcome: DoctorOutcome = doctorOutcome as DoctorOutcome;
// The annotation is the check: a literal the `CheckStatus` union drops fails
// `tsc` here, which the cast above would otherwise hide.
const allCheckStatuses: CheckStatus[] = ["ok", "warn", "fail"];
// `SchedulerStateOutcome.decision` is a tagged union (`SchedulerDecision`)
// whose `mode` discriminant JSON module inference widens to `string`, same
// reason as `AssetDetail` in `fixtures.test.ts` — a cast, plus the literal
// array below pinning the three modes and the `hold` arm's `hold_reason`
// values.
const typedSchedulerState: SchedulerStateOutcome =
  schedulerState as SchedulerStateOutcome;
const typedSchedulerStateHeld: SchedulerStateOutcome =
  schedulerStateHeld as SchedulerStateOutcome;
const allSchedulerModes: SchedulerDecision["mode"][] = [
  "run_full",
  "run_low",
  "hold",
];

describe("scheduler state fixtures", () => {
  it("carry the running and held shapes the scheduler card renders", () => {
    expect(typedSchedulerState.available).toBe(true);
    expect(typedSchedulerState.running).toBe(true);
    expect(typedSchedulerState.decision?.mode).toBe("run_full");
    expect(typedSchedulerState.last_error).toBeUndefined();
    expect(allSchedulerModes).toContain(typedSchedulerState.decision?.mode);

    expect(typedSchedulerStateHeld.running).toBe(false);
    expect(typedSchedulerStateHeld.decision?.mode).toBe("hold");
    if (typedSchedulerStateHeld.decision?.mode !== "hold") {
      throw new Error("the held fixture must carry a hold_reason");
    }
    expect(typedSchedulerStateHeld.decision.hold_reason).toBe("paused");
    expect(typedSchedulerStateHeld.last_error?.length).toBeGreaterThan(0);
  });
});

describe("doctor outcome fixture", () => {
  it("carries one row per status, a remedy on warn and fail, and a notice", () => {
    const statuses = typedDoctorOutcome.checks.map((c) => c.status);
    expect(statuses).toEqual(allCheckStatuses);
    const [ok, warn, fail] = typedDoctorOutcome.checks;
    expect(ok?.remedy).toBeUndefined();
    expect(warn?.remedy?.length).toBeGreaterThan(0);
    expect(fail?.remedy?.length).toBeGreaterThan(0);
    expect(typedDoctorOutcome.notices?.length).toBeGreaterThan(0);
  });
});

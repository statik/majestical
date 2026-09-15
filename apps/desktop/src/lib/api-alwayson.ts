// The always-on/health wire subject — `indexer.rs`'s scheduler commands
// and `commands::doctor_report` — split out of `api.ts` when that file
// reached its cap (see .oxlintrc.json). Same rules as api.ts: one
// interface per outcome struct, mirroring the Rust field-for-field, pinned
// by `fixtures.alwayson.test.ts` against `fixtures/*.json`.
import { invoke } from "@tauri-apps/api/core";

/** `majestical_services::doctor::CheckStatus`, serialized snake_case. */
export type CheckStatus = "ok" | "warn" | "fail";

/**
 * `majestical_services::doctor::DoctorCheck`. `remedy` is absent on an `Ok`
 * row and present on `Fail`; a `Warn` row may carry one (the no-catalog
 * `catalog` row does) or not (`blob_residue` and `platform` warnings don't).
 */
export interface DoctorCheck {
  name: string;
  status: CheckStatus;
  detail: string;
  remedy?: string;
}

/**
 * `majestical_services::doctor::DoctorOutcome` — what `doctorReport` returns.
 * Runs even before a catalog is chosen: the catalog-dependent checks report
 * `Warn` rows instead of the command failing.
 */
export interface DoctorOutcome {
  checks: DoctorCheck[];
  notices?: string[];
}

/**
 * The Tauri event `tray.rs` emits after showing and focusing the main
 * window from "Health…" or the tray's attention line ("Last batch failed"),
 * so the shell selects the Settings surface rather than wherever it was
 * left open to.
 */
export const NAVIGATE_SETTINGS_EVENT = "navigate-settings";

/** `majestical_services::autopilot::ThrottleOverride`, serialized snake_case. */
export type ThrottleOverride = "auto" | "paused" | "low" | "full";

/** `majestical_services::autopilot::PowerSource`, serialized snake_case. */
export type PowerSource = "ac" | "battery" | "unknown";

/** `majestical_services::autopilot::PowerState` */
export interface PowerState {
  source: PowerSource;
  low_power_mode: boolean;
}

/**
 * `majestical_services::autopilot::SchedulerDecision` — serde tag `mode`,
 * `hold_reason` only present on the `hold` arm, both snake_case.
 */
export type SchedulerDecision =
  | { mode: "run_full" }
  | { mode: "run_low" }
  | {
      mode: "hold";
      hold_reason: "paused" | "low_power_mode" | "no_pending_work";
    };

/** The `hold` arm's reason, pulled out so a `Record<HoldReason, ...>` (as
 *  `scheduler-status.ts` builds) fails to type-check on a variant it hasn't
 *  covered, rather than silently falling through. */
export type HoldReason = Extract<
  SchedulerDecision,
  { mode: "hold" }
>["hold_reason"];

/**
 * `indexer::SchedulerStateOutcome` — what `scheduler_state`/`set_throttle`
 * return. `decision` is `null`, not absent, before the scheduler loop's
 * first tick has run; `last_error` is absent both before that first tick
 * and once a later batch has succeeded.
 */
export interface SchedulerStateOutcome {
  available: boolean;
  throttle: ThrottleOverride;
  power: PowerState;
  decision: SchedulerDecision | null;
  pending_items: number;
  running: boolean;
  last_error?: string;
}

export const alwaysOnApi = {
  doctorReport: () => invoke<DoctorOutcome>("doctor_report"),
  schedulerState: () => invoke<SchedulerStateOutcome>("scheduler_state"),
  setThrottle: (throttle: ThrottleOverride) =>
    invoke<SchedulerStateOutcome>("set_throttle", { throttle }),
};

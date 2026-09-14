// The Always-on section's one-line scheduler status, pulled out of
// `AlwaysOnSection.svelte` (the `format.ts`/`ingest-progress.ts` pattern)
// so the compiler enforces exhaustiveness rather than a plain `switch`
// silently falling through on an uncovered case: `statusLine`'s explicit
// `string` return type fails to compile (TS2366) if a `SchedulerDecision`
// mode is left unhandled, and `HOLD_LINES` being a `Record<HoldReason,
// string>` fails to compile if a hold reason is left out of it.
//
// Deliberately NOT a port of `tray.rs`'s `menu_model`: this omits the
// power-source-aware second line `run_low_lines` adds under Auto (it would
// mean re-deriving `PowerSource` branching here) and the "only show the
// pending count when nonzero" refinement `menu_model` applies to a Low
// Power Mode hold. Both are tray-only polish, not information this line
// claims to give.
import type { HoldReason, SchedulerStateOutcome } from "./api";

const HOLD_LINES: Record<HoldReason, string> = {
  paused: "Paused",
  low_power_mode: "Paused (Low Power Mode)",
  no_pending_work: "Idle",
};

function pendingLine(pendingItems: number): string {
  return pendingItems === 1
    ? "1 item pending"
    : `${pendingItems} items pending`;
}

export function statusLine(state: SchedulerStateOutcome): string {
  if (state.decision === null) return "Starting…";
  const { decision } = state;
  switch (decision.mode) {
    case "hold":
      return HOLD_LINES[decision.hold_reason];
    case "run_full":
      return `Indexing — ${pendingLine(state.pending_items)}`;
    case "run_low":
      return `Indexing slowly — ${pendingLine(state.pending_items)}`;
  }
}

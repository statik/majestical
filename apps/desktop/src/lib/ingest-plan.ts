// What a plan says, counted once. `plan_ingest` answers with the whole
// decided file list, and the surface draws four numbers and one row list off
// it — partitioning that list at four separate points in the markup is four
// places for the counters to disagree with each other.
import type { FailedFile, IngestPlanOutcome } from "./api";

export interface PlanSummary {
  /** Every file the walk found, whatever was decided about it. */
  files: number;
  /** And their bytes — the size of the source, not of the copy. */
  bytes: number;
  toCopy: number;
  copyBytes: number;
  duplicates: number;
  /**
   * The files the planner would not take, with the reason each was
   * refused. Rows rather than a count, because a reject is something the
   * operator may want to go and fix — and rows, not errors: a plan that
   * rejected two files is a plan, not a failure.
   */
  rejects: FailedFile[];
}

/** The summary of a plan, or of no plan at all — all zeroes, so a surface
 *  with nothing planned yet needs no separate shape to render. */
export function planSummary(plan: IngestPlanOutcome | null): PlanSummary {
  const summary: PlanSummary = {
    files: 0,
    bytes: 0,
    toCopy: 0,
    copyBytes: 0,
    duplicates: 0,
    rejects: [],
  };
  for (const file of plan?.plan.files ?? []) {
    summary.files += 1;
    summary.bytes += file.size;
    switch (file.decision.decision) {
      case "copy":
        summary.toCopy += 1;
        summary.copyBytes += file.size;
        break;
      case "duplicate":
        summary.duplicates += 1;
        break;
      case "rejected":
        summary.rejects.push({ rel: file.rel, reason: file.decision.reason });
        break;
    }
  }
  return summary;
}

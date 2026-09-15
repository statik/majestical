// The fixture identities and outcomes every Ingest suite renders against:
// one catalog, one plan, one finished run. Split out of
// `ingest-test-support.ts` when that file next pressed its own line cap —
// a pure move, with the id and path constants each fixture is built from
// moved alongside it so this module has nothing to reach back for.
import type { IngestPlanOutcome, IngestRun, ParaOutcome } from "./api";

export const RUN = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
export const OTHER_RUN = "01BXQ8W2M4N6P8R0T2V4X6Z8AC";
export const SOURCE = "/Volumes/A7IV-CARD";
export const DEST_A = "/Volumes/SSD-A";
export const DEST_B = "/Volumes/NAS-1";
export const NODE = "01PROJECT";

/** One active node to file into, and an archived one that must not be
 *  offered — filing into an archive files into somewhere nobody looks. */
export const paraOutcome: ParaOutcome = {
  nodes: [
    { id: NODE, kind: "project", name: "client-x", archived: false },
    { id: "01ARCHIVED", kind: "archive", name: "talon-2024", archived: true },
  ],
};

/** One of each decision the planner can reach, so every counter on the plan
 *  panel has something to count. */
export const planOutcome: IngestPlanOutcome = {
  plan: {
    files: [
      {
        source: `${SOURCE}/DCIM/a.mov`,
        rel: "DCIM/a.mov",
        size: 1024,
        prehash: "0123456789abcdef0123456789abcdef",
        decision: { decision: "copy" },
      },
      {
        source: `${SOURCE}/DCIM/b.mov`,
        rel: "DCIM/b.mov",
        size: 2048,
        prehash: "89abcdef0123456789abcdef01234567",
        decision: {
          decision: "duplicate",
          asset: "xxh3:89abcdef0123456789abcdef01234567",
          action: "skip",
        },
      },
      {
        source: `${SOURCE}/DCIM/c.mov`,
        rel: "DCIM/c.mov",
        size: 4096,
        prehash: null,
        decision: { decision: "rejected", reason: "unreadable: permission denied" },
      },
    ],
  },
  subdir: "Projects/client-x/2026-08-12/A7IV-CARD",
  node_id: NODE,
  source_volume_id: "uuid:9E1F0C7A-0B4E-4C1D-9A2B-6D5E4F3C2B1A",
  source_volume_label: "A7IV-CARD",
  notices: ["a warning the plan_ingest call collected"],
};

/** A finished run: one file placed, one failed, one rejected, one MHL
 *  generation. The card is drawn from this and nothing else. */
export const ingestRun: IngestRun = {
  run_id: RUN,
  outcome: {
    placed: [
      {
        rel: "DCIM/a.mov",
        size: 1024,
        xxh3: "0123456789abcdef0123456789abcdef",
        xxh64: "0123456789abcdef",
        dest_rel: "Projects/client-x/2026-08-12/A7IV-CARD/DCIM/a.mov",
      },
    ],
    failed: [{ rel: "DCIM/d.mov", reason: "/Volumes/SSD-A: read-back mismatch" }],
    skipped_duplicates: ["DCIM/b.mov"],
    rejected: [{ rel: "DCIM/c.mov", reason: "unreadable: permission denied" }],
    skipped_resumed: 2,
    diagnostics: ["queue lock poisoned — continuing with recovered state"],
  },
  generations: [
    [
      DEST_A,
      {
        path: "/Volumes/SSD-A/ascmhl/0001_SSD-A_2026-08-12_101500.mhl",
        generation: 1,
        roothash: "c43MDX3ScQKZk8MRLZfXmqcbSjqQPmhpqFrLzCkFvNhBAd",
      },
    ],
  ],
  notices: ["a warning the ingest run collected"],
};

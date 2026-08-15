// The catalog the Organize suites organize, and the mock that records what
// they asked it for. Shared by `OrganizeView.test.ts` (the PARA column),
// `OrganizeView.archive.test.ts` (the dry-run modal) and
// `OrganizeView.tags.test.ts` (the tag manager) — one fixture, so the three
// halves cannot drift into describing three different catalogs.
import type { InvokeArgs } from "@tauri-apps/api/core";
import { render, screen } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import type {
  ArchiveOutcome,
  MountedRoot,
  ParaOutcome,
  TagsListOutcome,
} from "./api";
import OrganizeView from "./OrganizeView.svelte";
import type { CommandHandler } from "./test-support";
import { mockCommands } from "./test-support";

/** One node of each kind, and the archive-kind one is archived — the four
 *  headings the mockup draws, with an "archived" chip on the last. */
export const paraOutcome: ParaOutcome = {
  nodes: [
    { id: "01PROJECT", kind: "project", name: "client-x", archived: false },
    {
      id: "01PROJECT2",
      kind: "project",
      name: "spring-campaign",
      archived: false,
    },
    { id: "01AREA", kind: "area", name: "studio-ops", archived: false },
    {
      id: "01RESOURCE",
      kind: "resource",
      name: "stock-library",
      archived: false,
    },
    { id: "01ARCHIVED", kind: "archive", name: "talon-2024", archived: true },
  ],
};

/** Five tags, two of them the same word spelled two ways. */
export const tagsOutcome: TagsListOutcome = {
  tags: [
    { tag: "b-roll", count: 412, last_used_ms: 1_754_000_000_000 },
    { tag: "golden-hour", count: 67, last_used_ms: 1_754_400_000_000 },
    { tag: "goldenhour", count: 9, last_used_ms: 1_750_000_000_000 },
    { tag: "interview", count: 140, last_used_ms: 1_753_000_000_000 },
    { tag: "drone", count: 203, last_used_ms: 1_752_000_000_000 },
  ],
};

/** Two mounted volumes: an external drive and the boot volume. */
export const mountedRoots: MountedRoot[] = [
  { volume: "uuid:SSD-A-UUID", label: "SSD-A", path: "/Volumes/SSD-A" },
  { volume: "label:root", label: "root", path: "/" },
];

/** What a dry run against those roots plans: one directory to move. */
export const archivePreview: ArchiveOutcome = {
  moves: [
    {
      from: "/Volumes/SSD-A/Projects/client-x",
      to: "/Volumes/SSD-A/Archives/client-x",
      status: "planned",
    },
  ],
  executed: false,
};

/** One recorded invoke: which command, and the arguments it was handed. */
export interface OrganizeCall {
  cmd: string;
  args: InvokeArgs | undefined;
}

/** Whether this `archive_node` call is the preview or the real thing.
 *  `InvokeArgs` is a union (a record, or raw bytes), so the record case is
 *  narrowed here once rather than at every handler that has to answer
 *  differently for the two calls. */
export function isDryRun(args: InvokeArgs | undefined): boolean {
  const record = (args ?? {}) as Record<string, unknown>;
  return record["dryRun"] === true;
}

/**
 * Answers every command the surface can reach, recording each call in
 * order. `overrides` replaces one answer — a different outcome, a counter
 * that answers differently per call, or a `rejectCommand`.
 */
export function mockOrganize(
  overrides: Record<string, CommandHandler> = {},
): OrganizeCall[] {
  const calls: OrganizeCall[] = [];
  const answers: Record<string, CommandHandler> = {
    list_para: () => paraOutcome,
    list_tags: () => tagsOutcome,
    list_mounted_roots: () => mountedRoots,
    add_para_node: () => "01NEWNODE",
    // `rename_para_node` returns Rust's `()`, which arrives as null.
    rename_para_node: () => null,
    archive_node: (args) => ({
      ...archivePreview,
      executed: !isDryRun(args),
    }),
    rename_tag: () => ({ from: "golden-hour", to: "golden", rewritten: 67 }),
    merge_tags: () => ({ from: "goldenhour", to: "golden-hour", rewritten: 9 }),
    ...overrides,
  };
  const handlers: Record<string, CommandHandler> = {};
  for (const [cmd, answer] of Object.entries(answers)) {
    handlers[cmd] = (args) => {
      calls.push({ cmd, args });
      return answer(args);
    };
  }
  mockCommands(handlers);
  return calls;
}

/** Every call to one command, in order. */
export function callsTo(calls: OrganizeCall[], cmd: string): OrganizeCall[] {
  return calls.filter((call) => call.cmd === cmd);
}

export function renderOrganize() {
  return render(OrganizeView);
}

/** Renders the surface, selects `client-x`, and opens the archive modal on
 *  it — where every archive test starts. */
export async function openArchive() {
  renderOrganize();
  await userEvent.click(
    await screen.findByRole("button", { name: /client-x/u }),
  );
  await userEvent.click(screen.getByRole("button", { name: "Archive…" }));
  return screen.findByRole("dialog");
}

// The card, the catalog and the run the Ingest suites share. Split out for
// the same reason `organize-test-support.ts` was: `IngestView.test.ts` (the
// setup board, the plan and the resume banner) and `IngestView.run.test.ts`
// (the progress stream and the completion card) must not drift into
// describing two different runs of two different cards. The fixtures
// themselves — `paraOutcome`, `planOutcome`, `ingestRun` and the ids/paths
// they are built from — live in `ingest-fixtures.ts`; re-exported here so
// nothing that already imports them from this file has to change.
import type { InvokeArgs } from "@tauri-apps/api/core";
import { emit } from "@tauri-apps/api/event";
import { render, screen, waitFor } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { expect } from "vitest";
import type { ProgressEvent } from "./api";
import { INGEST_PROGRESS_EVENT } from "./api";
import { DEST_A, DEST_B, NODE, paraOutcome, planOutcome, RUN, SOURCE } from "./ingest-fixtures";
import IngestView from "./IngestView.svelte";
import type { CommandHandler } from "./test-support";
import { mockCommands } from "./test-support";

export {
  DEST_A,
  DEST_B,
  ingestRun,
  NODE,
  OTHER_RUN,
  paraOutcome,
  planOutcome,
  RUN,
  SOURCE,
} from "./ingest-fixtures";

/** `open()` from `@tauri-apps/plugin-dialog` is a plain command invoke, so
 *  both folder pickers mock through the same channel as everything else. */
export const PICKER = "plugin:dialog|open";

/** One recorded invoke: which command, and the arguments it was handed. */
export interface IngestCall {
  cmd: string;
  args: InvokeArgs | undefined;
}

/**
 * Answers every command the surface can reach, recording each call in
 * order. `overrides` replaces one answer — a different outcome, a counter
 * that answers differently per call, or a `rejectCommand`.
 */
export function mockIngest(
  overrides: Record<string, CommandHandler> = {},
): IngestCall[] {
  const calls: IngestCall[] = [];
  const answers: Record<string, CommandHandler> = {
    list_para: () => paraOutcome,
    list_unfinished_ingests: () => ({ runs: [] }),
    ingest_state: () => ({ busy: false }),
    plan_ingest: () => planOutcome,
    start_ingest: () => RUN,
    // `cancel_ingest` returns Rust's `()`, which arrives as null.
    cancel_ingest: () => null,
    [PICKER]: () => SOURCE,
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
export function callsTo(calls: IngestCall[], cmd: string): IngestCall[] {
  return calls.filter((call) => call.cmd === cmd);
}

/** A folder picker that answers each click with the next path, and then
 *  cancels — a test that clicks once more than it planned for gets "the
 *  operator dismissed the dialog", not a silently repeated folder. */
export function picksInTurn(paths: string[]): CommandHandler {
  const queue = [...paths];
  return () => queue.shift() ?? null;
}

/** The Source field, which IS the source display now: a chosen source is
 *  read off its value rather than out of a line of its own. */
export function sourceField(): HTMLInputElement {
  return screen.getByRole<HTMLInputElement>("textbox", { name: "Source path" });
}

/** Types a source path the way an operator does. The field commits on
 *  `change`, which fires when it loses the focus — so the tab is part of
 *  typing a path, not decoration. */
export async function typeSource(path: string): Promise<void> {
  const field = await screen.findByRole("textbox", { name: "Source path" });
  await userEvent.clear(field);
  await userEvent.type(field, path);
  await userEvent.tab();
}

/** Types a destination and presses Enter, which is that field's own add. */
export async function typeDest(path: string): Promise<void> {
  const field = await screen.findByRole("textbox", {
    name: "Destination path",
  });
  await userEvent.type(field, `${path}{Enter}`);
}

/** The props a suite may pin; only the clock is ever passed. */
export interface IngestProps {
  clock?: () => number;
}

let mounted: ReturnType<typeof render<typeof IngestView>> | null = null;

export function renderIngest(props: IngestProps = {}) {
  const view = render(IngestView, props);
  mounted = view;
  return view;
}

/** Unmounts the surface the way leaving the sidebar entry does — which is
 *  not a cancellation, and the suites pin that it stays that way. */
export function unmountIngest(): void {
  mounted?.unmount();
  mounted = null;
}

/** The global `mockIPC` installs its callback registry on. */
const TAURI_INTERNALS = "__TAURI_INTERNALS__";

/** How many callbacks the Tauri mock has registered. A `listen` registers
 *  its handler synchronously inside the call, so this going above zero is
 *  the surface's subscription being live — which an emit has to wait for,
 *  since the subscription is made in a mount effect. Exported for
 *  `App.test.ts`'s own `listen` calls (the tray's `navigate-settings`
 *  event), which race the same mount effect this one does. */
export function listenerCount(): number {
  // Reached by name rather than as a property, because the name is the
  // Tauri API's and its underscores are not ours to rename.
  const internals = (
    window as unknown as Record<
      string,
      { callbacks?: Map<number, unknown> } | undefined
    >
  )[TAURI_INTERNALS];
  return internals?.callbacks?.size ?? 0;
}

/** Emits one progress event exactly as `start_ingest` forwards it: the
 *  event, in an envelope stamped with the run it belongs to. */
export async function emitProgress(
  runId: string,
  event: ProgressEvent,
): Promise<void> {
  await waitFor(() => expect(listenerCount()).toBeGreaterThan(0));
  await emit(INGEST_PROGRESS_EVENT, { run_id: runId, event });
}

/** The `run_started` that turns the preparing window into a running one. */
export function runStarted(files: number, bytes: number): Promise<void> {
  return emitProgress(RUN, {
    type: "run_started",
    files_total: files,
    bytes_total: bytes,
  });
}

/**
 * Plans a copy and clicks Start — where every test about a live run
 * begins. `extra` replaces one answer the way `mockIngest` does, and
 * `props` reaches the surface (the clock, and nothing else).
 */
export async function startRun(
  extra: Record<string, CommandHandler> = {},
  props: IngestProps = {},
): Promise<IngestCall[]> {
  const calls = mockIngest({
    [PICKER]: picksInTurn([SOURCE, DEST_A, DEST_B]),
    ...extra,
  });
  await planned(calls, props);
  await userEvent.click(
    screen.getByRole("button", { name: "Start verified copy" }),
  );
  return calls;
}

/** The run panel's own subtree, so an assertion cannot accidentally read
 *  the plan panel's counters instead. */
export function runPanel(): Promise<HTMLElement> {
  return screen.findByRole("group", { name: "Run" });
}

/**
 * Renders the surface and fills the board in: source, one destination, the
 * PARA node, and a plan. Where every test that needs a startable setup
 * begins. The picker must be mocked to answer `SOURCE` then `DEST_A`.
 */
export async function planned(
  calls: IngestCall[],
  props: IngestProps = {},
): Promise<IngestCall[]> {
  renderIngest(props);
  await userEvent.click(
    await screen.findByRole("button", { name: "Browse for source" }),
  );
  await waitFor(() => expect(sourceField().value).toBe(SOURCE));
  await userEvent.click(
    screen.getByRole("button", { name: "Browse for destination" }),
  );
  await screen.findByText(DEST_A);
  await userEvent.selectOptions(
    screen.getByRole("combobox", { name: "PARA node" }),
    NODE,
  );
  await userEvent.click(screen.getByRole("button", { name: "Plan" }));
  await screen.findByText(/1 to copy/u);
  return calls;
}

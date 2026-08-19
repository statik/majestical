import { clearMocks } from "@tauri-apps/api/mocks";
import { cleanup, screen, waitFor } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, test } from "vitest";
import type { UnfinishedRunsOutcome } from "./api";
import {
  callsTo,
  DEST_A,
  DEST_B,
  emitProgress,
  mockIngest,
  NODE,
  OTHER_RUN,
  PICKER,
  picksInTurn,
  planned,
  renderIngest,
  RUN,
  SOURCE,
} from "./ingest-test-support";
import type { CommandHandler } from "./test-support";
import { rejectCommand } from "./test-support";

/** The Start button, which every gating test watches. */
function startButton(): Promise<HTMLButtonElement> {
  return screen.findByRole<HTMLButtonElement>("button", {
    name: "Start verified copy",
  });
}

// The setup board, the plan, the completion card and the resume banner.
// `IngestView.run.test.ts` has the progress stream and everything the run
// itself puts on screen.

// Unmounted before the Tauri internals go: a surface's `listen` handle
// unregisters itself on destroy, and `clearMocks` takes the registry away.
afterEach(() => {
  cleanup();
  clearMocks();
});

/** A picker that answers the source, then the first destination. A
 *  function, not a constant: the queue is drained by the test that uses it. */
function pickBoth(): Record<string, CommandHandler> {
  return { [PICKER]: picksInTurn([SOURCE, DEST_A]) };
}

test("nothing is startable before a source, a destination, a node and a plan", async () => {
  const calls = mockIngest({ [PICKER]: picksInTurn([SOURCE, DEST_A]) });
  renderIngest();

  const start = await startButton();
  expect(start.disabled).toBe(true);

  await userEvent.click(screen.getByRole("button", { name: "Choose source…" }));
  await screen.findByText(SOURCE);
  expect(start.disabled).toBe(true);

  await userEvent.click(screen.getByRole("button", { name: "+ Add destination" }));
  await screen.findByText(DEST_A);
  expect(start.disabled).toBe(true);

  await userEvent.selectOptions(
    screen.getByRole("combobox", { name: "PARA node" }),
    NODE,
  );
  // Source, destination and node are all set — and Start is still refused,
  // because nothing copies before the plan is on screen.
  expect(start.disabled).toBe(true);

  await userEvent.click(screen.getByRole("button", { name: "Plan" }));
  await screen.findByText(/1 to copy/u);
  expect(start.disabled).toBe(false);
  expect(callsTo(calls, "start_ingest")).toHaveLength(0);
});

test("the plan panel counts what the planner decided, and says the rest verbatim", async () => {
  await planned(mockIngest(pickBoth()));

  expect(screen.getByText(/1 to copy/u)).toBeTruthy();
  expect(screen.getByText("1.0 KB")).toBeTruthy();
  expect(screen.getByText("1 duplicate skipped")).toBeTruthy();
  expect(screen.getByText("1 rejected")).toBeTruthy();
  // Rejects are rows, not errors — the same polarity every surface keeps.
  const rejects = screen.getByRole("list", { name: "Rejected by the plan" });
  expect(rejects.textContent).toContain("unreadable: permission denied");
  expect(screen.queryByRole("alert")).toBeNull();
  expect(
    screen.getByText("a warning the plan_ingest call collected"),
  ).toBeTruthy();
  expect(
    screen.getByText(/Projects\/client-x\/2026-08-12\/A7IV-CARD/u),
  ).toBeTruthy();
});

test("any edit stales the plan back to Plan again, and refuses Start until it is redone", async () => {
  const calls = mockIngest({ [PICKER]: picksInTurn([SOURCE, DEST_A, DEST_B]) });
  await planned(calls);

  const start = screen.getByRole<HTMLButtonElement>("button", {
    name: "Start verified copy",
  });
  expect(start.disabled).toBe(false);

  await userEvent.click(screen.getByRole("button", { name: "+ Add destination" }));
  await screen.findByText(DEST_B);

  expect(start.disabled).toBe(true);
  expect(screen.getByText(/plan again before starting/u)).toBeTruthy();

  await userEvent.click(screen.getByRole("button", { name: "Plan again" }));
  await waitFor(() => expect(start.disabled).toBe(false));
  expect(callsTo(calls, "plan_ingest")).toHaveLength(2);
});

test("the template box names the default it is left at, and sends nothing until it is typed in", async () => {
  const calls = mockIngest(pickBoth());
  await planned(calls);

  const box = screen.getByRole("textbox", { name: "Subfolder template" });
  expect(box.getAttribute("placeholder")).toBe("{date}/{source-label}");
  expect(callsTo(calls, "plan_ingest")[0]?.args).toEqual({
    source: SOURCE,
    para: NODE,
    template: undefined,
  });

  // No braces typed here: `userEvent.type` reads `{` as the start of a key
  // descriptor, and what this pins is that an edited box is sent at all.
  await userEvent.type(box, "cards-only");
  await userEvent.click(screen.getByRole("button", { name: "Plan again" }));

  await waitFor(() =>
    expect(callsTo(calls, "plan_ingest")[1]?.args).toEqual({
      source: SOURCE,
      para: NODE,
      template: "cards-only",
    }),
  );
});

test("an archived node is not offered to file into", async () => {
  mockIngest(pickBoth());
  renderIngest();

  const picker = await screen.findByRole("combobox", { name: "PARA node" });
  await screen.findByRole("option", { name: "project/client-x" });
  const options = [...picker.querySelectorAll("option")].map((o) => o.textContent);
  expect(options).toEqual(["Choose a PARA node…", "project/client-x"]);
});

test("a refused plan shows the command's whole message chain and its notices", async () => {
  mockIngest({
    ...pickBoth(),
    plan_ingest: () =>
      rejectCommand("source must be a directory: /Volumes/A7IV-CARD", [
        "the catalog log had one unreadable entry",
      ]),
  });
  renderIngest();

  await userEvent.click(await screen.findByRole("button", { name: "Choose source…" }));
  await screen.findByText(SOURCE);
  await userEvent.selectOptions(
    screen.getByRole("combobox", { name: "PARA node" }),
    NODE,
  );
  await userEvent.click(screen.getByRole("button", { name: "Plan" }));

  const alert = await screen.findByRole("alert");
  expect(alert.textContent).toBe("source must be a directory: /Volumes/A7IV-CARD");
  expect(screen.getByText("the catalog log had one unreadable entry")).toBeTruthy();
});

test("cancelling the folder picker changes nothing", async () => {
  const calls = mockIngest({ [PICKER]: () => null });
  renderIngest();

  await userEvent.click(await screen.findByRole("button", { name: "Choose source…" }));

  await waitFor(() => expect(callsTo(calls, PICKER)).toHaveLength(1));
  expect(screen.getByText("No source chosen yet.")).toBeTruthy();
  expect(screen.queryByRole("alert")).toBeNull();
});

const unfinished: UnfinishedRunsOutcome = {
  runs: [
    {
      run_id: RUN,
      placed: 118,
      planned: 207,
      source: SOURCE,
      destinations: [DEST_A, DEST_B],
    },
  ],
  notices: ["one run journal could not be read"],
};

test("an unfinished run becomes a banner, and Resume fills in everything the journal knows", async () => {
  const calls = mockIngest({
    ...pickBoth(),
    list_unfinished_ingests: () => unfinished,
  });
  renderIngest();

  const banner = await screen.findByRole("list", { name: "Unfinished runs" });
  expect(banner.textContent).toContain(RUN);
  expect(banner.textContent).toContain("118 of 207 files placed");
  expect(banner.textContent).toContain(SOURCE);
  expect(screen.getByText("one run journal could not be read")).toBeTruthy();

  await userEvent.click(screen.getByRole("button", { name: `Resume run ${RUN}` }));

  // Source and destinations come back off the journal; the PARA node does
  // not — `UnfinishedRun` has no field for it — so the board asks again and
  // Start stays refused until it is answered and re-planned.
  expect(screen.getByText(SOURCE)).toBeTruthy();
  expect(screen.getByText(DEST_A)).toBeTruthy();
  expect(screen.getByText(DEST_B)).toBeTruthy();
  expect(
    screen.getByRole<HTMLButtonElement>("button", { name: "Start verified copy" })
      .disabled,
  ).toBe(true);

  await userEvent.selectOptions(
    screen.getByRole("combobox", { name: "PARA node" }),
    NODE,
  );
  await userEvent.click(screen.getByRole("button", { name: "Plan" }));
  await screen.findByText(/1 to copy/u);
  await userEvent.click(screen.getByRole("button", { name: "Start verified copy" }));

  await waitFor(() =>
    expect(callsTo(calls, "start_ingest")[0]?.args).toEqual({
      source: SOURCE,
      dests: [DEST_A, DEST_B],
      para: NODE,
      template: undefined,
      resume: RUN,
    }),
  );
});

test("hiding a resume banner leaves the board alone", async () => {
  mockIngest({ ...pickBoth(), list_unfinished_ingests: () => unfinished });
  renderIngest();

  await screen.findByRole("list", { name: "Unfinished runs" });
  await userEvent.click(screen.getByRole("button", { name: `Hide run ${RUN} for now` }));

  await waitFor(() =>
    expect(screen.queryByRole("list", { name: "Unfinished runs" })).toBeNull(),
  );
  expect(screen.getByText("No source chosen yet.")).toBeTruthy();
});

test("re-picking the source after a Resume drops the run it would have continued", async () => {
  const calls = mockIngest({
    [PICKER]: picksInTurn(["/Volumes/OTHER-CARD", DEST_A]),
    list_unfinished_ingests: () => unfinished,
  });
  renderIngest();

  await screen.findByRole("list", { name: "Unfinished runs" });
  await userEvent.click(screen.getByRole("button", { name: `Resume run ${RUN}` }));
  await userEvent.click(screen.getByRole("button", { name: "Choose source…" }));
  await screen.findByText("/Volumes/OTHER-CARD");

  await userEvent.selectOptions(
    screen.getByRole("combobox", { name: "PARA node" }),
    NODE,
  );
  await userEvent.click(screen.getByRole("button", { name: "Plan" }));
  await screen.findByText(/1 to copy/u);
  await userEvent.click(screen.getByRole("button", { name: "Start verified copy" }));

  await waitFor(() =>
    expect(callsTo(calls, "start_ingest")[0]?.args).toEqual({
      source: "/Volumes/OTHER-CARD",
      dests: [DEST_A, DEST_B],
      para: NODE,
      template: undefined,
      resume: undefined,
    }),
  );
});

test("mounting mid-run resumes rendering the run the backend is already on", async () => {
  mockIngest({
    ...pickBoth(),
    ingest_state: () => ({ busy: true, running: RUN }),
    list_unfinished_ingests: () => unfinished,
  });
  renderIngest();

  const run = await screen.findByRole("group", { name: "Run" });
  // The journal lists this very run as unfinished — it is, until it ends —
  // and a banner offering to Resume it on top of its own live progress
  // would be two contradictory things about one run.
  expect(screen.queryByRole("list", { name: "Unfinished runs" })).toBeNull();
  expect(run.textContent).toContain(`run ${RUN} — resumable`);
  // The totals rode `run_started`, which happened before this surface
  // existed — so the bar is absent rather than drawn against a guess, and
  // there is no elapsed clock either: this window never saw the run start.
  expect(screen.queryByRole("progressbar")).toBeNull();
  expect(run.textContent).not.toContain("elapsed");
  expect(screen.queryByRole("button", { name: "Start verified copy" })).toBeNull();

  // The events still coming are this run's to apply, and another run's are
  // still not — the id came off `ingest_state`, not off a start this
  // window made.
  await emitProgress(RUN, { type: "file_started", rel: "a.mov", size: 400 });
  await emitProgress(RUN, { type: "file_placed", rel: "a.mov" });
  await emitProgress(OTHER_RUN, { type: "file_placed", rel: "z.mov" });

  await waitFor(() => expect(run.textContent).toContain("1 file done"));
  expect(run.textContent).not.toContain("2 files done");
});

test("a state read that fails on mount says so instead of showing a clean board", async () => {
  const message = "the ingest job state is not managed";
  mockIngest({
    ...pickBoth(),
    ingest_state: () => rejectCommand(message, ["one run journal could not be read"]),
  });
  renderIngest();

  // Nothing else on this surface would ever mention it: the board renders
  // exactly as it does when nothing is running.
  const alert = await screen.findByRole("alert");
  expect(alert.textContent).toBe(message);
  expect(screen.getByText("one run journal could not be read")).toBeTruthy();
  expect(screen.getByRole("button", { name: "Choose source…" })).toBeTruthy();
});

test("the source panel says what the plan walked, and stops when the plan goes stale", async () => {
  const calls = mockIngest({ [PICKER]: picksInTurn([SOURCE, DEST_A, DEST_B]) });
  await planned(calls);

  // Every file the walk found and their bytes — not the copy subset, which
  // is the plan panel's line.
  expect(screen.getByText("A7IV-CARD · 3 files · 7.0 KB")).toBeTruthy();

  await userEvent.click(screen.getByRole("button", { name: "+ Add destination" }));
  await screen.findByText(DEST_B);

  await waitFor(() =>
    expect(screen.queryByText(/A7IV-CARD · 3 files/u)).toBeNull(),
  );
});

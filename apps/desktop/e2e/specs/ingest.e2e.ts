// Ingest flow: the one spec that copies real bytes. A source folder and a
// destination folder are TYPED, not browsed — the native folder picker is a
// macOS dialog no WebDriver session can drive, and typed paths are the half
// of path entry this suite can actually exercise end to end. What it proves
// is the whole chain in one go: a typed source and destination reach
// `plan_ingest`, the plan's counts say what the run will do, Start hands the
// job to the backend, and the completion card the surface draws afterwards
// agrees with what is on disk under the destination.
//
// THIS SPEC RUNS LAST, and wdio.conf.ts's explicit `specs` order is what
// makes that true (see the comment there). An ingest run appends immutable
// events to the shared fixture catalog — two new assets, and a new volume
// for the destination root — and `onPrepare` seeds that catalog once for the
// whole `wdio run`, not once per spec file. organize.e2e.ts can undo its
// mutation with `maj tag rm`; nothing undoes an ingest, so this spec has no
// `after()` cleanup and nothing may run after it. volumes.e2e.ts is the
// concrete casualty: it asserts EXACTLY one volume row with exactly 3
// assets, both of which this run changes.
//
// The destination is added with the Add button rather than an Enter
// keypress. Both run the same `addTypedDest` handler; the button click is
// plain WebDriver, which is all this suite uses anywhere (organize.e2e.ts's
// header records key input through the Actions API not carrying against
// this embedded driver), so the button is the path with no second thing to
// debug if the row does not appear.
import { readdir } from "node:fs/promises";
import { $, browser, expect } from "@wdio/globals";
import { readFixtureCatalog } from "../setup/fixture-catalog.ts";
import type { FixtureCatalog } from "../setup/fixture-catalog.ts";
import { openSurface } from "../setup/surfaces.ts";
import { suppressAutoFocusRecovery } from "../setup/window-focus.ts";

/** Start does the planning walk, hashes every file, copies, verifies each
 *  copy by reading it back, and writes an MHL generation per destination —
 *  on two 14-byte files that is fast, but it is a real backend run over a
 *  real filesystem, so it gets a minute rather than the surface-render
 *  timeouts the rest of this file uses. */
const RUN_TIMEOUT = 60_000;

/** Fills the board in: the typed source, the typed destination, and the
 *  PARA node to file under. The source box commits on every keystroke
 *  (IngestView.svelte's `setSource`), so Plan is already enabled when this
 *  returns — no blur, and no click on a still-disabled button. */
async function fillJob(fixture: FixtureCatalog): Promise<void> {
  await $('[aria-label="Source path"]').setValue(fixture.ingestSourceDir);

  await $('[aria-label="Destination path"]').setValue(fixture.ingestDestDir);
  await $('[aria-label="Add destination"]').click();
  await expect($('[aria-label="Destinations"]')).toHaveText(
    expect.stringContaining(fixture.ingestDestDir),
  );

  // Rendered as `${kind}/${name}` from the node setup/fixture-catalog.ts
  // seeded with `maj para add project e2e-ingest`.
  await selectNode(`project/${fixture.paraNodeName}`);
}

/** Chooses a PARA node by its option text. Not `selectByVisibleText`: against
 *  this embedded WebKit driver (tauri-plugin-wdio-webdriver) the option click
 *  it performs leaves the `<select>`'s value unchanged and fires no `change`
 *  (observed 2026-09-15: value still `""`, Plan still disabled), the same
 *  class of gap organize.e2e.ts documents for a held-key click. Setting the
 *  value and dispatching `change` runs the exact `onchange` handler
 *  IngestView.svelte wires (`pickNode`), so this exercises the real app code. */
async function selectNode(optionText: string): Promise<void> {
  const select = await $('[aria-label="PARA node"]');
  await select.waitForDisplayed({ timeout: 10_000 });
  await browser.execute(
    `const select = arguments[0];
     const option = Array.from(select.options).find((o) => o.text === arguments[1]);
     if (!option) throw new Error("no PARA option " + arguments[1]);
     select.value = option.value;
     select.dispatchEvent(new Event("change", { bubbles: true }));`,
    select,
    optionText,
  );
  await expect(select).not.toHaveValue("");
}

/**
 * Plans the job and reads the counts back. Both source files are new to the
 * catalog and non-empty, so the planner decides `Copy` for each — see
 * `INGEST_SOURCE_FILES`' comment in setup/fixture-catalog.ts for why that is
 * a fact and not a hope. `.ingest-counts` is unambiguous here: the
 * completion card carries the same class, but no run has finished yet, so
 * the plan panel's line is the only one in the document.
 */
async function planTwoCopies(): Promise<void> {
  await $("button=Plan").click();
  const counts = await $(".ingest-counts");
  await counts.waitForDisplayed({ timeout: 10_000 });
  await expect(counts).toHaveText(expect.stringContaining("2 to copy"));
}

/**
 * Starts the run and waits out the completion card. The card is drawn from
 * the finished `IngestRun` the surface polls for after the copy loop ends —
 * the authority on what the run placed, as opposed to the progress events
 * the run panel accumulated. The failures list is rendered only when there
 * were failures, so its absence is the assertion that every copy verified.
 */
async function runToCompletion(): Promise<void> {
  await $("button=Start verified copy").click();
  const card = await $('[aria-label="Completed run"]');
  await card.waitForDisplayed({ timeout: RUN_TIMEOUT });
  await expect(card).toHaveText(expect.stringContaining("2 placed"));
  await expect($('[aria-label="Failed files"]')).not.toBeExisting();
}

/** And the bytes are really there. Files land at `<dest>/<subdir>/<rel>`
 *  (crates/ingest/src/engine.rs), where the subdir comes from the layout
 *  template, so this matches on the tail of each path rather than naming a
 *  date-dependent folder. */
async function assertPlacedOnDisk(destDir: string): Promise<void> {
  const placed = await readdir(destDir, { recursive: true });
  expect(placed.some((entry) => entry.endsWith("clip-a.txt"))).toBe(true);
  expect(placed.some((entry) => entry.endsWith("clip-b.txt"))).toBe(true);
}

describe("Majestical desktop — Ingest flow", () => {
  let fixture: FixtureCatalog;

  before(async () => {
    fixture = readFixtureCatalog();
    await suppressAutoFocusRecovery(browser);
    await $('[data-e2e="nav-search"]').waitForDisplayed({ timeout: 20_000 });
  });

  it("copies a typed source into a typed destination, verified, filed under the PARA node", async () => {
    await openSurface('[data-e2e="nav-ingest"]', ".ingest-surface");
    await fillJob(fixture);
    await planTwoCopies();
    await runToCompletion();
    await assertPlacedOnDisk(fixture.ingestDestDir);
  });
});

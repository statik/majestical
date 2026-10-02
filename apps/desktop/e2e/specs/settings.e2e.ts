// Settings flow: the health panel renders the real `maj doctor` sweep over
// the fixture catalog — all nine checks, in the order services emits them
// (the surface never sorts), with the catalog row Ok because the fixture
// catalog is the selected one. Environment rows (ffmpeg, models, platform)
// are asserted present, not by status: their status is the machine's.
import { $, $$, browser, expect } from "@wdio/globals";
import { openSurface } from "../setup/surfaces.ts";
import { suppressAutoFocusRecovery } from "../setup/window-focus.ts";

describe("Majestical desktop — Settings flow", () => {
  before(async () => {
    await suppressAutoFocusRecovery(browser);
    await $('[data-e2e="nav-search"]').waitForDisplayed({ timeout: 20_000 });
    await openSurface('[data-e2e="nav-settings"]', ".settings-checks");
  });

  it("shows every doctor check in the order services emits them", async () => {
    await expect($$(".settings-check")).toBeElementsArrayOfSize(9);
    const names = await $$(".settings-check-name").map((el) => el.getText());
    expect(names).toEqual([
      "ffmpeg",
      "imagemagick",
      "models",
      "state_dir",
      "catalog",
      "blob_residue",
      "failed_items",
      "describer",
      "platform",
    ]);
  });

  it("reports the selected fixture catalog as Ok", async () => {
    // Looked up by name, not position, so a check services inserts ahead of
    // it does not silently retarget this assertion. The pill's DOM text is
    // the raw wire status; CSS uppercases it for the reader.
    const rows = await $$(".settings-check");
    const names = await rows.map((el) => el.$(".settings-check-name").getText());
    const catalogRow = rows[names.indexOf("catalog")];
    if (catalogRow === undefined) throw new Error("no catalog row");
    await expect(catalogRow.$(".settings-pill")).toHaveText("ok");
  });
});

// A separate `describe` (not a third `it` above) to keep both function
// bodies under this project's line cap — see .oxlintrc.json's default.
describe("Majestical desktop — Settings — Always-on throttle", () => {
  before(async () => {
    await suppressAutoFocusRecovery(browser);
    await $('[data-e2e="nav-search"]').waitForDisplayed({ timeout: 20_000 });
    await openSurface('[data-e2e="nav-settings"]', ".settings-checks");
  });

  after(async () => {
    // Restore Auto so a later spec — in this file's own session or, if
    // sessions turn out not to be per-file, a spec sharing one — does not
    // inherit a paused scheduler.
    const autoRadio = await $('input[name="throttle"][value="auto"]');
    if (!(await autoRadio.isSelected())) {
      await autoRadio.click();
    }
  });

  it("renders the throttle radio group with Auto checked on a fresh app", async () => {
    await expect($('input[name="throttle"][value="auto"]')).toBeSelected();
  });

  it("switching to Paused round-trips through a real scheduler_state call", async () => {
    // `set_throttle`'s response is immediate, and so is the status line:
    // `indexer.rs::set_throttle_impl` recomputes the decision from the
    // last poll's power/pending state right away rather than waiting for
    // the scheduler's next tick (up to 30s later), so both the radio and
    // `.settings-status` move synchronously with the click — no tick wait
    // needed here.
    const pausedRadio = await $('input[name="throttle"][value="paused"]');
    await pausedRadio.click();

    await expect(pausedRadio).toBeSelected();
    await expect($('input[name="throttle"][value="auto"]')).not.toBeSelected();
    await expect($(".settings-status")).toHaveText("Paused");

    const autoRadio = await $('input[name="throttle"][value="auto"]');
    await autoRadio.click();

    await expect(autoRadio).toBeSelected();
    await expect($(".settings-status")).not.toHaveText("Paused");
  });
});

/** The Health panel's describer row detail, looked up by name and re-queried
 *  on every call: the panel re-renders its rows after a Save. */
async function describerDetail(): Promise<string> {
  const rows = await $$(".settings-check");
  const names = await rows.map((el) => el.$(".settings-check-name").getText());
  const row = rows[names.indexOf("describer")];
  if (row === undefined) throw new Error("no describer row");
  return row.$(".settings-check-detail").getText();
}

// Saving a describer makes caption work plannable for the rest of the
// session, and nothing listens on localhost:11434, so the scheduler's next
// batch fails. That is safe here: the only spec after this file,
// ingest.e2e.ts, asserts nothing about the scheduler's status or errors.
// Ollama is the preselected backend, so the `<select>` — which this embedded
// WebKit driver cannot change through `selectByVisibleText` — is never
// touched; Save stores no key and makes no network call.
describe("Majestical desktop — Settings — Captions", () => {
  before(async () => {
    await suppressAutoFocusRecovery(browser);
    await $('[data-e2e="nav-search"]').waitForDisplayed({ timeout: 20_000 });
    await openSurface('[data-e2e="nav-settings"]', ".settings-checks");
  });

  it("saving an Ollama describer changes the Health describer row", async () => {
    expect(await describerDetail()).toBe("no describer configured — captions off");

    await $('[data-e2e="captions-model"]').setValue("llava");
    await $('[data-e2e="captions-save"]').click();
    await $('[data-e2e="captions-saved"]').waitForDisplayed();

    await browser.waitUntil(async () => (await describerDetail()) === "ollama · llava");
  });
});

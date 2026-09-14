// Settings flow: the health panel renders the real `maj doctor` sweep over
// the fixture catalog — all seven checks, in the order services emits them
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
    await expect($$(".settings-check")).toBeElementsArrayOfSize(7);
    const names = await $$(".settings-check-name").map((el) => el.getText());
    expect(names).toEqual([
      "ffmpeg",
      "imagemagick",
      "models",
      "state_dir",
      "catalog",
      "blob_residue",
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
    // `set_throttle`'s response is immediate — this asserts on it directly,
    // not on the scheduler's next tick (30s later): the returned throttle
    // moves the checked radio right away, but `decision` (and so the
    // status line, when the scheduler has one to show) is only as fresh as
    // the last tick, so this does not assert on that line's text — doing
    // so would mean waiting out the tick, which the plan for this spec
    // rules out.
    const pausedRadio = await $('input[name="throttle"][value="paused"]');
    await pausedRadio.click();

    await expect(pausedRadio).toBeSelected();
    await expect($('input[name="throttle"][value="auto"]')).not.toBeSelected();
  });
});

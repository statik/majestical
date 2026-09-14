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
    await expect($(".settings-check:nth-child(5) .settings-pill")).toHaveText("Ok");
  });
});

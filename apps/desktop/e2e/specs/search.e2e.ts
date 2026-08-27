// Search flow: type the fixture jpg's own file name into the omnibox and
// confirm both the result count and the rendered card name are for that
// asset specifically — not just that *some* card showed up.
import { $, browser, expect } from "@wdio/globals";
import { readFixtureCatalog } from "../setup/fixture-catalog.ts";
import type { FixtureCatalog } from "../setup/fixture-catalog.ts";
import { suppressAutoFocusRecovery } from "../setup/window-focus.ts";

describe("Majestical desktop — Search flow", () => {
  let fixture: FixtureCatalog;

  before(async () => {
    fixture = readFixtureCatalog();
    await suppressAutoFocusRecovery(browser);
    await $('[data-e2e="nav-search"]').waitForDisplayed({ timeout: 20_000 });
    await $('[data-e2e="nav-search"]').click();
  });

  it("finds the fixture photo by its own file name", async () => {
    const omnibox = await $(".omnibox");
    await omnibox.setValue(fixture.photoFileName);

    // Debounced (SearchView.svelte's DEBOUNCE_MS): the count only appears
    // once the search this text queued has actually resolved.
    const count = await $(".count");
    await count.waitForDisplayed({ timeout: 10_000 });
    await expect(count).toHaveText("1 results");

    const card = await $(".grid .card");
    await expect(card).toBeDisplayed();
    await expect(card.$(".name")).toHaveText(fixture.photoFileName);
  });
});

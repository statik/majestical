// Launch smoke: the real app, pointed at a fixture catalog (see
// setup/fixture-catalog.ts), opens and each sidebar surface renders real
// data from that catalog.
//
// No `browser.tauri.*` bridge is used here: that requires importing
// `@wdio/tauri-plugin` in the app frontend (shipping test-only guest JS)
// and enabling `withGlobalTauri`, neither of which this suite needs — every
// assertion below is a plain WebDriver element query, which works against
// the embedded WebDriver server (`tauri-plugin-wdio-webdriver`) alone. For
// the same reason, this suite does not assert on a zero-console-errors
// invariant: capturing frontend console logs is also gated behind the
// `@wdio/tauri-plugin` bridge.
import { $, $$, browser, expect } from "@wdio/globals";
import { readFixtureCatalog } from "../setup/fixture-catalog.ts";
import type { FixtureCatalog } from "../setup/fixture-catalog.ts";
import { suppressAutoFocusRecovery } from "../setup/window-focus.ts";

/** Clicks a sidebar nav entry and waits for that surface's own container to
 *  render, so every `it` below asserts against a surface that has actually
 *  mounted rather than racing its own load. */
async function openSurface(nav: string, container: string) {
  await $(nav).click();
  const el = await $(container);
  await el.waitForDisplayed({ timeout: 10_000 });
  return el;
}

describe("Majestical desktop — launch smoke", () => {
  let fixture: FixtureCatalog;

  before(async () => {
    fixture = await readFixtureCatalog();
    await suppressAutoFocusRecovery(browser);

    // The shell's first paint is "Opening the catalog…" until `app_status`
    // resolves; a nav entry exists only once a catalog was found ready.
    await $('[data-e2e="nav-search"]').waitForDisplayed({ timeout: 20_000 });
  });

  it("opens the app window", async () => {
    await expect(browser).toHaveTitle("Majestical");
  });

  it("finds the tagged fixture asset from Search", async () => {
    await $('[data-e2e="nav-search"]').click();
    const omnibox = await $(".omnibox");
    await omnibox.setValue(`tag:${fixture.tagName}`);

    const count = await $(".count");
    await count.waitForDisplayed({ timeout: 10_000 });
    await expect(count).toHaveText("1 results");
    await expect($(".grid .card")).toBeDisplayed();
  });

  it("shows the fixture volume in Volumes", async () => {
    await openSurface('[data-e2e="nav-volumes"]', ".volume-table");
    await expect($$(".volume-table tbody tr")).toBeElementsArrayOfSize(1);
    await expect($(".volume-table .label")).toHaveText(fixture.volumeLabel);
  });

  it("shows the fixture volume in the Browse tree", async () => {
    await openSurface('[data-e2e="nav-browse"]', ".browse-tree");
    await expect($(".browse-tree .browse-label")).toHaveText(fixture.volumeLabel);
  });

  it("lists the fixture tag in Organize", async () => {
    await openSurface('[data-e2e="nav-organize"]', ".org-taglist");
    await expect($(".org-tagpill")).toHaveText(fixture.tagName);
  });

  it("renders the Ingest setup board", async () => {
    await openSurface('[data-e2e="nav-ingest"]', ".ingest-board");
  });
});

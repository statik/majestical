// Browse flow: select the fixture volume's root (flatten defaults to true —
// BrowseView.svelte — so the root lists its whole subtree, all 3 fixture
// assets), then select the "sub" folder and confirm the grid narrows to
// just the jpg it actually contains. Real counts confirmed against
// `maj browse list --json` for this fixture: 3 items/2 folders at the
// root, 1 item/1 folder under "sub".
import { $, $$, browser, expect } from "@wdio/globals";
import { readFixtureCatalog } from "../setup/fixture-catalog.ts";
import type { FixtureCatalog } from "../setup/fixture-catalog.ts";
import { suppressAutoFocusRecovery } from "../setup/window-focus.ts";

describe("Majestical desktop — Browse flow", () => {
  let fixture: FixtureCatalog;

  before(async () => {
    fixture = readFixtureCatalog();
    await suppressAutoFocusRecovery(browser);
    await $('[data-e2e="nav-search"]').waitForDisplayed({ timeout: 20_000 });
    await $('[data-e2e="nav-browse"]').click();
    await $(".browse-tree").waitForDisplayed({ timeout: 10_000 });
  });

  it("lists the whole fixture volume, then narrows to one folder", async () => {
    // The tree auto-expands its first (only) volume on load (BrowseView's
    // loadTree), so the "sub" row is already visible without any caret click.
    const subNode = await $(".browse-tree .browse-node:not(.browse-vol)");
    await expect(subNode.$(".browse-label")).toHaveText("sub");

    await $(".browse-tree .browse-vol").click();
    const count = await $(".browse-main .count");
    await count.waitForDisplayed({ timeout: 10_000 });
    await expect(count).toHaveText("3 items across 2 folders");
    await expect($$(".browse-main .grid .card")).toBeElementsArrayOfSize(3);

    await subNode.click();
    await expect(count).toHaveText("1 items across 1 folders");
    await expect($$(".browse-main .grid .card")).toBeElementsArrayOfSize(1);
    await expect($(".browse-main .grid .card .name")).toHaveText(fixture.photoFileName);
  });
});

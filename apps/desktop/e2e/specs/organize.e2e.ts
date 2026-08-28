// Organize flow: the real mutation. Browse and Organize share one catalog,
// so a tag assigned from Browse's selection bar has to show up back in
// Organize's vocabulary — that round trip, not just one surface in
// isolation, is what this spec proves.
//
// The vocabulary-before-mutation assertion runs FIRST and the tag-assigning
// mutation SECOND, and mocha's file order is load-bearing here: `tags_list`
// (crates/services/src/tags.rs) tallies into a `BTreeMap<String, _>`, so
// Organize's tag rows are alphabetical. "e2e-added" (assigned below) sorts
// before the fixture tag `fixture.tagName` ("e2e-smoke"), so `$(".org-tagpill")`
// — the first pill on the page — would find the NEW tag instead of the
// fixture one if this ran after the mutation. Keeping the pre-mutation read
// first is what keeps that selector honest.
//
// SelectionBar.svelte only raises its bar at 2+ selected assets (`MINIMUM`),
// so the mutation below ctrl-clicks a second card rather than the single
// click the phase 7E plan sketched — which is also why the new tag lands
// with count 2, not 1: both selected assets get it.
//
// The ctrl-click is a dispatched `MouseEvent`, not a held-key WebDriver
// Actions sequence: `browser.action('key').down(...)` followed by a
// separate `element.click()` does not carry the held key into that second
// command against this embedded WebKit driver (tauri-plugin-wdio-webdriver)
// — the bar never raised when tried that way. Dispatching the event
// directly still runs the exact same handler `clickCard` wires up
// (BrowseView.svelte's plain `addEventListener("click", ...)` via Svelte's
// `onclick`), so this is exercising the real app code, not a stand-in for
// it. The dispatch itself is a string script rather than a function: this
// project's tsconfig carries no `dom` lib (nothing else here touches the
// DOM), and a string script sidesteps type-checking DOM globals for the one
// place that needs them.
//
// `after()` below undoes the tag assignment via `maj tag rm`: `onPrepare`
// (wdio.conf.ts) seeds this catalog once for the whole `wdio run`, not once
// per spec file, so a tag left standing here would leak into every spec
// file mocha runs after this one in the same invocation (smoke.e2e.ts's own
// Organize assertion caught exactly that the first time this spec ran
// without cleanup).
import { $, $$, browser, expect } from "@wdio/globals";
import { readFixtureCatalog, removeTag } from "../setup/fixture-catalog.ts";
import type { FixtureCatalog } from "../setup/fixture-catalog.ts";
import { openSurface } from "../setup/surfaces.ts";
import { suppressAutoFocusRecovery } from "../setup/window-focus.ts";

const NEW_TAG = "e2e-added";

/** Safe indexing into a list already size-checked at runtime — a plain
 *  generic `T[]` doesn't unify with `WebdriverIO.ElementArray` (it overrides
 *  `Array.prototype.every` with an incompatible signature), so this is typed
 *  directly for it rather than generically. `noUncheckedIndexedAccess`
 *  still types a plain index as possibly `undefined`; this is where that's
 *  resolved, the same way `fixture-catalog.ts`'s `firstAssetId` resolves
 *  its own. */
function nth(list: WebdriverIO.ElementArray, index: number): WebdriverIO.Element {
  const item = list[index];
  if (item === undefined) {
    throw new Error(`expected an element at index ${index}, found none`);
  }
  return item;
}

/** A ctrl-click, dispatched directly at the element — see the header
 *  comment for why this isn't a WebDriver Actions key-hold. */
async function ctrlClick(el: WebdriverIO.Element): Promise<void> {
  await browser.execute(
    "arguments[0].dispatchEvent(new MouseEvent('click', { bubbles: true, cancelable: true, ctrlKey: true }))",
    el,
  );
}

/** Picks two cards from Browse's grid — a plain click on the first, a
 *  ctrl-click on the second — crossing SelectionBar's 2-asset threshold. */
async function selectTwoCards(): Promise<void> {
  await openSurface('[data-e2e="nav-browse"]', ".browse-tree");

  // The root lists the whole fixture volume (flatten defaults to true —
  // BrowseView.svelte), so it has the 3 fixture assets to pick 2 cards from.
  await $(".browse-tree .browse-vol").click();
  const count = await $(".browse-main .count");
  await count.waitForDisplayed({ timeout: 10_000 });
  await expect(count).toHaveText("3 items across 2 folders");
  const cards = await $$(".browse-main .grid .card").getElements();
  expect(cards.length).toBe(3);

  // A plain click selects the first card alone (selection.ts's
  // `clickSelection`); ctrl-clicking the second adds it without dropping
  // the first.
  await nth(cards, 0).click();
  await ctrlClick(nth(cards, 1));

  const selBar = await $(".sel-bar");
  await selBar.waitForDisplayed({ timeout: 10_000 });
  await expect($(".sel-count")).toHaveText("2 selected");
}

/** Opens the bar's tag picker, types the new tag, and applies it. */
async function applyNewTag(): Promise<void> {
  await $("button=Tag…").click();
  const newTagInput = await $('[aria-label="New tag"]');
  await newTagInput.waitForDisplayed({ timeout: 10_000 });
  await newTagInput.setValue(NEW_TAG);
  await $("button=Apply tags").click();

  const outcome = await $(".sel-bar .count");
  await outcome.waitForDisplayed({ timeout: 10_000 });
  await expect(outcome).toHaveText("Tagged 2 assets");
}

describe("Majestical desktop — Organize flow", () => {
  let fixture: FixtureCatalog;

  before(async () => {
    fixture = readFixtureCatalog();
    await suppressAutoFocusRecovery(browser);
    await $('[data-e2e="nav-search"]').waitForDisplayed({ timeout: 20_000 });
  });

  after(() => {
    removeTag(fixture, NEW_TAG);
  });

  it("lists the fixture tag with its real count, before any mutation", async () => {
    await openSurface('[data-e2e="nav-organize"]', ".org-taglist");
    await expect($(".org-tagpill")).toHaveText(fixture.tagName);
    const counts = await $$(".org-num").getElements();
    await expect(nth(counts, 0)).toHaveText("1");
  });

  it("assigns a new tag to two Browse cards and Organize picks it up", async () => {
    await selectTwoCards();
    await applyNewTag();

    await openSurface('[data-e2e="nav-organize"]', ".org-taglist");
    const rows = await $$(".org-tagrow").getElements();
    expect(rows.length).toBe(2);
    // Alphabetical: "e2e-added" sorts before the fixture tag.
    const newTagRow = nth(rows, 0);
    await expect(newTagRow.$(".org-tagpill")).toHaveText(NEW_TAG);
    const newTagCounts = await newTagRow.$$(".org-num").getElements();
    await expect(nth(newTagCounts, 0)).toHaveText("2");
  });
});

// Volumes flow: the fixture volume's row reports what the fixture actually
// scanned — three assets (fixture-doc.pdf, fixture-note.txt,
// sub/fixture-photo.jpg; see setup/fixture-catalog.ts) — and its real online
// state, not the state a mounted drive would show.
import { $, $$, browser, expect } from "@wdio/globals";
import { readFixtureCatalog } from "../setup/fixture-catalog.ts";
import type { FixtureCatalog } from "../setup/fixture-catalog.ts";
import { suppressAutoFocusRecovery } from "../setup/window-focus.ts";

describe("Majestical desktop — Volumes flow", () => {
  let fixture: FixtureCatalog;

  before(async () => {
    fixture = readFixtureCatalog();
    await suppressAutoFocusRecovery(browser);
    await $('[data-e2e="nav-search"]').waitForDisplayed({ timeout: 20_000 });
    await $('[data-e2e="nav-volumes"]').click();
    await $(".volume-table").waitForDisplayed({ timeout: 10_000 });
  });

  it("shows the fixture volume's real label, asset count, and online state", async () => {
    await expect($$(".volume-table tbody tr")).toBeElementsArrayOfSize(1);
    await expect($(".volume-table .label")).toHaveText(fixture.volumeLabel);
    await expect($(".volume-table .asset-count")).toHaveText("3");

    // Pinned offline, not online: `volume_is_online` (crates/services/src/
    // volumes.rs) treats a `--volume`-labeled id as online only if
    // `/Volumes/<label>` exists. The fixture scans a directory under the
    // repo, not a real mount at that path, so this volume reads offline in
    // every environment this suite runs in — confirmed against the real
    // `maj volumes list --json` output for this fixture, not assumed.
    // No `label` prop on the Volumes surface's badge (VolumesView.svelte —
    // the label column already shows it), so the glyph and the accessible
    // name are the bare state, not the volume's label.
    const badge = await $(".volume-table .badge");
    await expect(badge).toHaveText("○");
    await expect(badge).toHaveAttribute("aria-label", "offline");
  });
});

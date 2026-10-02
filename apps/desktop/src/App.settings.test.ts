// The shell's Settings tests, split out of `App.test.ts`: reaching the
// Settings surface by the sidebar and by the tray's event. Settings mounts
// three sections that each ask the backend something on mount, so these
// tests carry the mocks for all of them; each section's own suite
// (`SettingsView.test.ts`, `CaptionsSection.test.ts`,
// `AlwaysOnSection.test.ts`) is where what they render is pinned.
import { emit } from "@tauri-apps/api/event";
import { clearMocks } from "@tauri-apps/api/mocks";
import { cleanup, render, screen, waitFor } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import App from "./App.svelte";
import { NAVIGATE_SETTINGS_EVENT } from "./lib/api-alwayson";
import { listenerCount } from "./lib/ingest-test-support";
import { mockCommands, stubMatchMedia } from "./lib/test-support";
import describerSettingsUnconfigured from "./lib/fixtures/describer_settings_unconfigured.json";
import doctorOutcome from "./lib/fixtures/doctor_outcome.json";
import schedulerState from "./lib/fixtures/scheduler_state.json";

beforeEach(() => {
  // The browse surface asks how wide the window is; jsdom has no answer.
  stubMatchMedia(false);
});
afterEach(() => {
  cleanup();
  clearMocks();
  vi.unstubAllGlobals();
});

/** What the shell asks with a ready catalog — the Search surface it lands
 *  on, the update check, the ingest run marker — plus what each Settings
 *  section asks on mount. */
function mockSettings() {
  mockCommands({
    app_status: () => ({ catalog_path: "/catalogs/main", catalog_ready: true }),
    "plugin:updater|check": () => null,
    list_saved_searches: () => ({ saved: [] }),
    ingest_state: () => ({ busy: false }),
    doctor_report: () => doctorOutcome,
    describer_settings: () => describerSettingsUnconfigured,
    scheduler_state: () => schedulerState,
    "plugin:autostart|is_enabled": () => false,
  });
}

test("the settings surface swaps in with the doctor's health rows", async () => {
  mockSettings();
  const { container } = render(App);

  const settings = await screen.findByRole("button", { name: "Settings" });
  await userEvent.click(settings);

  expect(await screen.findByRole("heading", { name: "Health" })).toBeTruthy();
  // The sidebar's own nav entries are list items too, so count check rows
  // by their class rather than by role.
  await waitFor(() =>
    expect(container.querySelectorAll(".settings-check")).toHaveLength(
      doctorOutcome.checks.length,
    ),
  );
  expect(screen.getByRole("heading", { name: "Captions" })).toBeTruthy();
  expect(screen.queryByRole("searchbox")).toBeNull();
  expect(settings.getAttribute("aria-current")).toBe("page");
});

test("the tray's navigate-settings event selects the Settings surface", async () => {
  mockSettings();
  render(App);

  // Still on Search: the event can arrive at any time, not only after the
  // operator has clicked into Settings themselves. Two listeners mount with
  // the shell (ingest progress and this one); wait for both, the same race
  // `emitProgress` guards against.
  await screen.findByRole("searchbox");
  await waitFor(() => expect(listenerCount()).toBeGreaterThanOrEqual(2));

  await emit(NAVIGATE_SETTINGS_EVENT, null);

  expect(await screen.findByRole("heading", { name: "Health" })).toBeTruthy();
  expect(screen.queryByRole("searchbox")).toBeNull();
});

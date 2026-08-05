import { clearMocks, mockIPC } from "@tauri-apps/api/mocks";
import { render, screen, waitFor } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { mockCommands } from "./test-support";
import UpdateBanner from "./UpdateBanner.svelte";

// Every failure path in `updater.ts` ends in a `console.debug`, and these
// tests assert on it: it is the only externally visible difference between
// "the failure was caught" and "the promise rejected into nowhere", which is
// exactly the property this component depends on.
beforeEach(() => {
  vi.spyOn(console, "debug").mockImplementation(() => {});
});
afterEach(() => {
  clearMocks();
  vi.restoreAllMocks();
});

/** What `plugin:updater|check` answers with when a release is newer than us. */
const metadata = {
  rid: 1,
  currentVersion: "0.1.0",
  version: "0.2.0",
  rawJson: {},
};

test("an available update is offered by version", async () => {
  mockCommands({ "plugin:updater|check": () => metadata });
  render(UpdateBanner);

  expect(await screen.findByText("Update to v0.2.0 available")).toBeTruthy();
  expect(screen.getByRole("button", { name: "Restart to apply" })).toBeTruthy();
});

test("nothing is offered when the endpoint has nothing newer", async () => {
  mockCommands({ "plugin:updater|check": () => null });
  const { container } = render(UpdateBanner);

  await waitFor(() =>
    expect(container.querySelector(".update-banner")).toBeNull(),
  );
  expect(console.debug).not.toHaveBeenCalled();
});

test("a rejected check is swallowed, not surfaced", async () => {
  // Offline, endpoint unreachable, or a signature that does not verify — the
  // plugin rejects, and the shell must not learn about it.
  mockIPC((cmd) => {
    throw new Error(`Command ${cmd} failed: error sending request`);
  });
  const { container } = render(UpdateBanner);

  await waitFor(() => expect(console.debug).toHaveBeenCalled());
  expect(container.querySelector(".update-banner")).toBeNull();
  expect(screen.queryByRole("alert")).toBeNull();
});

test("applying an update installs it, then restarts into it", async () => {
  const calls: string[] = [];
  mockCommands({
    "plugin:updater|check": () => {
      calls.push("check");
      return metadata;
    },
    "plugin:updater|download_and_install": () => {
      calls.push("install");
      return null;
    },
    "plugin:process|restart": () => {
      calls.push("restart");
      return null;
    },
  });
  render(UpdateBanner);

  await userEvent.click(
    await screen.findByRole("button", { name: "Restart to apply" }),
  );

  await waitFor(() => expect(calls).toContain("restart"));
  // Order matters: restarting before the bytes are installed would relaunch
  // the same version.
  expect(calls.indexOf("install")).toBeLessThan(calls.indexOf("restart"));
});

test("an install that fails leaves the banner up to try again", async () => {
  mockIPC((cmd) => {
    if (cmd === "plugin:updater|check") return metadata;
    throw new Error("download failed: connection reset");
  });
  render(UpdateBanner);

  await userEvent.click(
    await screen.findByRole("button", { name: "Restart to apply" }),
  );

  await waitFor(() => expect(console.debug).toHaveBeenCalled());
  const retry = screen.getByRole("button", { name: "Restart to apply" });
  expect(retry.hasAttribute("disabled")).toBe(false);
  expect(screen.queryByRole("alert")).toBeNull();
});

test("dismissing takes the banner away for the session", async () => {
  mockCommands({ "plugin:updater|check": () => metadata });
  const { container } = render(UpdateBanner);

  await userEvent.click(
    await screen.findByRole("button", { name: "Dismiss update notice" }),
  );

  expect(container.querySelector(".update-banner")).toBeNull();
});

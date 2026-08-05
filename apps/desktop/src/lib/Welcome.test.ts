import { clearMocks, mockIPC } from "@tauri-apps/api/mocks";
import { render, screen, waitFor } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, test } from "vitest";
import type { AppStatus } from "./api";
import Welcome from "./Welcome.svelte";

afterEach(clearMocks);

const ready: AppStatus = { catalog_path: "/catalogs/main", catalog_ready: true };

/**
 * `open()` from `@tauri-apps/plugin-dialog` is a plain command invoke —
 * `invoke("plugin:dialog|open", { options })` — so the folder picker mocks
 * through the same channel as everything else.
 */
function mockPicker(picked: string | null, onCommand: (cmd: string, args?: unknown) => unknown) {
  mockIPC((cmd, args) => {
    if (cmd === "plugin:dialog|open") return picked;
    return onCommand(cmd, args);
  });
}

test("the first-run surface offers both ways to reach a catalog", () => {
  mockPicker(null, () => null);
  render(Welcome, { oninitialized: () => {} });

  expect(screen.getByRole("heading", { name: /majestical/iu })).toBeTruthy();
  expect(screen.getByRole("button", { name: /^Initialize catalog/u })).toBeTruthy();
  expect(screen.getByRole("button", { name: /^Use existing catalog/u })).toBeTruthy();
});

test("initializing invokes initialize_catalog with the picked folder", async () => {
  const paths: string[] = [];
  let initialized: AppStatus | null = null;
  mockPicker("/catalogs/main", (cmd, args) => {
    if (cmd === "initialize_catalog") {
      paths.push((args as { path: string }).path);
      return ready;
    }
    throw new Error(`unexpected command ${cmd}`);
  });

  render(Welcome, { oninitialized: (status) => (initialized = status) });
  await userEvent.click(screen.getByRole("button", { name: /^Initialize catalog/u }));

  await waitFor(() => expect(paths).toEqual(["/catalogs/main"]));
  await waitFor(() => expect(initialized).toEqual(ready));
});

test("opening an existing catalog invokes use_existing_catalog", async () => {
  const paths: string[] = [];
  mockPicker("/catalogs/main", (cmd, args) => {
    if (cmd === "use_existing_catalog") {
      paths.push((args as { path: string }).path);
      return ready;
    }
    throw new Error(`unexpected command ${cmd}`);
  });

  render(Welcome, { oninitialized: () => {} });
  await userEvent.click(screen.getByRole("button", { name: /^Use existing catalog/u }));

  await waitFor(() => expect(paths).toEqual(["/catalogs/main"]));
});

test("cancelling the picker adopts nothing", async () => {
  let commands = 0;
  mockPicker(null, (cmd) => {
    commands += 1;
    throw new Error(`unexpected command ${cmd}`);
  });

  render(Welcome, { oninitialized: () => {} });
  await userEvent.click(screen.getByRole("button", { name: /^Initialize catalog/u }));

  await waitFor(() => expect(commands).toBe(0));
  expect(screen.queryByRole("alert")).toBeNull();
});

test("a refused catalog shows the command's whole message chain", async () => {
  const message =
    "no catalog at /catalogs/main — run `maj catalog init /catalogs/main` to create one";
  // eslint-disable-next-line prefer-promise-reject-errors -- a rejected command carries the serialized `CommandError`, never an Error instance.
  mockPicker("/catalogs/main", () => Promise.reject({ message }));

  render(Welcome, { oninitialized: () => {} });
  await userEvent.click(screen.getByRole("button", { name: /^Use existing catalog/u }));

  const alert = await screen.findByRole("alert");
  expect(alert.textContent).toBe(message);
});

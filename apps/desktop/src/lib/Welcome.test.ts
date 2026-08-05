import { clearMocks, mockIPC } from "@tauri-apps/api/mocks";
import { render, screen, waitFor } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, test } from "vitest";
import type { AppStatus } from "./api";
import { mockCommands, rejectCommand } from "./test-support";
import Welcome from "./Welcome.svelte";

afterEach(clearMocks);

const ready: AppStatus = { catalog_path: "/catalogs/main", catalog_ready: true };

// `open()` from `@tauri-apps/plugin-dialog` is a plain command invoke —
// `invoke("plugin:dialog|open", { options })` — so the folder picker mocks
// through the same channel as everything else.
const PICKER = "plugin:dialog|open";

test("the first-run surface offers both ways to reach a catalog", () => {
  mockCommands({ [PICKER]: () => null });
  render(Welcome, { oninitialized: () => {} });

  expect(screen.getByRole("heading", { name: /majestical/iu })).toBeTruthy();
  expect(screen.getByRole("button", { name: /^Initialize catalog/u })).toBeTruthy();
  expect(screen.getByRole("button", { name: /^Use existing catalog/u })).toBeTruthy();
});

test("initializing invokes initialize_catalog with the picked folder", async () => {
  const paths: string[] = [];
  let initialized: AppStatus | null = null;
  mockCommands({
    [PICKER]: () => "/catalogs/main",
    initialize_catalog: (args) => {
      paths.push((args as { path: string }).path);
      return ready;
    },
  });

  render(Welcome, { oninitialized: (status) => (initialized = status) });
  await userEvent.click(screen.getByRole("button", { name: /^Initialize catalog/u }));

  await waitFor(() => expect(paths).toEqual(["/catalogs/main"]));
  await waitFor(() => expect(initialized).toEqual(ready));
});

test("opening an existing catalog invokes use_existing_catalog", async () => {
  const paths: string[] = [];
  mockCommands({
    [PICKER]: () => "/catalogs/main",
    use_existing_catalog: (args) => {
      paths.push((args as { path: string }).path);
      return ready;
    },
  });

  render(Welcome, { oninitialized: () => {} });
  await userEvent.click(screen.getByRole("button", { name: /^Use existing catalog/u }));

  await waitFor(() => expect(paths).toEqual(["/catalogs/main"]));
});

test("cancelling the picker adopts nothing", async () => {
  // Counted rather than left to `mockCommands`: the assertion here is that
  // nothing beyond the picker was invoked at all.
  let commands = 0;
  mockIPC((cmd) => {
    if (cmd === PICKER) return null;
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
  mockCommands({
    [PICKER]: () => "/catalogs/main",
    use_existing_catalog: () => rejectCommand(message),
  });

  render(Welcome, { oninitialized: () => {} });
  await userEvent.click(screen.getByRole("button", { name: /^Use existing catalog/u }));

  const alert = await screen.findByRole("alert");
  expect(alert.textContent).toBe(message);
});

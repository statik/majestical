import { clearMocks, mockConvertFileSrc, mockIPC } from "@tauri-apps/api/mocks";
import { render, screen } from "@testing-library/svelte";
import { afterEach, beforeEach, expect, test } from "vitest";
import App from "./App.svelte";
import type { AppStatus } from "./lib/api";

beforeEach(() => mockConvertFileSrc("macos"));
afterEach(clearMocks);

function mockStatus(status: AppStatus) {
  mockIPC((cmd) => {
    if (cmd === "app_status") return status;
    if (cmd === "list_saved_searches") return { saved: [] };
    throw new Error(`unexpected command ${cmd}`);
  });
}

test("shell renders", () => {
  mockStatus({ catalog_path: "", catalog_ready: false });
  render(App);
  expect(screen.getByRole("main")).toBeTruthy();
});

test("with no catalog chosen the shell shows the first-run surface", async () => {
  mockStatus({ catalog_path: "", catalog_ready: false });
  render(App);

  expect(
    await screen.findByRole("button", { name: /^Initialize catalog/u }),
  ).toBeTruthy();
  expect(screen.queryByRole("searchbox")).toBeNull();
});

test("with a ready catalog the shell shows the search surface", async () => {
  mockStatus({ catalog_path: "/catalogs/main", catalog_ready: true });
  render(App);

  expect(await screen.findByRole("searchbox")).toBeTruthy();
  expect(
    screen.queryByRole("button", { name: /^Initialize catalog/u }),
  ).toBeNull();
  expect(screen.getByText("/catalogs/main")).toBeTruthy();
});

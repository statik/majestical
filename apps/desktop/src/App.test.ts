import { clearMocks, mockConvertFileSrc, mockIPC } from "@tauri-apps/api/mocks";
import { render, screen, waitFor } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import App from "./App.svelte";
import type { AppStatus, AssetDetail, SearchHit } from "./lib/api";

beforeEach(() => {
  mockConvertFileSrc("macos");
  // The inspector asks the `thumb://` protocol for a keyframe manifest; this
  // app has none for the fixture asset.
  vi.stubGlobal("fetch", () =>
    Promise.resolve({
      ok: false,
      status: 404,
      json: () => Promise.resolve({}),
      text: () => Promise.resolve("no keyframe manifest"),
    } as Response),
  );
});
afterEach(() => {
  clearMocks();
  vi.unstubAllGlobals();
});

const ready: AppStatus = { catalog_path: "/catalogs/main", catalog_ready: true };

const hit: SearchHit = {
  asset: "xxh3:abc123",
  score: 1,
  known: true,
  name: "sunset.mov",
  volumes: [{ id: "label:Card", label: "Card", online: true }],
  tags: [],
  para: null,
};

const detail: AssetDetail = {
  asset: "xxh3:abc123",
  instances: [
    {
      volume: "label:Card",
      volume_label: "Card",
      online: true,
      path: "shoot/day1/sunset.mov",
      size: 2048,
      mtime_ms: 1_700_000_000_000,
    },
  ],
  tags: [],
  para: null,
  fields: {},
  verifications: [],
  has_thumb: false,
};

function mockStatus(status: AppStatus) {
  mockIPC((cmd) => {
    if (cmd === "app_status") return status;
    if (cmd === "list_saved_searches") return { saved: [] };
    throw new Error(`unexpected command ${cmd}`);
  });
}

/** Every command the shell reaches for once a catalog is ready. */
function mockCatalog(counts: { volumes: string[] }) {
  mockIPC((cmd) => {
    if (cmd === "app_status") return ready;
    if (cmd === "list_saved_searches") return { saved: [] };
    if (cmd === "search_assets") return { count: 1, results: [hit] };
    if (cmd === "get_asset") return detail;
    if (cmd === "list_volumes") {
      return {
        volumes: counts.volumes.map((label) => ({
          id: `label:${label}`,
          label,
          last_seen_ms: 1_700_000_000_000,
          online: false,
          asset_count: 3,
          clock_suspect: false,
        })),
      };
    }
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
  mockStatus(ready);
  render(App);

  expect(await screen.findByRole("searchbox")).toBeTruthy();
  expect(
    screen.queryByRole("button", { name: /^Initialize catalog/u }),
  ).toBeNull();
  expect(screen.getByText("/catalogs/main")).toBeTruthy();
});

test("the sidebar swaps surfaces, and says which one you are on", async () => {
  mockCatalog({ volumes: ["Card"] });
  render(App);

  const volumes = await screen.findByRole("button", { name: "Volumes" });
  const search = screen.getByRole("button", { name: "Search" });
  expect(search.getAttribute("aria-current")).toBe("page");
  expect(volumes.getAttribute("aria-current")).toBeNull();

  await userEvent.click(volumes);

  expect(await screen.findByRole("table")).toBeTruthy();
  expect(screen.queryByRole("searchbox")).toBeNull();
  expect(volumes.getAttribute("aria-current")).toBe("page");
  expect(search.getAttribute("aria-current")).toBeNull();

  await userEvent.click(search);

  expect(await screen.findByRole("searchbox")).toBeTruthy();
  expect(screen.queryByRole("table")).toBeNull();
});

test("leaving the search surface closes the inspector with it", async () => {
  mockCatalog({ volumes: ["Card"] });
  const { container } = render(App);

  await userEvent.type(await screen.findByRole("searchbox"), "sunset");
  await userEvent.click(await screen.findByRole("button", { name: /sunset/u }));
  await waitFor(() =>
    expect(container.querySelector(".inspector")).not.toBeNull(),
  );

  // The inspector describes a search result; nothing on the volumes surface
  // can reach an asset, so the panel goes with the surface that opened it.
  await userEvent.click(screen.getByRole("button", { name: "Volumes" }));

  await waitFor(() => expect(container.querySelector(".inspector")).toBeNull());
});

test("a failed startup offers a retry that asks again", async () => {
  const message = "catalog at /catalogs/main is 2 schema versions ahead";
  let calls = 0;
  mockIPC((cmd) => {
    if (cmd === "list_saved_searches") return { saved: [] };
    if (cmd !== "app_status") throw new Error(`unexpected command ${cmd}`);
    calls += 1;
    if (calls === 1) {
      // eslint-disable-next-line prefer-promise-reject-errors -- a rejected command carries the serialized `CommandError`, never an Error instance.
      return Promise.reject({ message });
    }
    return ready;
  });
  render(App);

  const alert = await screen.findByRole("alert");
  expect(alert.textContent).toBe(message);

  await userEvent.click(screen.getByRole("button", { name: "Retry" }));

  expect(await screen.findByRole("searchbox")).toBeTruthy();
  expect(calls).toBe(2);
  expect(screen.queryByRole("alert")).toBeNull();
});

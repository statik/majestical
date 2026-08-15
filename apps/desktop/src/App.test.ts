import { clearMocks, mockConvertFileSrc } from "@tauri-apps/api/mocks";
import { render, screen, waitFor } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import App from "./App.svelte";
import type { AppStatus, AssetDetail, SearchHit } from "./lib/api";
import {
  mockCommands,
  rejectCommand,
  stubManifest,
  stubMatchMedia,
} from "./lib/test-support";

beforeEach(() => {
  mockConvertFileSrc("macos");
  // The browse surface asks how wide the window is; jsdom has no answer.
  stubMatchMedia(false);
  // The inspector asks the `thumb://` protocol for a keyframe manifest; this
  // app has none for the fixture asset.
  stubManifest(404, "no keyframe manifest");
});
afterEach(() => {
  clearMocks();
  vi.unstubAllGlobals();
});

const ready: AppStatus = { catalog_path: "/catalogs/main", catalog_ready: true };

// `UpdateBanner` mounts alongside every surface below and asks the updater
// plugin what is newer. These tests are about the shell, so it is answered
// with "nothing" rather than left to reject — `UpdateBanner.test.ts` is where
// the offer and its failure paths are pinned.
const UPDATE_CHECK = "plugin:updater|check";

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
  mockCommands({
    app_status: () => status,
    [UPDATE_CHECK]: () => null,
    list_saved_searches: () => ({ saved: [] }),
  });
}

/** Every command the shell reaches for once a catalog is ready; the catalog
 *  holds one volume per label given. */
function mockCatalog(volumeLabels: string[]) {
  mockCommands({
    app_status: () => ready,
    [UPDATE_CHECK]: () => null,
    list_saved_searches: () => ({ saved: [] }),
    search_assets: () => ({ count: 1, results: [hit] }),
    get_asset: () => detail,
    browse_tree: () => ({
      volumes: volumeLabels.map((label) => ({
        id: `label:${label}`,
        label,
        online: true,
        folders: [{ path: "", children: [], recursive_count: 1 }],
      })),
    }),
    browse_list: () => ({ count: 1, folder_count: 1, results: [hit] }),
    list_para: () => ({
      nodes: [
        { id: "01PROJECT", kind: "project", name: "client-x", archived: false },
      ],
    }),
    list_tags: () => ({
      tags: [{ tag: "b-roll", count: 412, last_used_ms: 1_754_000_000_000 }],
    }),
    list_volumes: () => ({
      volumes: volumeLabels.map((label) => ({
        id: `label:${label}`,
        label,
        last_seen_ms: 1_700_000_000_000,
        online: false,
        asset_count: 3,
        clock_suspect: false,
      })),
    }),
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
  mockCatalog(["Card"]);
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
  mockCatalog(["Card"]);
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

test("the sidebar offers exactly the surfaces this phase ships, in order", async () => {
  mockCatalog(["Card"]);
  const { container } = render(App);

  await screen.findByRole("searchbox");
  const surfaces = [...container.querySelectorAll(".surfaces button")].map(
    (button) => button.textContent,
  );
  // No dead buttons: Ingest arrives with its own surface.
  expect(surfaces).toEqual(["Search", "Browse", "Organize", "Volumes"]);
});

test("the browse surface swaps in, and takes the inspector's selection with it", async () => {
  mockCatalog(["Card"]);
  const { container } = render(App);

  await userEvent.type(await screen.findByRole("searchbox"), "sunset");
  await userEvent.click(await screen.findByRole("button", { name: /sunset/u }));
  await waitFor(() =>
    expect(container.querySelector(".inspector")).not.toBeNull(),
  );

  const browse = screen.getByRole("button", { name: "Browse" });
  await userEvent.click(browse);

  expect(await screen.findByRole("button", { name: /Card/u })).toBeTruthy();
  expect(screen.queryByRole("searchbox")).toBeNull();
  expect(browse.getAttribute("aria-current")).toBe("page");
  expect(container.querySelector(".inspector")).toBeNull();
});

test("a browse card opens the inspector the same way a search hit does", async () => {
  mockCatalog(["Card"]);
  const { container } = render(App);

  await userEvent.click(await screen.findByRole("button", { name: "Browse" }));
  await userEvent.click(await screen.findByRole("button", { name: /Card/u }));
  await userEvent.click(await screen.findByRole("button", { name: /sunset/u }));

  await waitFor(() =>
    expect(container.querySelector(".inspector")).not.toBeNull(),
  );
});

test("the organize surface swaps in with both of its columns", async () => {
  mockCatalog(["Card"]);
  render(App);

  const organize = await screen.findByRole("button", { name: "Organize" });
  await userEvent.click(organize);

  expect(await screen.findByRole("list", { name: "Projects" })).toBeTruthy();
  expect(screen.getByRole("searchbox", { name: "Filter tags" })).toBeTruthy();
  expect(screen.queryByRole("searchbox", { name: /^Search the catalog/u })).toBeNull();
  expect(organize.getAttribute("aria-current")).toBe("page");
});

test("leaving a surface for Organize closes the inspector with it", async () => {
  mockCatalog(["Card"]);
  const { container } = render(App);

  await userEvent.type(await screen.findByRole("searchbox"), "sunset");
  await userEvent.click(await screen.findByRole("button", { name: /sunset/u }));
  await waitFor(() =>
    expect(container.querySelector(".inspector")).not.toBeNull(),
  );

  // Organize never selects an asset, so the panel goes with the surface
  // that opened it — the same rule every other switch follows.
  await userEvent.click(screen.getByRole("button", { name: "Organize" }));

  await waitFor(() => expect(container.querySelector(".inspector")).toBeNull());
});

test("a failed startup offers a retry that asks again", async () => {
  const message = "catalog at /catalogs/main is 2 schema versions ahead";
  let calls = 0;
  mockCommands({
    list_saved_searches: () => ({ saved: [] }),
    [UPDATE_CHECK]: () => null,
    app_status: () => {
      calls += 1;
      return calls === 1 ? rejectCommand(message) : ready;
    },
  });
  render(App);

  const alert = await screen.findByRole("alert");
  expect(alert.textContent).toBe(message);

  await userEvent.click(screen.getByRole("button", { name: "Retry" }));

  expect(await screen.findByRole("searchbox")).toBeTruthy();
  expect(calls).toBe(2);
  expect(screen.queryByRole("alert")).toBeNull();
});

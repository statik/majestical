import { clearMocks, mockConvertFileSrc, mockIPC } from "@tauri-apps/api/mocks";
import { render, screen, waitFor } from "@testing-library/svelte";
import { afterEach, beforeEach, expect, test } from "vitest";
import type { AssetDetail } from "./api";
import Inspector from "./Inspector.svelte";

beforeEach(() => mockConvertFileSrc("macos"));
afterEach(clearMocks);

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
    {
      volume: "uuid:archive",
      volume_label: "Archive",
      online: false,
      path: "backup/sunset.mov",
      size: 2048,
      mtime_ms: 1_700_000_000_000,
    },
  ],
  tags: ["golden-hour", "keeper"],
  para: "project/pitch-reel",
  fields: { camera: "FX3" },
  verifications: [],
  has_thumb: true,
};

test("no selection renders nothing at all", () => {
  mockIPC((cmd) => {
    throw new Error(`unexpected command ${cmd}`);
  });
  const { container } = render(Inspector, { assetId: null });

  expect(container.textContent).toBe("");
  expect(container.querySelector("*")).toBeNull();
});

test("a selection renders the asset's name, tags, PARA and volume badges", async () => {
  mockIPC((cmd) => {
    if (cmd === "get_asset") return detail;
    throw new Error(`unexpected command ${cmd}`);
  });
  render(Inspector, { assetId: "xxh3:abc123" });

  expect(await screen.findByText("sunset.mov")).toBeTruthy();
  expect(screen.getByText("golden-hour")).toBeTruthy();
  expect(screen.getByText("project/pitch-reel")).toBeTruthy();
  // The CLI's own glyphs: filled for online, hollow for offline.
  expect(screen.getByText("Card●")).toBeTruthy();
  expect(screen.getByText("Archive○")).toBeTruthy();
});

test("an asset the catalog does not know says so", async () => {
  mockIPC((cmd) => {
    if (cmd === "get_asset") return null;
    throw new Error(`unexpected command ${cmd}`);
  });
  render(Inspector, { assetId: "xxh3:gone" });

  await waitFor(() =>
    expect(screen.getByText(/xxh3:gone/u)).toBeTruthy(),
  );
});

test("a failed lookup reports the command's whole message chain", async () => {
  const message = "no catalog selected yet — initialize or choose one first";
  // eslint-disable-next-line prefer-promise-reject-errors -- a rejected command carries the serialized `CommandError`, never an Error instance.
  mockIPC(() => Promise.reject({ message }));
  render(Inspector, { assetId: "xxh3:abc123" });

  const alert = await screen.findByRole("alert");
  expect(alert.textContent).toBe(message);
});

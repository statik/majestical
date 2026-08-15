import { clearMocks, mockConvertFileSrc } from "@tauri-apps/api/mocks";
import { fireEvent, render, screen, waitFor, within } from "@testing-library/svelte";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import type { AssetDetail, AssetVerification } from "./api";
import Inspector from "./Inspector.svelte";
import { mockCommands, rejectCommand, stubManifest } from "./test-support";
import { keyframeImageUrl } from "./thumb";

beforeEach(() => {
  mockConvertFileSrc("macos");
  // Every rendered asset asks the `thumb://` protocol for its keyframe
  // manifest. The default answer is the ordinary one: this asset has none.
  stubManifest(404, "no keyframe manifest for xxh3:abc123");
});
afterEach(() => {
  clearMocks();
  vi.unstubAllGlobals();
});

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

/** The catalog hands verifications back in its own order, not in date order. */
const older: AssetVerification = {
  volume: "uuid:archive",
  path: "backup/sunset.mov",
  algo: "xxh3",
  value: "abc123",
  outcome: "original",
  hashdate_ms: 1_700_000_000_000,
};
const newer: AssetVerification = {
  volume: "label:Card",
  path: "shoot/day1/sunset.mov",
  algo: "xxh3",
  value: "abc123",
  outcome: "verified",
  hashdate_ms: 1_700_086_400_000,
};

function mockAsset(found: AssetDetail | null) {
  mockCommands({ get_asset: () => found });
}

test("no selection renders nothing at all", () => {
  // No handlers at all: any invoke this renders would fail the test by name.
  mockCommands({});
  const { container } = render(Inspector, { assetId: null });

  expect(container.textContent).toBe("");
  expect(container.querySelector("*")).toBeNull();
});

test("a selection renders the asset's name, tags, PARA and volume badges", async () => {
  mockAsset(detail);
  render(Inspector, { assetId: "xxh3:abc123" });

  expect(await screen.findByText("sunset.mov")).toBeTruthy();
  expect(screen.getByText("golden-hour")).toBeTruthy();
  expect(screen.getByText("project/pitch-reel")).toBeTruthy();
  // The CLI's own glyphs: filled for online, hollow for offline.
  expect(screen.getByText("Card●")).toBeTruthy();
  expect(screen.getByText("Archive○")).toBeTruthy();
  // …each naming, for anyone who cannot see the glyph, what it means.
  expect(screen.getByRole("img", { name: "Card online" })).toBeTruthy();
  expect(screen.getByRole("img", { name: "Archive offline" })).toBeTruthy();
});

test("the metadata fields the catalog holds render as name and value", async () => {
  mockAsset({ ...detail, fields: { camera: "FX3", lens: "24mm" } });
  render(Inspector, { assetId: "xxh3:abc123" });

  expect(await screen.findByText("camera")).toBeTruthy();
  expect(screen.getByText("FX3")).toBeTruthy();
  expect(screen.getByText("lens")).toBeTruthy();
  expect(screen.getByText("24mm")).toBeTruthy();
});

test("a notice repeated in one detail renders twice, detail intact", async () => {
  const notice = "warning: skipped 1 corrupt event log line(s) in /x/events";
  mockAsset({ ...detail, notices: [notice, notice] });
  render(Inspector, { assetId: "xxh3:abc123" });

  await waitFor(() => expect(screen.getAllByText(notice)).toHaveLength(2));
  expect(screen.getByText("sunset.mov")).toBeTruthy();
  expect(screen.getByText("golden-hour")).toBeTruthy();
});

test("an asset the catalog does not know says so", async () => {
  mockAsset(null);
  render(Inspector, { assetId: "xxh3:gone" });

  await waitFor(() => expect(screen.getByText(/xxh3:gone/u)).toBeTruthy());
});

test("a failed lookup reports the command's whole message chain", async () => {
  const message = "no catalog selected yet — initialize or choose one first";
  mockCommands({ get_asset: () => rejectCommand(message) });
  render(Inspector, { assetId: "xxh3:abc123" });

  const alert = await screen.findByRole("alert");
  expect(alert.textContent).toBe(message);
});

/** Collapses the whitespace Svelte leaves between interpolations. */
function line(node: Element | null): string {
  return (node?.textContent ?? "").replaceAll(/\s+/gu, " ").trim();
}

test("verify state shows the most recent verification, however it arrived", async () => {
  // The catalog hands these back in its own order, newest last here — so an
  // implementation that took the array's first entry would report the 14th
  // as this asset's current state.
  mockAsset({ ...detail, verifications: [older, newer] });
  const { container } = render(Inspector, { assetId: "xxh3:abc123" });

  await waitFor(() =>
    expect(line(container.querySelector(".verify-state"))).toBe(
      "verified · 2023-11-15",
    ),
  );
});

test("the whole verification history hides behind a details element", async () => {
  mockAsset({ ...detail, verifications: [older, newer] });
  const { container } = render(Inspector, { assetId: "xxh3:abc123" });

  await waitFor(() => expect(container.querySelector("details")).not.toBeNull());
  const details = container.querySelector("details") as HTMLElement;
  expect(within(details).getByText(/Full history \(2\)/u)).toBeTruthy();
  // Latest first inside the history too, each naming the copy it hashed.
  const entries = [...details.querySelectorAll("li")].map((node) => line(node));
  expect(entries).toEqual([
    "verified · 2023-11-15 · shoot/day1/sunset.mov",
    "original · 2023-11-14 · backup/sunset.mov",
  ]);
});

test("an asset nobody has verified says so, with no history to open", async () => {
  mockAsset(detail);
  const { container } = render(Inspector, { assetId: "xxh3:abc123" });

  expect(await screen.findByText("Never verified")).toBeTruthy();
  expect(container.querySelector("details")).toBeNull();
});

test("the keyframe strip renders one image per timestamp, addressed by index", async () => {
  mockAsset(detail);
  stubManifest(
    200,
    '{"model_tag":"siglip2-b16-v1","detected":2,"timestamps":[1500,65500]}',
  );
  render(Inspector, { assetId: "xxh3:abc123" });

  // `@MmSSs`, the timecode `maj search` prints for a keyframe hit — carried
  // as the accessible name (`alt`) of each strip image.
  const first = await screen.findByRole("img", { name: "@0m01s" });
  const second = screen.getByRole("img", { name: "@1m05s" });
  // The literal URL `mockConvertFileSrc("macos")` produces for
  // `keyframe/xxh3:abc123/0` — spelled out rather than comparing against
  // `keyframeImageUrl(...)` itself, which would just check the component
  // calls the same function this assertion also calls.
  expect(first.getAttribute("src")).toBe(
    "thumb://localhost/keyframe%2Fxxh3%3Aabc123%2F0",
  );
  expect(second.getAttribute("src")).toBe(keyframeImageUrl("xxh3:abc123", 1));
  // Every detected keyframe was indexed: there is no gap to report.
  expect(screen.queryByText(/detected keyframes indexed/u)).toBeNull();
});

test("a strip image that fails to load falls back to the timecode chip", async () => {
  mockAsset(detail);
  stubManifest(
    200,
    '{"model_tag":"siglip2-b16-v1","detected":1,"timestamps":[1500]}',
  );
  render(Inspector, { assetId: "xxh3:abc123" });

  const image = await screen.findByRole("img", { name: "@0m01s" });
  await fireEvent.error(image);

  expect(screen.queryByRole("img", { name: "@0m01s" })).toBeNull();
  expect(screen.getByText("@0m01s")).toBeTruthy();
});

test("switching to a different asset resets the previous asset's failed-image fallback", async () => {
  const second: AssetDetail = { ...detail, asset: "xxh3:def456" };
  mockCommands({
    get_asset: (args) => {
      const record = args as Record<string, unknown> | undefined;
      return record?.["assetId"] === "xxh3:def456" ? second : detail;
    },
  });
  stubManifest(
    200,
    '{"model_tag":"siglip2-b16-v1","detected":1,"timestamps":[1500]}',
  );
  const { rerender } = render(Inspector, { assetId: "xxh3:abc123" });

  const firstImage = await screen.findByRole("img", { name: "@0m01s" });
  await fireEvent.error(firstImage);
  expect(screen.queryByRole("img", { name: "@0m01s" })).toBeNull();
  expect(screen.getByText("@0m01s")).toBeTruthy();

  await rerender({ assetId: "xxh3:def456" });

  // The second asset's own strip image has never failed — a stale
  // `failedKeyframes` left over from the first asset would still show the
  // timecode chip here even though nothing about THIS image has 404'd.
  expect(await screen.findByRole("img", { name: "@0m01s" })).toBeTruthy();
});

test("an asset with no keyframe manifest shows no strip", async () => {
  mockAsset(detail);
  render(Inspector, { assetId: "xxh3:abc123" });

  // The 404 is the ordinary answer for a still; it is not a failure and must
  // not put anything on the panel.
  await screen.findByText("sunset.mov");
  await waitFor(() => expect(screen.getByText("Never verified")).toBeTruthy());
  expect(screen.queryByRole("img", { name: /^@/u })).toBeNull();
  expect(screen.queryByText(/^@/u)).toBeNull();
  expect(screen.queryByText(/no keyframe manifest/u)).toBeNull();
});

test("a manifest listing fewer keyframes than were detected says how many", async () => {
  mockAsset(detail);
  stubManifest(
    200,
    '{"model_tag":"siglip2-b16-v1","detected":5,"timestamps":[1500,65500]}',
  );
  render(Inspector, { assetId: "xxh3:abc123" });

  expect(
    await screen.findByText("2 of 5 detected keyframes indexed"),
  ).toBeTruthy();
});

test("a manifest that cannot be read reports why without hiding the asset", async () => {
  mockAsset(detail);
  stubManifest(503, "no catalog selected yet — initialize or choose one first");
  render(Inspector, { assetId: "xxh3:abc123" });

  expect(await screen.findByText(/no catalog selected yet/u)).toBeTruthy();
  expect(screen.getByText("sunset.mov")).toBeTruthy();
});

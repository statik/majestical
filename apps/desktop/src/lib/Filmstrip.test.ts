import { clearMocks, mockConvertFileSrc } from "@tauri-apps/api/mocks";
import { fireEvent, render, screen, waitFor } from "@testing-library/svelte";
import { createRawSnippet } from "svelte";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import Filmstrip from "./Filmstrip.svelte";
import { stubManifest } from "./test-support";
import { keyframeImageUrl } from "./thumb";

beforeEach(() => mockConvertFileSrc("macos"));
afterEach(() => {
  clearMocks();
  vi.unstubAllGlobals();
});

const ASSET = "xxh3:abc123";

/** The static thumb a card hands the filmstrip to wrap. */
const thumb = createRawSnippet(() => ({
  render: () => `<img class="thumb" alt="" src="thumb://localhost/thumb" />`,
}));

/** A manifest of four keyframes, half a minute apart. */
const FOUR = JSON.stringify({
  model_tag: "scene-v1",
  detected: 4,
  timestamps: [0, 30_000, 60_000, 90_000],
});

/** The drawn keyframe. Queried by class, not by role: the whole overlay is
 *  `aria-hidden`, so nothing it draws answers a role query. */
function frameOf(container: HTMLElement): HTMLImageElement | null {
  return container.querySelector(".browse-frame");
}

/** The drawn keyframe, once the manifest has arrived and placed one. */
async function findFrame(container: HTMLElement): Promise<HTMLImageElement> {
  await waitFor(() => expect(frameOf(container)).not.toBeNull());
  return frameOf(container) as HTMLImageElement;
}

/**
 * jsdom lays nothing out, so every `getBoundingClientRect` is zeros and an
 * x-fraction is undefined. This gives the hover area the 100px width the
 * fractions below are read against.
 */
function widen(container: HTMLElement): HTMLElement {
  const film = container.querySelector(".browse-film") as HTMLElement;
  vi.spyOn(film, "getBoundingClientRect").mockReturnValue({
    left: 0,
    width: 100,
  } as DOMRect);
  return film;
}

test("an asset with no keyframes stays a plain thumb, hover or not", async () => {
  stubManifest(404, "no keyframe manifest");
  const { container } = render(Filmstrip, { assetId: ASSET, children: thumb });
  const film = widen(container);

  await fireEvent.pointerEnter(film);
  await fireEvent.pointerMove(film, { clientX: 60 });

  expect(container.querySelector(".browse-frame")).toBeNull();
  expect(container.querySelector(".browse-scrub")).toBeNull();
  expect(container.querySelector(".thumb")).not.toBeNull();
});

test("mouse-x picks the keyframe under it, with its timecode", async () => {
  stubManifest(200, FOUR);
  const { container } = render(Filmstrip, { assetId: ASSET, children: thumb });
  const film = widen(container);

  await fireEvent.pointerEnter(film);
  // 60% of four keyframes is the third: floor(0.6 * 4) === 2.
  await fireEvent.pointerMove(film, { clientX: 60 });

  const frame = await findFrame(container);
  expect(frame.getAttribute("src")).toBe(keyframeImageUrl(ASSET, 2));
  expect(screen.getByText("@1m00s")).toBeTruthy();
  // Drawn, but out of the accessibility tree: the enclosing card button
  // takes its name from its contents, and a timecode that joined that name
  // would rename the button on every pixel the pointer travels.
  // `BrowseView.grid.test.ts` pins that consequence on a real card.
  expect(
    container.querySelector(".browse-overlay")?.getAttribute("aria-hidden"),
  ).toBe("true");
  // One bar per keyframe, the one under the pointer marked.
  const bars = container.querySelectorAll(".browse-scrub i");
  expect(bars).toHaveLength(4);
  expect([...bars].findIndex((bar) => bar.classList.contains("pos"))).toBe(2);
});

test("the last keyframe is reachable at the far right edge", async () => {
  stubManifest(200, FOUR);
  const { container } = render(Filmstrip, { assetId: ASSET, children: thumb });
  const film = widen(container);

  await fireEvent.pointerEnter(film);
  // The fraction is 1.0 here, which indexes one past the last timestamp
  // without the clamp.
  await fireEvent.pointerMove(film, { clientX: 100 });

  const frame = await findFrame(container);
  expect(frame.getAttribute("src")).toBe(keyframeImageUrl(ASSET, 3));
});

test("leaving the card puts the static thumb back", async () => {
  stubManifest(200, FOUR);
  const { container } = render(Filmstrip, { assetId: ASSET, children: thumb });
  const film = widen(container);

  await fireEvent.pointerEnter(film);
  await fireEvent.pointerMove(film, { clientX: 60 });
  await findFrame(container);

  await fireEvent.pointerLeave(film);

  expect(container.querySelector(".browse-frame")).toBeNull();
  expect(container.querySelector(".browse-scrub")).toBeNull();
  expect(container.querySelector(".thumb")).not.toBeNull();
});

test("a keyframe image that 404s drops the scrub for this hover", async () => {
  // Extraction hasn't reached this timestamp yet: the manifest lists it, the
  // image behind it isn't there. Falling back for the whole hover keeps a
  // pointermove per pixel from retrying an image that is not coming.
  stubManifest(200, FOUR);
  const { container } = render(Filmstrip, { assetId: ASSET, children: thumb });
  const film = widen(container);

  await fireEvent.pointerEnter(film);
  await fireEvent.pointerMove(film, { clientX: 60 });
  const frame = await findFrame(container);

  await fireEvent.error(frame);

  expect(container.querySelector(".browse-frame")).toBeNull();
  expect(container.querySelector(".thumb")).not.toBeNull();

  await fireEvent.pointerMove(film, { clientX: 20 });
  expect(container.querySelector(".browse-frame")).toBeNull();
});

test("the manifest is fetched once, however far the pointer travels", async () => {
  stubManifest(200, FOUR);
  const fetches = vi.spyOn(globalThis, "fetch");
  const { container } = render(Filmstrip, { assetId: ASSET, children: thumb });
  const film = widen(container);

  await fireEvent.pointerEnter(film);
  await fireEvent.pointerMove(film, { clientX: 10 });
  await fireEvent.pointerMove(film, { clientX: 60 });
  await fireEvent.pointerLeave(film);
  await fireEvent.pointerEnter(film);
  await fireEvent.pointerMove(film, { clientX: 80 });

  await findFrame(container);
  expect(fetches).toHaveBeenCalledTimes(1);
});

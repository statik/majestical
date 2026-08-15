<script lang="ts">
  /**
   * Hover-scrub over one video's keyframes: the card's static thumbnail until
   * the pointer is on it, then the frame nearest the pointer's x-fraction,
   * with the timecode it sits at and a bar per keyframe showing where in the
   * clip that is.
   *
   * The manifest is read once, on the first hover — a grid of a hundred
   * cards must not ask the `thumb://` protocol for a hundred manifests
   * nobody looked at. A video with no manifest (a still, or a clip nothing
   * has indexed keyframes for yet) is a plain thumbnail forever: the scrub
   * affordance never appears, rather than appearing and doing nothing.
   *
   * One instance reads one asset's manifest, once: the grid keys its cards
   * by asset id, so a card that is still on screen is still the same asset.
   *
   * No transition, here or in the sheet: the frames are the content, and
   * swapping which one is shown is not motion — nothing about this needs
   * `prefers-reduced-motion`.
   */
  import type { Snippet } from "svelte";
  import { timecode } from "./format";
  import { fetchKeyframes, keyframeImageUrl } from "./thumb";
  import type { KeyframeManifest } from "./thumb";

  let {
    assetId,
    children,
  }: { assetId: string; children: Snippet } = $props();

  let manifest = $state<KeyframeManifest | null>(null);
  /** Whether the manifest read has happened at all — distinct from a null
   *  manifest, which is the answer "this asset has no keyframes". */
  let asked = false;
  /** Which keyframe the pointer is over, null whenever it is off the card. */
  let frame = $state<number | null>(null);
  /**
   * Set when the frame `<img>` fails: extraction runs after detection, so a
   * manifest can list a timestamp whose image is not there yet. Cleared on
   * the next hover, not on the next pointermove — a move per pixel would
   * otherwise retry an image that is not coming, all the way across the card.
   */
  let broken = $state(false);

  let position = $derived(
    manifest !== null && frame !== null && !broken ? frame : null,
  );

  async function readManifest() {
    if (asked) return;
    asked = true;
    try {
      manifest = await fetchKeyframes(assetId);
    } catch {
      // The manifest rides `thumb://`, not IPC, and this is a decoration on
      // a card: an unreadable manifest leaves the plain thumbnail, and the
      // inspector is where that failure is reported in words.
      manifest = null;
    }
  }

  function enter() {
    broken = false;
    void readManifest();
  }

  function scrub(event: PointerEvent) {
    if (manifest === null || broken) return;
    const total = manifest.timestamps.length;
    if (total === 0) return;
    const box = (event.currentTarget as HTMLElement).getBoundingClientRect();
    if (box.width <= 0) return;
    const fraction = Math.min(
      1,
      Math.max(0, (event.clientX - box.left) / box.width),
    );
    // The right edge is fraction 1.0, which indexes one past the last frame.
    frame = Math.min(total - 1, Math.floor(fraction * total));
  }

  function leave() {
    frame = null;
  }
</script>

<!-- `role="presentation"`: the hover area adds nothing to the accessibility
     tree that the card's own button and name do not already say, and hovering
     is not something a keyboard reaches — the inspector is where the same
     keyframes are browsable without a pointer. -->
<span
  class="browse-film"
  role="presentation"
  onpointerenter={enter}
  onpointermove={scrub}
  onpointerleave={leave}
>
  {@render children()}
  {#if manifest !== null && position !== null}
    <!-- `aria-hidden`, and on the wrapper rather than the pieces: a
         presentational role does not hide what is inside it, so without
         this the frame and its timecode would join the enclosing card
         button's accessible name — and rename that button under the reader
         on every pixel of pointer travel. -->
    <span class="browse-overlay" aria-hidden="true">
      <img
        class="browse-frame"
        src={keyframeImageUrl(assetId, position)}
        alt=""
        onerror={() => (broken = true)}
      />
      <span class="browse-tc"
        >{timecode(manifest.timestamps[position] ?? 0)}</span
      >
      <!-- One bar per keyframe, in manifest order — unkeyed for the same
           reason the inspector's strip is: the manifest's bytes are served
           verbatim, so two timestamps can repeat and a keyed block would
           throw. -->
      <span class="browse-scrub">
        {#each manifest.timestamps as _ts, index}
          <i class:pos={index === position}></i>
        {/each}
      </span>
    </span>
  {/if}
</span>

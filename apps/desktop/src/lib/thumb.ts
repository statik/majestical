// URLs for the `thumb://` protocol `src-tauri/src/thumb_protocol.rs` serves:
// image bytes reach the webview as a normal resource load, never over IPC.
import { convertFileSrc } from "@tauri-apps/api/core";

/** The asset's WebP thumbnail. */
export const thumbUrl = (assetId: string) =>
  convertFileSrc(`thumb/${assetId}`, "thumb");

/** The asset's keyframe manifest (JSON). */
export const keyframesUrl = (assetId: string) =>
  convertFileSrc(`keyframes/${assetId}`, "thumb");

/**
 * The keyframe manifest body, field-for-field from
 * `crates/services/src/index/run.rs::keyframes_manifest_json`. `detected` is
 * the video's full scene-detected count and `timestamps` only the keyframes
 * that were indexed successfully, so the two differ when a frame permanently
 * failed to extract — the gap is the manifest's own audit trail.
 */
export interface KeyframeManifest {
  model_tag: string;
  detected: number;
  timestamps: number[];
}

/**
 * Reads one asset's keyframe manifest over the `thumb://` protocol. Returns
 * null for the ordinary 404 — a still, or a video nobody has run `maj index
 * run --kinds keyframes` over yet.
 *
 * @throws the protocol's own reason text for any other failure: the remedy
 * is already in the body (`thumb_protocol::failure`), so it is thrown whole.
 */
export async function fetchKeyframes(
  assetId: string,
): Promise<KeyframeManifest | null> {
  const response = await fetch(keyframesUrl(assetId));
  if (response.status === 404) return null;
  if (!response.ok) throw new Error(await response.text());
  return (await response.json()) as KeyframeManifest;
}

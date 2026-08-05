// URLs for the `thumb://` protocol `src-tauri/src/thumb_protocol.rs` serves:
// image bytes reach the webview as a normal resource load, never over IPC.
import { convertFileSrc } from "@tauri-apps/api/core";

/** The asset's WebP thumbnail. */
export const thumbUrl = (assetId: string) =>
  convertFileSrc(`thumb/${assetId}`, "thumb");

/** The asset's keyframe manifest (JSON). */
export const keyframesUrl = (assetId: string) =>
  convertFileSrc(`keyframes/${assetId}`, "thumb");

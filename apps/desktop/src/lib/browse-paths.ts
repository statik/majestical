// The browse tree's path arithmetic: turning a volume and a `/`-separated
// folder path into the child paths, breadcrumb prefixes and node keys the
// surface addresses its tree by. Pure functions over the flat
// `BrowseVolume.folders` list the backend sends — no component state, which
// is why they are unit-tested here rather than through a rendered surface.
import type { BrowseFolder, BrowseVolume } from "./api";

/** The folder at `at` on this volume, or null. The tree arrives flat and is
 *  nested client-side by path, so every lookup goes through here. */
export function folderAt(vol: BrowseVolume, at: string): BrowseFolder | null {
  return vol.folders.find((folder) => folder.path === at) ?? null;
}

/** The path of `name` directly under `parent`; the volume root is "". */
export function childPath(parent: string, name: string): string {
  return parent === "" ? name : `${parent}/${name}`;
}

/** The path the breadcrumb's `index`th segment names: everything up to and
 *  including it. */
export function crumbPath(crumbs: string[], index: number): string {
  return crumbs.slice(0, index + 1).join("/");
}

/**
 * One tree node's identity. A folder path is unique per volume, not across
 * the catalog, so the volume id is half of the key — length-prefixed rather
 * than joined on a separator, because every printable character is legal
 * inside both halves. `12:label:SSD-A/A` can be read as no other pair, where
 * a `/`-joined `label:SSD-A/A` is also the volume `label:SSD` at `A/A`.
 */
export function nodeKey(volumeId: string, at: string): string {
  return `${volumeId.length}:${volumeId}/${at}`;
}

// The tag column's near-duplicate hint. A pure function over the tag list
// rather than a block inside `OrganizeView.svelte`, for the same reason
// `browse-paths.ts` exists: what it computes is worth testing on its own,
// without a component around it.
import type { TagRow } from "./api";

/** Case, hyphens, underscores and spaces folded away — everything else is
 *  left alone, so this stays an exact match on a normalized spelling and
 *  never becomes a fuzzy one. Unicode is composed first: macOS hands out
 *  decomposed filenames, so a tag typed as `café` and one carried in from a
 *  path as `cafe` + a combining accent are the same word and have to fold
 *  to the same key. */
function normalize(tag: string): string {
  return tag.normalize("NFC").toLowerCase().replaceAll(/[-_ ]/gu, "");
}

/**
 * For every tag that has a near-duplicate, the tag to point at with the ≈
 * marker: two tags that normalize to the same form are one word spelled two
 * ways ("golden-hour" / "goldenhour"), and each names the other so a merge
 * target is findable from either row.
 *
 * A tag with no near-duplicate is absent from the map. In a group of three
 * or more spellings each names the next one, so every row still points at a
 * real merge target and following the hints walks the whole group — a
 * "points at the first one" rule would instead leave that first row naming
 * only one of the several tags it collides with.
 *
 * Client-side by design (mockup note 3): no service verb computes this, and
 * the tag list the surface already holds is everything it needs.
 */
export function nearDuplicates(tags: TagRow[]): Map<string, string> {
  const groups = new Map<string, string[]>();
  for (const row of tags) {
    const key = normalize(row.tag);
    const group = groups.get(key);
    if (group === undefined) {
      groups.set(key, [row.tag]);
    } else {
      group.push(row.tag);
    }
  }
  const hints = new Map<string, string>();
  for (const group of groups.values()) {
    if (group.length < 2) continue;
    for (const [index, tag] of group.entries()) {
      const other = group[(index + 1) % group.length];
      if (other !== undefined) hints.set(tag, other);
    }
  }
  return hints;
}

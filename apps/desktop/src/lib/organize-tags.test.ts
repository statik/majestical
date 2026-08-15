import { expect, test } from "vitest";
import type { TagRow } from "./api";
import { nearDuplicates } from "./organize-tags";

/** A tag row; only the name matters to the hint. */
function tag(name: string): TagRow {
  return { tag: name, count: 1, last_used_ms: 1_700_000_000_000 };
}

test("two spellings of one word point at each other", () => {
  const hints = nearDuplicates([tag("golden-hour"), tag("goldenhour")]);

  expect(hints.get("golden-hour")).toBe("goldenhour");
  expect(hints.get("goldenhour")).toBe("golden-hour");
});

test("two different words are not near-duplicates", () => {
  const hints = nearDuplicates([tag("drone"), tag("b-roll")]);

  expect(hints.size).toBe(0);
});

test("case, underscores and spaces are all folded away", () => {
  const hints = nearDuplicates([tag("Golden Hour"), tag("golden_hour")]);

  expect(hints.get("Golden Hour")).toBe("golden_hour");
});

test("a hyphen is not the same word as no hyphen plus another letter", () => {
  // `b-roll` folds to `broll`, which `brol` is not — the fold must not
  // become a fuzzy match, or every short tag would hint at its neighbours.
  const hints = nearDuplicates([tag("b-roll"), tag("brol")]);

  expect(hints.size).toBe(0);
});

test("three spellings of one word each name a real merge target", () => {
  const hints = nearDuplicates([
    tag("golden-hour"),
    tag("goldenhour"),
    tag("Golden_Hour"),
  ]);

  // Every row hints, no row hints at itself, and following the hints walks
  // the whole group — so a merge started from any of the three reaches the
  // rest.
  expect(new Set(hints.keys())).toEqual(
    new Set(["golden-hour", "goldenhour", "Golden_Hour"]),
  );
  for (const [from, to] of hints) {
    expect(to).not.toBe(from);
  }
  expect(new Set(hints.values()).size).toBe(3);
});

test("a composed accent and a decomposed one are the same word", () => {
  // macOS hands out decomposed filenames, so the same tag can arrive spelled
  // both ways and has to fold to one key.
  // Two different JS strings, one word: composed é, and e + a combining
  // accent.
  const hints = nearDuplicates([tag("caf\u00E9"), tag("cafe\u0301")]);

  expect(hints.get("caf\u00E9")).toBe("cafe\u0301");
  expect(hints.get("cafe\u0301")).toBe("caf\u00E9");
});

test("a lone tag has nothing to point at", () => {
  const hints = nearDuplicates([tag("interview")]);

  expect(hints.size).toBe(0);
});

// The date, size and timecode spellings the surfaces share, so a keyframe chip
// in the inspector and a keyframe hit on the search surface can never drift
// into two different timecodes for the same millisecond — nor the inspector's
// header and a browse card into two different sizes for the same file.

/** The ISO day, the spelling `maj volumes list` and `maj asset` both print. */
export function isoDay(ms: number): string {
  return new Date(ms).toISOString().slice(0, 10);
}

/** `@MmSSs`, the timecode `maj search` prints for a keyframe hit. */
export function timecode(ms: number): string {
  const minutes = Math.floor(ms / 60_000);
  const seconds = Math.floor((ms % 60_000) / 1000);
  return `@${minutes}m${String(seconds).padStart(2, "0")}s`;
}

/**
 * `MM:SS`, and `H:MM:SS` once a run passes the hour — how long a copy has
 * been going and how much longer it is estimated to take, spelled the same
 * way for both so the two halves of that line cannot disagree.
 */
export function duration(ms: number): string {
  const total = Math.max(0, Math.floor(ms / 1000));
  const minutes = String(Math.floor(total / 60) % 60).padStart(2, "0");
  const seconds = String(total % 60).padStart(2, "0");
  const hours = Math.floor(total / 3600);
  return hours > 0 ? `${hours}:${minutes}:${seconds}` : `${minutes}:${seconds}`;
}

const UNITS = ["B", "KB", "MB", "GB", "TB", "PB"];

/** Binary units, one decimal place above bytes — the inspector's header and
 *  the browse grid's sub-line print the same number for the same file. */
export function fileSize(bytes: number): string {
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < UNITS.length - 1) {
    value /= 1024;
    unit += 1;
  }
  const rounded = unit === 0 ? String(value) : value.toFixed(1);
  return `${rounded} ${UNITS[unit] ?? "B"}`;
}

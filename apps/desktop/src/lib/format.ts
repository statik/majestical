// The date and timecode spellings the surfaces share, so a keyframe chip in
// the inspector and a keyframe hit on the search surface can never drift into
// two different timecodes for the same millisecond.

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

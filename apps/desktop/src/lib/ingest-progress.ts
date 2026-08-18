// What one ingest run has done so far, accumulated from the progress events
// the engine emits. A module of its own, and pure, for the same reason
// `selection.ts` is: this is the arithmetic behind every number the run card
// shows, and it is worth pinning without a component around it.
//
// It is NOT the authority on a finished run. The end-of-run sweep can demote
// a file already announced as `file_placed`, and that demotion appears only
// in the run's `Outcome` — so the completion card is drawn from `IngestRun`,
// never from one of these.
import type { ProgressEvent } from "./api";
import { duration } from "./format";

/** A file between its `file_started` and its `file_placed`/`file_failed`. */
export interface CopyingFile {
  rel: string;
  /** The size `file_started` announced. */
  size: number;
  /** The cumulative source bytes its last `bytes_copied` reported. */
  done: number;
}

export interface FileFailure {
  rel: string;
  reason: string;
}

export interface RunProgress {
  /**
   * Whether `run_started` was seen. A surface that joined a run mid-flight
   * missed it, and has no totals at all — which is a different thing to
   * say than totals of zero.
   */
  totalsKnown: boolean;
  filesTotal: number;
  bytesTotal: number;
  placed: number;
  failed: number;
  failures: FileFailure[];
  copying: CopyingFile[];
  /**
   * Bytes belonging to files that have left `copying`, kept as one number
   * rather than a row each: a 200k-file card must not leave 200k rows
   * behind to add up on every repaint.
   */
  settledBytes: number;
  /** How many files each destination root has verified, by root. */
  verified: Record<string, number>;
}

/** A run nothing has been heard about yet. */
export function noProgress(): RunProgress {
  return {
    totalsKnown: false,
    filesTotal: 0,
    bytesTotal: 0,
    placed: 0,
    failed: 0,
    failures: [],
    copying: [],
    settledBytes: 0,
    verified: {},
  };
}

/**
 * Moves a finished file's bytes out of the in-flight rows. A placed file was
 * read end to end whatever its last chunk said; a failed one only got as far
 * as it got. A file nobody saw start (this surface joined mid-run)
 * contributes nothing rather than a guess.
 */
function settle(state: RunProgress, rel: string, whole: boolean): RunProgress {
  const row = state.copying.find((file) => file.rel === rel);
  return {
    ...state,
    settledBytes: state.settledBytes + ((whole ? row?.size : row?.done) ?? 0),
    copying: state.copying.filter((file) => file.rel !== rel),
  };
}

/** The state after one more event. `run_stopped` changes nothing here: it is
 *  a phase the surface moves into, not a number. */
export function applyProgress(
  state: RunProgress,
  event: ProgressEvent,
): RunProgress {
  switch (event.type) {
    case "run_started":
      return {
        ...state,
        totalsKnown: true,
        filesTotal: event.files_total,
        bytesTotal: event.bytes_total,
      };
    case "file_started":
      return {
        ...state,
        copying: [
          ...state.copying.filter((file) => file.rel !== event.rel),
          { rel: event.rel, size: event.size, done: 0 },
        ],
      };
    case "bytes_copied":
      return {
        ...state,
        copying: state.copying.map((file) =>
          file.rel === event.rel ? { ...file, done: event.bytes_done } : file,
        ),
      };
    case "file_verified":
      return {
        ...state,
        verified: {
          ...state.verified,
          [event.dest_root]: (state.verified[event.dest_root] ?? 0) + 1,
        },
      };
    case "file_placed":
      return { ...settle(state, event.rel, true), placed: state.placed + 1 };
    case "file_failed":
      return {
        ...settle(state, event.rel, false),
        failed: state.failed + 1,
        failures: [...state.failures, { rel: event.rel, reason: event.reason }],
      };
    case "run_stopped":
      return state;
  }
}

/** Source bytes read so far: the files that finished, plus how far the ones
 *  in flight have got. Every byte is counted once however many destinations
 *  it was fanned out to, because that is what `bytes_copied` reports. */
export function bytesDone(state: RunProgress): number {
  return state.copying.reduce((sum, file) => sum + file.done, state.settledBytes);
}

/**
 * The bar, clamped. `bytes_total` is a plan-time sum while every
 * `bytes_copied` is measured at copy time, so a source that changed between
 * planning and copying legitimately overshoots — a bar past 100% would be
 * the surface reporting a backend bug that isn't one.
 */
export function barPercent(state: RunProgress): number {
  if (state.bytesTotal <= 0) return 0;
  return Math.min(100, Math.round((bytesDone(state) / state.bytesTotal) * 100));
}

/** How far one in-flight file has got, as a whole percent. A file whose
 *  `file_started` announced no size cannot have a fraction, so it reads 0
 *  rather than dividing by zero. */
export function filePercent(file: CopyingFile): number {
  if (file.size <= 0) return 0;
  return Math.min(100, Math.round((file.done / file.size) * 100));
}

/**
 * How much longer the run is estimated to take, from the rate it has
 * managed so far — or null when there is nothing honest to say, which is
 * every one of: no `run_started` (so no total), no elapsed time yet, no
 * bytes copied yet (no rate), and a run already past its plan-time total.
 * A null omits the estimate; it is never rendered as "0:00 left".
 */
export function remainingMs(
  state: RunProgress,
  elapsedMs: number | null,
): number | null {
  if (elapsedMs === null || elapsedMs <= 0) return null;
  if (!state.totalsKnown || state.bytesTotal <= 0) return null;
  const done = bytesDone(state);
  const left = state.bytesTotal - done;
  if (done <= 0 || left <= 0) return null;
  return Math.round((left / done) * elapsedMs);
}

/** The rows the destination tallies draw: the roots chosen for this run,
 *  then any root only the events know about — which is all a surface that
 *  joined a run mid-flight has. */
export function destRoots(state: RunProgress, chosen: string[]): string[] {
  return [
    ...chosen,
    ...Object.keys(state.verified).filter((root) => !chosen.includes(root)),
  ];
}

/**
 * "elapsed 06:12 · about 08:40 left" — the line an operator actually
 * watches. The estimate is left off entirely when `remainingMs` had nothing
 * honest to give, rather than rendered as a fabricated "00:00 left".
 */
export function timingLine(
  elapsedMs: number | null,
  remaining: number | null,
): string | null {
  if (elapsedMs === null) return null;
  const line = `elapsed ${duration(elapsedMs)}`;
  return remaining === null ? line : `${line} · about ${duration(remaining)} left`;
}

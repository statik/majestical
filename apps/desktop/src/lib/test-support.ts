// Mocking helpers shared by the component suites. Every surface talks to the
// backend through the same two channels — `invoke` for commands and `fetch`
// for the `thumb://` protocol — so the stubs for both live here rather than
// once per file.
import type { InvokeArgs } from "@tauri-apps/api/core";
import { mockIPC } from "@tauri-apps/api/mocks";
import { vi } from "vitest";

/** What one command answers with; a promise is fine, and so is a rejection. */
type CommandHandler = (args?: InvokeArgs) => unknown;

/**
 * Answers exactly the commands named and nothing else: an invoke this test did
 * not plan for throws by name instead of resolving to `undefined` and leaving
 * the surface to fail somewhere further along.
 */
export function mockCommands(handlers: Record<string, CommandHandler>): void {
  mockIPC((cmd, args) => {
    const handler = handlers[cmd];
    if (handler === undefined) {
      throw new Error(`unexpected command ${cmd}`);
    }
    return handler(args);
  });
}

/**
 * The rejection a failing command produces: `commands::CommandError`
 * serialized, which is a plain object and never an `Error`. Returned rather
 * than installed, so it composes into a `mockCommands` handler. `notices`
 * mirrors the Rust side's `skip_serializing_if`: omitted from the object
 * entirely when empty, never present as `[]`.
 */
export function rejectCommand(
  message: string,
  notices: string[] = [],
): Promise<never> {
  // eslint-disable-next-line prefer-promise-reject-errors -- a rejected command carries the serialized `CommandError`, never an Error instance.
  return Promise.reject(
    notices.length > 0 ? { message, notices } : { message },
  );
}

/**
 * Answers the keyframe-manifest `fetch` with one canned response. jsdom has no
 * fetch stack, so the slice of `Response` the manifest reader uses is built by
 * hand; `body` is the text a real protocol handler would return, parsed as
 * JSON only when the reader asks for it.
 */
export function stubManifest(status: number, body: string): void {
  const response = {
    ok: status >= 200 && status < 300,
    status,
    json: () => Promise.resolve(JSON.parse(body) as unknown),
    text: () => Promise.resolve(body),
  } as Response;
  vi.stubGlobal("fetch", () => Promise.resolve(response));
}

// The in-app update check, which is best-effort in the strongest sense:
// nothing in this module throws, and every reason there is no update to offer
// looks the same from the outside — no release published yet, the machine is
// offline, GitHub is unreachable, or the signature on what came back does not
// verify. An update is an offer, not an event worth interrupting anyone over,
// so failures are logged and dropped. In dev this reaches the network for
// real, against the endpoint in tauri.conf.json.
import { relaunch } from "@tauri-apps/plugin-process";
import { check } from "@tauri-apps/plugin-updater";
import type { Update } from "@tauri-apps/plugin-updater";

export type { Update };

/** Answers `null` when there is nothing to offer, for any of the reasons above. */
export async function checkForUpdate(): Promise<Update | null> {
  try {
    return await check();
  } catch (failure) {
    console.debug("update check skipped", failure);
    return null;
  }
}

/**
 * Downloads and installs `update`, then restarts into it. Returning at all
 * means it did not work — a successful `relaunch` replaces this process, so
 * no caller ever observes the resolved promise on the happy path.
 */
export async function installAndRestart(update: Update): Promise<void> {
  try {
    await update.downloadAndInstall();
    await relaunch();
  } catch (failure) {
    console.debug("update install failed", failure);
  }
}

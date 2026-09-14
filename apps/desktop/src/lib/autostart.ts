// The OS "start at login" toggle: a thin wrapper over the `autostart`
// plugin's JS API, the same shape `updater.ts` gives the update check.
// Reading the current state is best-effort like that check — a probe that
// cannot answer (an unsupported platform, a permission the OS has not
// granted) reads as "off" rather than breaking the Always-on section.
// Changing it is NOT best-effort: a rejected enable/disable is exactly what
// `AlwaysOnSection.svelte` shows the user, not something to swallow.
import { disable, enable, isEnabled } from "@tauri-apps/plugin-autostart";

/** Whether the app is registered as a login item. `false` on any failure to ask. */
export async function autostartEnabled(): Promise<boolean> {
  try {
    return await isEnabled();
  } catch (failure) {
    console.debug("autostart status unavailable", failure);
    return false;
  }
}

/** Registers or removes the app as a login item. Rejects on failure — the caller reports it. */
export async function setAutostart(next: boolean): Promise<void> {
  if (next) {
    await enable();
  } else {
    await disable();
  }
}

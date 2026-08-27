/**
 * @wdio/tauri-service's own `beforeCommand` hook tries to recover window
 * focus ahead of getTitle/find/click by calling
 * `core.invoke('plugin:wdio|get_window_states')` — which this app can't
 * answer without the `@wdio/tauri-plugin` guest bridge (deliberately not
 * installed; see specs/smoke.e2e.ts's header). That failure is normally
 * caught and only warned about, but it occasionally leaks a stray
 * rejection that lands on whatever `it()` happens to be running.
 *
 * A plain `switchToWindow` to the window already active suppresses that
 * recovery check for the rest of the session — see the service's own
 * `suppressActiveWindowFocus`, triggered by any explicit window switch,
 * standard WebDriver or not.
 */
export async function suppressAutoFocusRecovery(browser: WebdriverIO.Browser): Promise<void> {
  const handle = await browser.getWindowHandle();
  await browser.switchToWindow(handle);
}

// Every flow spec starts its own surface the same way: click the sidebar
// nav entry, then wait for THAT surface's own container to render — so an
// `it` never races a surface that has not actually mounted. Lifted out of
// smoke.e2e.ts once a third spec (organize.e2e.ts) needed the identical two
// lines search.e2e.ts, volumes.e2e.ts and browse.e2e.ts already carried
// inline.
import { $ } from "@wdio/globals";

export async function openSurface(nav: string, container: string): Promise<void> {
  await $(nav).click();
  const el = await $(container);
  await el.waitForDisplayed({ timeout: 10_000 });
}

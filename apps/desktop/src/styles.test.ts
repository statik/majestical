import { clearMocks, mockConvertFileSrc, mockIPC } from "@tauri-apps/api/mocks";
import { render, screen } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeAll, expect, test } from "vitest";
import "./app.css";
import SearchView from "./lib/SearchView.svelte";
import VolumesView from "./lib/VolumesView.svelte";

// `app.css` is one global sheet, so a class name means the same thing to every
// surface that uses it. Nothing else in this suite can see that: jsdom's role
// and text queries read markup, and a rule from another surface silently
// changing this one's layout leaves the markup untouched. These tests load the
// real sheet and ask for the computed value, which is where such a collision
// is visible at all — `.volumes` styling the search card's badge row once laid
// the volumes table out as a flex container, head and body side by side.

// The `./app.css` import above is what puts the sheet in the document, and it
// only does so because `vite.config.ts` sets `test.css`: the default hands CSS
// back as an empty string, under which every assertion below would pass
// against no stylesheet at all. This makes that vacuous pass impossible.
beforeAll(() => {
  const rules = [...document.styleSheets].flatMap((sheet) => [
    ...sheet.cssRules,
  ]);
  expect(rules.length).toBeGreaterThan(20);
  expect(rules.map((rule) => rule.cssText).join("")).toContain(".volume-table");
});
afterEach(clearMocks);

test("the volumes table is laid out as a table", async () => {
  mockIPC((cmd) => {
    if (cmd === "list_volumes") {
      return {
        volumes: [
          {
            id: "label:Card",
            label: "Card",
            last_seen_ms: 1_700_000_000_000,
            online: true,
            asset_count: 42,
            clock_suspect: false,
          },
        ],
      };
    }
    throw new Error(`unexpected command ${cmd}`);
  });
  const { container } = render(VolumesView);

  await screen.findByRole("table");
  const table = container.querySelector(".volume-table") as HTMLElement;
  expect(globalThis.getComputedStyle(table).display).toBe("table");
});

test("a search card's volume badges still sit in a row", async () => {
  mockConvertFileSrc("macos");
  mockIPC((cmd) => {
    if (cmd === "list_saved_searches") return { saved: [] };
    return {
      count: 1,
      results: [
        {
          asset: "xxh3:abc123",
          score: 1,
          known: true,
          name: "sunset.mov",
          volumes: [{ id: "label:Card", label: "Card", online: true }],
          tags: [],
          para: null,
        },
      ],
    };
  });
  const { container } = render(SearchView, { onselect: () => {} });
  await userEvent.type(screen.getByRole("searchbox"), "q");

  await screen.findByText("sunset.mov");
  const badges = container.querySelector(".card .volumes") as HTMLElement;
  expect(globalThis.getComputedStyle(badges).display).toBe("flex");
});

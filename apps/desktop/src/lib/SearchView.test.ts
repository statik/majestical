import { clearMocks, mockConvertFileSrc, mockIPC } from "@tauri-apps/api/mocks";
import { render, screen, waitFor } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, expect, test } from "vitest";
import type { SearchHit, SearchOutcome } from "./api";
import SearchView from "./SearchView.svelte";

// `convertFileSrc` reads a webview-only internal, so every result thumbnail
// throws without this stub. The OS only picks the URL spelling.
beforeEach(() => mockConvertFileSrc("macos"));
afterEach(clearMocks);

function hit(name: string): SearchHit {
  return {
    asset: `xxh3:${name}`,
    score: 1,
    known: true,
    name,
    volumes: [{ id: "label:Card", label: "Card", online: true }],
    tags: [],
    para: null,
  };
}

const noResults: SearchOutcome = { count: 0, results: [] };

test("typing debounces to a single search for the final query", async () => {
  const queries: string[] = [];
  mockIPC((cmd, args) => {
    if (cmd === "list_saved_searches") return { saved: [] };
    if (cmd === "search_assets") {
      queries.push((args as { query: string }).query);
      return noResults;
    }
    throw new Error(`unexpected command ${cmd}`);
  });

  render(SearchView, { onselect: () => {} });
  await userEvent.type(screen.getByRole("searchbox"), "sunset");

  await waitFor(() => expect(queries).toEqual(["sunset"]));
});

test("a stale response never overwrites a newer query's results", async () => {
  let resolveFirst!: (value: SearchOutcome) => void;
  let calls = 0;
  mockIPC((cmd) => {
    if (cmd === "list_saved_searches") return { saved: [] };
    calls += 1;
    if (calls === 1) {
      return new Promise<SearchOutcome>((resolve) => {
        resolveFirst = resolve;
      });
    }
    return { count: 1, results: [hit("second")] };
  });

  render(SearchView, { onselect: () => {} });
  const box = screen.getByRole("searchbox");
  await userEvent.type(box, "a");
  await waitFor(() => expect(calls).toBe(1));
  await userEvent.clear(box);
  await userEvent.type(box, "b");
  await waitFor(() => expect(calls).toBe(2));
  await screen.findByText("second");

  resolveFirst({ count: 1, results: [hit("stale")] });
  // Give the late arrival every chance to land before asserting it did not —
  // `waitFor(() => expect(...).toBeNull())` would pass on its first check,
  // before an unguarded assignment had even run.
  await new Promise((resolve) => {
    setTimeout(resolve, 50);
  });

  expect(screen.queryByText("stale")).toBeNull();
  expect(screen.getByText("second")).toBeTruthy();
});

test("notices and coverage render verbatim, with nothing to dismiss them", async () => {
  const notice =
    "warning: skipped 1 corrupt event log line(s) in /x/events — damaged transport; affected metadata may be missing";
  mockIPC((cmd) => {
    if (cmd === "list_saved_searches") return { saved: [] };
    return {
      count: 0,
      results: [],
      notices: [notice],
      semantic_coverage: { embedded: 3, eligible: 10 },
      text_coverage: [
        {
          label: "transcripts",
          noun: "media assets",
          covered: 1,
          eligible: 4,
          remedy: "run `maj index run --transcribe`",
          source: "transcript",
        },
      ],
    };
  });

  render(SearchView, { onselect: () => {} });
  await userEvent.type(screen.getByRole("searchbox"), "q");

  await screen.findByText(notice);
  expect(
    screen.getByText("semantic index: 3 of 10 eligible assets"),
  ).toBeTruthy();
  expect(
    screen.getByText(
      "transcripts: 1 of 4 media assets — run `maj index run --transcribe`",
    ),
  ).toBeTruthy();
  // Verbatim and permanent: no control exists that could hide any of them.
  expect(screen.queryAllByRole("button")).toEqual([]);
});

test("a failed search reports the command's whole message chain", async () => {
  const message =
    "unknown filter key 'colour' — known keys: tag, vol/volume, para, kind, online, before, after, in";
  mockIPC((cmd) => {
    if (cmd === "list_saved_searches") return { saved: [] };
    // eslint-disable-next-line prefer-promise-reject-errors -- a rejected command carries the serialized `CommandError`, never an Error instance.
    return Promise.reject({ message });
  });

  render(SearchView, { onselect: () => {} });
  await userEvent.type(screen.getByRole("searchbox"), "colour:red");

  const alert = await screen.findByRole("alert");
  expect(alert.textContent).toBe(message);
});

test("a saved-search chip runs that saved search", async () => {
  const names: string[] = [];
  mockIPC((cmd, args) => {
    if (cmd === "list_saved_searches") {
      return { saved: [{ name: "b-roll", query: "tag:broll" }] };
    }
    if (cmd === "run_saved_search") {
      names.push((args as { name: string }).name);
      return { count: 1, results: [hit("clip")] };
    }
    throw new Error(`unexpected command ${cmd}`);
  });

  render(SearchView, { onselect: () => {} });
  await userEvent.click(await screen.findByRole("button", { name: "b-roll" }));

  await waitFor(() => expect(names).toEqual(["b-roll"]));
  expect(screen.getByText("clip")).toBeTruthy();
});

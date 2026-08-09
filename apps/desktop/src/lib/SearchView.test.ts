import { clearMocks, mockConvertFileSrc } from "@tauri-apps/api/mocks";
import { render, screen, waitFor, within } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, expect, test } from "vitest";
import type { SearchHit, SearchOutcome } from "./api";
import SearchView from "./SearchView.svelte";
import { mockCommands, rejectCommand } from "./test-support";

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
  mockCommands({
    list_saved_searches: () => ({ saved: [] }),
    search_assets: (args) => {
      queries.push((args as { query: string }).query);
      return noResults;
    },
  });

  render(SearchView, { onselect: () => {} });
  await userEvent.type(screen.getByRole("searchbox"), "sunset");

  await waitFor(() => expect(queries).toEqual(["sunset"]));
});

test("surrounding whitespace never reaches the backend", async () => {
  const queries: string[] = [];
  mockCommands({
    list_saved_searches: () => ({ saved: [] }),
    search_assets: (args) => {
      queries.push((args as { query: string }).query);
      return noResults;
    },
  });

  render(SearchView, { onselect: () => {} });
  await userEvent.type(screen.getByRole("searchbox"), "  sunset  ");

  await waitFor(() => expect(queries).toEqual(["sunset"]));
});

test("a stale response never overwrites a newer query's results", async () => {
  let resolveFirst!: (value: SearchOutcome) => void;
  let calls = 0;
  mockCommands({
    list_saved_searches: () => ({ saved: [] }),
    search_assets: () => {
      calls += 1;
      if (calls === 1) {
        return new Promise<SearchOutcome>((resolve) => {
          resolveFirst = resolve;
        });
      }
      return { count: 1, results: [hit("second")] };
    },
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
  mockCommands({
    list_saved_searches: () => ({ saved: [] }),
    search_assets: () => ({
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
    }),
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

test("a notice repeated in one outcome renders twice, results intact", async () => {
  // Reachable on real data: a saved-search run drains the same corrupt-log
  // notice from both the projection load and the catalog open, byte for byte.
  const notice = "warning: skipped 1 corrupt event log line(s) in /x/events";
  mockCommands({
    list_saved_searches: () => ({ saved: [] }),
    search_assets: () => ({
      count: 1,
      results: [hit("kept")],
      notices: [notice, notice],
    }),
  });

  render(SearchView, { onselect: () => {} });
  await userEvent.type(screen.getByRole("searchbox"), "q");

  await waitFor(() => expect(screen.getAllByText(notice)).toHaveLength(2));
  expect(screen.getByText("1 results")).toBeTruthy();
  expect(screen.getByText("kept")).toBeTruthy();
});

test("clearing the box cancels the search still in flight", async () => {
  let resolveFirst!: (value: SearchOutcome) => void;
  let calls = 0;
  mockCommands({
    list_saved_searches: () => ({ saved: [] }),
    search_assets: () => {
      calls += 1;
      return new Promise<SearchOutcome>((resolve) => {
        resolveFirst = resolve;
      });
    },
  });

  render(SearchView, { onselect: () => {} });
  const box = screen.getByRole("searchbox");
  await userEvent.type(box, "a");
  await waitFor(() => expect(calls).toBe(1));
  await userEvent.clear(box);

  resolveFirst({ count: 1, results: [hit("late")] });
  await new Promise((resolve) => {
    setTimeout(resolve, 50);
  });

  expect(screen.queryByText("late")).toBeNull();
  expect(screen.queryByText(/results/u)).toBeNull();
});

test("a failed search reports its whole message chain and drops stale results", async () => {
  const message =
    "unknown filter key 'colour' — known keys: tag, vol/volume, para, kind, online, before, after, in";
  let calls = 0;
  mockCommands({
    list_saved_searches: () => ({ saved: [] }),
    search_assets: () => {
      calls += 1;
      if (calls === 1) return { count: 1, results: [hit("earlier")] };
      return rejectCommand(message);
    },
  });

  render(SearchView, { onselect: () => {} });
  const box = screen.getByRole("searchbox");
  await userEvent.type(box, "sunset");
  await screen.findByText("earlier");
  // Append rather than clear: an empty box drops the results on its own, which
  // would let this pass without the catch clearing them.
  await userEvent.type(box, " colour:red");

  const alert = await screen.findByRole("alert");
  expect(alert.textContent).toBe(message);
  expect(screen.queryByText("earlier")).toBeNull();
  expect(screen.queryByText("1 results")).toBeNull();
});

test("a failed search's notices render above the error text", async () => {
  const message = "no catalog selected yet — initialize or choose one first";
  const notice = "warning: skipped 1 corrupt event log line(s) in /x/events";
  mockCommands({
    list_saved_searches: () => ({ saved: [] }),
    search_assets: () => rejectCommand(message, [notice]),
  });

  render(SearchView, { onselect: () => {} });
  await userEvent.type(screen.getByRole("searchbox"), "q");

  await screen.findByText(notice);
  expect(screen.getByRole("alert").textContent).toBe(message);
  const order = screen.getByText(notice).compareDocumentPosition(screen.getByRole("alert"));
  expect(order & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
});

test("clearing the box drops a failed search's error", async () => {
  mockCommands({
    list_saved_searches: () => ({ saved: [] }),
    search_assets: () => rejectCommand("unknown filter key 'colour'"),
  });

  render(SearchView, { onselect: () => {} });
  const box = screen.getByRole("searchbox");
  await userEvent.type(box, "colour:red");
  await screen.findByRole("alert");

  await userEvent.clear(box);

  await waitFor(() => expect(screen.queryByRole("alert")).toBeNull());
});

test("what a search returned is announced, not just drawn", async () => {
  const notice = "warning: skipped 1 corrupt event log line(s) in /x/events";
  mockCommands({
    list_saved_searches: () => ({ saved: [] }),
    search_assets: () => ({
      count: 1,
      results: [hit("kept")],
      notices: [notice],
    }),
  });

  render(SearchView, { onselect: () => {} });
  // The live region exists before the results do: one created together with
  // its text is not reliably announced.
  const live = screen.getByRole("status");
  expect(live.textContent).toBe("");

  await userEvent.type(screen.getByRole("searchbox"), "q");

  await waitFor(() =>
    expect(within(live).getByText("1 results")).toBeTruthy(),
  );
  expect(within(live).getByText(notice)).toBeTruthy();
});

test("a result's volume badge names the state its glyph stands for", async () => {
  mockCommands({
    list_saved_searches: () => ({ saved: [] }),
    search_assets: () => ({ count: 1, results: [hit("kept")] }),
  });

  render(SearchView, { onselect: () => {} });
  await userEvent.type(screen.getByRole("searchbox"), "q");

  expect(await screen.findByRole("img", { name: "Card online" })).toBeTruthy();
  expect(screen.getByText("Card●")).toBeTruthy();
});

test("a saved-search chip runs that saved search", async () => {
  const names: string[] = [];
  mockCommands({
    list_saved_searches: () => ({
      saved: [{ name: "b-roll", query: "tag:broll" }],
    }),
    run_saved_search: (args) => {
      names.push((args as { name: string }).name);
      return { count: 1, results: [hit("clip")] };
    },
  });

  render(SearchView, { onselect: () => {} });
  await userEvent.click(await screen.findByRole("button", { name: "b-roll" }));

  await waitFor(() => expect(names).toEqual(["b-roll"]));
  expect(screen.getByText("clip")).toBeTruthy();
});

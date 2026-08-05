import { render, screen } from "@testing-library/svelte";
import { expect, test } from "vitest";
import Notices from "./Notices.svelte";

// The canonical home of the keying regression: every surface renders its
// notices through this component, so one test here covers all of them.
test("a notice repeated in one outcome renders twice", () => {
  // Reachable on real data: a saved-search run drains the same corrupt-log
  // notice from both the projection load and the catalog open, byte for byte.
  // A keyed each would throw on the repeat instead of rendering it.
  const notice = "warning: skipped 1 corrupt event log line(s) in /x/events";
  render(Notices, { notices: [notice, notice] });

  expect(screen.getAllByText(notice)).toHaveLength(2);
});

test("notices render verbatim, in the order they arrived", () => {
  const first = "warning: skipped 1 corrupt event log line(s) in /x/events";
  const second = "warning: catalog is 2 schema versions behind this binary";
  const { container } = render(Notices, { notices: [first, second] });

  const rendered = [...container.querySelectorAll(".notice")].map(
    (node) => node.textContent,
  );
  expect(rendered).toEqual([first, second]);
});

test("an outcome carrying no notices renders nothing at all", () => {
  const { container } = render(Notices, { notices: undefined });

  expect(container.querySelector("*")).toBeNull();
});

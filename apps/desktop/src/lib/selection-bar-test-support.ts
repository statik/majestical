// The selection the bar suites start from, and the backend they run against.
// Shared by `SelectionBar.test.ts` (the bar and its two pickers) and
// `SelectionBar.outcome.test.ts` (what the backend's answer looks like on
// screen) — one selection and one catalog, so the two halves cannot describe
// two different bars.
import type { InvokeArgs } from "@tauri-apps/api/core";
import { render, screen } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import type { AssignOutcome } from "./api";
// The same PARA nodes and tags the Organize suites use: one catalog
// vocabulary across the surfaces that read it.
import { paraOutcome, tagsOutcome } from "./organize-test-support";
import SelectionBar from "./SelectionBar.svelte";
import type { CommandHandler } from "./test-support";
import { mockCommands } from "./test-support";

/** The three assets a grid has selected. */
export const THREE = ["xxh3:a", "xxh3:b", "xxh3:c"];

/**
 * Answers the four commands the bar can reach and records what the two
 * assignment verbs were sent. An override replaces one answer — a failure,
 * an outcome with per-asset rows — in which case that call is not recorded,
 * because what those cases assert is on screen.
 */
export function mockBar(
  overrides: Record<string, CommandHandler> = {},
): InvokeArgs[] {
  const sent: InvokeArgs[] = [];
  const record = (args: InvokeArgs | undefined): AssignOutcome => {
    sent.push(args ?? {});
    return { applied: 3, failed: [] };
  };
  mockCommands({
    list_tags: () => tagsOutcome,
    list_para: () => paraOutcome,
    assign_tags: record,
    file_assets: record,
    ...overrides,
  });
  return sent;
}

export function renderBar(
  selected: string[] = THREE,
  onclear: () => void = () => {},
) {
  return render(SelectionBar, { selected, onclear });
}

/** Renders the bar and opens one of its two pickers. */
export async function openPicker(name: string, selected: string[] = THREE) {
  renderBar(selected);
  await userEvent.click(screen.getByRole("button", { name }));
  return screen.findByRole("group", { name: /picker$/u });
}

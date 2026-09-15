import { clearMocks } from "@tauri-apps/api/mocks";
import { cleanup, screen, waitFor, within } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, test } from "vitest";
import {
  callsTo,
  DEST_A,
  mockIngest,
  NODE,
  PICKER,
  picksInTurn,
  planned,
  renderIngest,
  SOURCE,
  sourceField,
  typeDest,
  typeSource,
} from "./ingest-test-support";

// Typing a path onto the setup board. The native folder dialog reaches
// what it can mount and browse; a server share, a folder pasted out of a
// shot list, or a path an operator simply knows is typed. Nothing here
// checks that a typed path exists — `plan_ingest` is what validates it,
// and its refusal already renders in the same panel.

// Unmounted before the Tauri internals go: a surface's `listen` handle
// unregisters itself on destroy, and `clearMocks` takes the registry away.
afterEach(() => {
  cleanup();
  clearMocks();
});

/** The destination list's own subtree, so a row is never read off the
 *  field that added it. */
function destList(): HTMLElement {
  return screen.getByRole("list", { name: "Destinations" });
}

function destField(): HTMLInputElement {
  return screen.getByRole<HTMLInputElement>("textbox", {
    name: "Destination path",
  });
}

test("a typed source path is the job's source", async () => {
  const calls = mockIngest();
  renderIngest();

  await typeSource(SOURCE);
  await userEvent.selectOptions(
    screen.getByRole("combobox", { name: "PARA node" }),
    NODE,
  );
  await userEvent.click(screen.getByRole("button", { name: "Plan" }));

  await waitFor(() => expect(callsTo(calls, "plan_ingest")).toHaveLength(1));
  expect(callsTo(calls, "plan_ingest")[0]?.args).toEqual({
    source: SOURCE,
    para: NODE,
    template: undefined,
  });
  // The dialog is the alternative route, not the route: a typed path never
  // opens it.
  expect(callsTo(calls, PICKER)).toHaveLength(0);
});

test("a typed source enables Plan without leaving the field", async () => {
  const calls = mockIngest();
  renderIngest();

  // The node is chosen FIRST, so the source field is still focused when
  // Plan is clicked: the field commits as it is typed, which is what lets
  // that click land on an enabled button (a blur-only commit would leave
  // it dead — see `setSource`).
  // The node list loads on mount; wait for its option before choosing it.
  await screen.findByRole("option", { name: "project/client-x" });
  await userEvent.selectOptions(
    screen.getByRole("combobox", { name: "PARA node" }),
    NODE,
  );
  await userEvent.type(sourceField(), SOURCE);
  expect(document.activeElement).toBe(sourceField());
  // The consequence, not just the precondition: Plan is already enabled
  // while the field still has focus. A blur-only commit would leave it
  // disabled here (jsdom blurs on the click below, so the click alone
  // cannot tell the two apart).
  expect(
    screen.getByRole<HTMLButtonElement>("button", { name: "Plan" }).disabled,
  ).toBe(false);
  await userEvent.click(screen.getByRole("button", { name: "Plan" }));

  await waitFor(() => expect(callsTo(calls, "plan_ingest")).toHaveLength(1));
  expect(callsTo(calls, "plan_ingest")[0]?.args).toEqual({
    source: SOURCE,
    para: NODE,
    template: undefined,
  });
});

test("a typed source after a plan stales it", async () => {
  const calls = mockIngest({ [PICKER]: picksInTurn([SOURCE, DEST_A]) });
  await planned(calls);

  await typeSource("/Volumes/OTHER");

  // The same rule `IngestView.test.ts` pins for a browsed destination —
  // this is the typed half, on the source field.
  expect(screen.getByText(/plan again before starting/u)).toBeTruthy();
  expect(
    screen.getByRole("button", { name: "Plan again" }),
  ).toBeTruthy();
});

test("Enter in the destination field adds it and clears the field", async () => {
  mockIngest();
  renderIngest();

  await typeDest(DEST_A);

  expect(await within(destList()).findByText(DEST_A)).toBeTruthy();
  // Cleared, because the next destination is typed into the same box.
  expect(destField().value).toBe("");
});

test("the Add button adds the typed destination", async () => {
  mockIngest();
  renderIngest();

  await userEvent.type(await screen.findByRole("textbox", {
    name: "Destination path",
  }), DEST_A);
  await userEvent.click(screen.getByRole("button", { name: "Add destination" }));

  expect(await within(destList()).findByText(DEST_A)).toBeTruthy();
});

test("a duplicate destination is refused with the pinned message", async () => {
  mockIngest();
  renderIngest();

  await typeDest(DEST_A);
  await within(destList()).findByText(DEST_A);
  await typeDest(DEST_A);

  // Said out loud, rather than silently ignored: a paste that changed
  // nothing must not look like a paste that worked.
  const alert = await screen.findByRole("alert");
  expect(alert.textContent).toBe(`${DEST_A} is already a destination.`);
  expect(within(destList()).getAllByText(DEST_A)).toHaveLength(1);

  // Removing the row it collided with answers it, so the message goes.
  await userEvent.click(
    screen.getByRole("button", { name: `Remove ${DEST_A}` }),
  );
  await waitFor(() => expect(screen.queryByRole("alert")).toBeNull());
});

test("Browse for source still goes through the dialog", async () => {
  const calls = mockIngest({ [PICKER]: picksInTurn([SOURCE]) });
  renderIngest();

  await userEvent.click(
    await screen.findByRole("button", { name: "Browse for source" }),
  );

  // What the dialog answered lands in the field, which is the only place
  // the source is shown.
  await waitFor(() => expect(sourceField().value).toBe(SOURCE));
  expect(callsTo(calls, PICKER)).toHaveLength(1);
});

test("whitespace-only input is ignored", async () => {
  mockIngest();
  renderIngest();

  await typeDest("   ");

  expect(within(destList()).queryAllByRole("listitem")).toHaveLength(0);
  expect(screen.queryByRole("alert")).toBeNull();
});

test("a path with an inner space is kept as typed and trimmed only at the wire", async () => {
  const calls = mockIngest();
  renderIngest();

  await screen.findByRole("option", { name: "project/client-x" });
  await userEvent.selectOptions(
    screen.getByRole("combobox", { name: "PARA node" }),
    NODE,
  );
  await userEvent.type(sourceField(), "  /Volumes/My Card  ");
  // The field shows exactly what was typed — nothing is trimmed under the
  // operator's cursor mid-edit.
  expect(sourceField().value).toBe("  /Volumes/My Card  ");
  await userEvent.click(screen.getByRole("button", { name: "Plan" }));

  await waitFor(() => expect(callsTo(calls, "plan_ingest")).toHaveLength(1));
  expect(callsTo(calls, "plan_ingest")[0]?.args).toMatchObject({
    source: "/Volumes/My Card",
  });
});

test("a whitespace-only source leaves Plan disabled", async () => {
  mockIngest();
  renderIngest();

  await screen.findByRole("option", { name: "project/client-x" });
  await userEvent.selectOptions(
    screen.getByRole("combobox", { name: "PARA node" }),
    NODE,
  );
  await userEvent.type(sourceField(), "   ");
  expect(
    screen.getByRole<HTMLButtonElement>("button", { name: "Plan" }).disabled,
  ).toBe(true);
});

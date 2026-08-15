// The PARA column: how the nodes are grouped, what selecting one shows, and
// what adding and renaming ask the backend for. The archive modal is
// `OrganizeView.archive.test.ts`; the tag manager is
// `OrganizeView.tags.test.ts`.
import { clearMocks } from "@tauri-apps/api/mocks";
import { screen, waitFor, within } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, test } from "vitest";
import type { ParaOutcome } from "./api";
import {
  callsTo,
  mockOrganize,
  paraOutcome,
  renderOrganize,
} from "./organize-test-support";
import { rejectCommand } from "./test-support";

afterEach(() => clearMocks());

test("every node is grouped under the heading of its own kind", async () => {
  mockOrganize();
  renderOrganize();

  const projects = await screen.findByRole("list", { name: "Projects" });
  expect(
    within(projects).getByRole("button", { name: /client-x/u }),
  ).toBeTruthy();
  expect(
    within(projects).getByRole("button", { name: /spring-campaign/u }),
  ).toBeTruthy();
  expect(
    within(screen.getByRole("list", { name: "Areas" })).getByRole("button", {
      name: /studio-ops/u,
    }),
  ).toBeTruthy();
  expect(
    within(screen.getByRole("list", { name: "Resources" })).getByRole(
      "button",
      { name: /stock-library/u },
    ),
  ).toBeTruthy();
  expect(
    within(screen.getByRole("list", { name: "Archive" })).getByRole("button", {
      name: /talon-2024/u,
    }),
  ).toBeTruthy();
});

test("a kind nothing lives under gets no heading of its own", async () => {
  mockOrganize({
    list_para: (): ParaOutcome => ({ nodes: paraOutcome.nodes.slice(0, 1) }),
  });
  renderOrganize();

  await screen.findByRole("list", { name: "Projects" });
  expect(screen.queryByRole("list", { name: "Areas" })).toBeNull();
});

test("a catalog with no nodes says so, and only once it has been read", async () => {
  mockOrganize({ list_para: (): ParaOutcome => ({ nodes: [] }) });
  renderOrganize();

  // Before the read answers there is nothing to say about the catalog — the
  // line is an assertion about it, not a placeholder.
  expect(screen.queryByText(/No PARA nodes yet/u)).toBeNull();
  expect(await screen.findByText(/No PARA nodes yet/u)).toBeTruthy();
});

test("selecting a node fills the detail card with what the row says", async () => {
  mockOrganize();
  renderOrganize();

  const node = await screen.findByRole("button", { name: /client-x/u });
  expect(node.getAttribute("aria-current")).toBeNull();

  await userEvent.click(node);

  const detail = await screen.findByRole("group", { name: "Selected node" });
  const title = within(detail).getByText("project/client-x");
  expect(detail.textContent).toContain("active");
  // The node id is what support asks for, not what a user reads: it stays
  // on the title attribute and out of the card's own lines.
  expect(detail.textContent).not.toContain("01PROJECT");
  expect(title.getAttribute("title")).toBe("01PROJECT");
  expect(node.getAttribute("aria-current")).toBe("true");
});

test("an archived node is chipped as one and offers no archive action", async () => {
  mockOrganize();
  renderOrganize();

  const archived = await screen.findByRole("button", { name: /talon-2024/u });
  expect(archived.textContent).toContain("archived");

  await userEvent.click(archived);

  const detail = await screen.findByRole("group", { name: "Selected node" });
  expect(detail.textContent).toContain("archived");
  // Archiving an archived node is refused by the service; no dead button
  // offers it here.
  expect(within(detail).queryByRole("button", { name: "Archive…" })).toBeNull();
  expect(within(detail).getByRole("button", { name: "Rename" })).toBeTruthy();
});

test("a new node is created with the kind and name given, and the list re-read", async () => {
  const calls = mockOrganize();
  renderOrganize();

  await screen.findByRole("list", { name: "Projects" });
  await userEvent.selectOptions(
    screen.getByRole("combobox", { name: "Kind of the new node" }),
    "area",
  );
  await userEvent.type(
    screen.getByRole("textbox", { name: "Name of the new node" }),
    "studio-b",
  );
  await userEvent.click(screen.getByRole("button", { name: "+ New node" }));

  await waitFor(() => expect(callsTo(calls, "add_para_node")).toHaveLength(1));
  expect(callsTo(calls, "add_para_node")[0]?.args).toEqual({
    kind: "area",
    name: "studio-b",
  });
  // The list is what says the node exists, so it is re-read rather than
  // patched from the id the create returned.
  await waitFor(() => expect(callsTo(calls, "list_para")).toHaveLength(2));
});

test("a nameless new node is never sent", async () => {
  const calls = mockOrganize();
  renderOrganize();

  await screen.findByRole("list", { name: "Projects" });
  await userEvent.click(screen.getByRole("button", { name: "+ New node" }));

  expect(callsTo(calls, "add_para_node")).toHaveLength(0);
  expect((await screen.findAllByRole("alert"))[0]?.textContent).toContain(
    "needs a name",
  );
});

test("renaming a node sends the node's id and re-reads the list", async () => {
  const calls = mockOrganize();
  renderOrganize();

  await userEvent.click(
    await screen.findByRole("button", { name: /client-x/u }),
  );
  await userEvent.type(
    screen.getByRole("textbox", { name: "New name for this node" }),
    "client-y",
  );
  await userEvent.click(screen.getByRole("button", { name: "Rename" }));

  await waitFor(() =>
    expect(callsTo(calls, "rename_para_node")).toHaveLength(1),
  );
  // The id, not `project/client-x`: a node id resolves whatever the name is
  // doing, including once two machines have raced on it.
  expect(callsTo(calls, "rename_para_node")[0]?.args).toEqual({
    node: "01PROJECT",
    name: "client-y",
  });
  await waitFor(() => expect(callsTo(calls, "list_para")).toHaveLength(2));
});

test("a refused rename says why, verbatim, and keeps the node selected", async () => {
  const message = "no active PARA node 'project/client-x' — see `maj para list`";
  mockOrganize({ rename_para_node: () => rejectCommand(message) });
  renderOrganize();

  await userEvent.click(
    await screen.findByRole("button", { name: /client-x/u }),
  );
  await userEvent.type(
    screen.getByRole("textbox", { name: "New name for this node" }),
    "client-y",
  );
  await userEvent.click(screen.getByRole("button", { name: "Rename" }));

  expect((await screen.findByRole("alert")).textContent).toBe(message);
  expect(
    screen.getByRole("group", { name: "Selected node" }).textContent,
  ).toContain("project/client-x");
});

test("a failed node list leaves the column saying why, notices and all", async () => {
  const message = "no catalog selected yet — initialize or choose one first";
  mockOrganize({
    list_para: () => rejectCommand(message, ["1 event line was unreadable"]),
  });
  renderOrganize();

  expect((await screen.findByRole("alert")).textContent).toBe(message);
  expect(screen.getByText("1 event line was unreadable")).toBeTruthy();
});

test("the notices a node list carries are shown with it", async () => {
  mockOrganize({
    list_para: (): ParaOutcome => ({
      ...paraOutcome,
      notices: ["the projection was rebuilt from the log"],
    }),
  });
  renderOrganize();

  expect(
    await screen.findByText("the projection was rebuilt from the log"),
  ).toBeTruthy();
});

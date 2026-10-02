// The Captions section's Test and Remove key, and the guard that keeps one
// command in flight at a time. The form and Save are in
// `CaptionsSection.test.ts`.
import { clearMocks } from "@tauri-apps/api/mocks";
import { render, screen, waitFor } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, test } from "vitest";
import type { DescriberProbeOutcome, KeySource } from "./api-captions";
import { keyStatusLine, testLines } from "./captions-status";
import {
  backendSelect,
  button,
  field,
  loaded,
  ollama,
  openRouter,
  probe,
  unconfigured,
  withKeySource,
} from "./captions-test-support";
import { deferred, mockCommands, rejectCommand } from "./test-support";
import CaptionsSection from "./CaptionsSection.svelte";

afterEach(clearMocks);

const ACTIONS = ["Save", "Test", "Remove key"];

function resultLines(container: HTMLElement): { className: string; text: string | null }[] {
  return [...container.querySelectorAll(".captions-results li")].map((line) => ({
    className: line.className,
    text: line.textContent,
  }));
}

test("Test is disabled until the form matches what is saved", async () => {
  // A hand-set base URL, so changing the backend leaves the URL alone and
  // only the backend differs from what is saved.
  const view = withKeySource("keychain").describer;
  if (view === null) throw new Error("withKeySource must be configured");
  const proxied = { ...openRouter, describer: { ...view, base_url: "https://proxy.lan/api" } };
  mockCommands({ describer_settings: () => proxied });
  render(CaptionsSection);
  await loaded("google/gemini-2.5-flash");
  expect(button("Test").disabled).toBe(false);

  await userEvent.type(field("Model"), "-lite");
  expect(button("Test").disabled).toBe(true);
  expect(button("Test").title).toBe("Save before testing");
  await userEvent.clear(field("Model"));
  await userEvent.type(field("Model"), "google/gemini-2.5-flash");
  expect(button("Test").disabled).toBe(false);

  await userEvent.type(field("Base URL"), "/v2");
  expect(button("Test").disabled).toBe(true);
  await userEvent.clear(field("Base URL"));
  await userEvent.type(field("Base URL"), "https://proxy.lan/api");
  expect(button("Test").disabled).toBe(false);

  await userEvent.selectOptions(backendSelect(), "LM Studio");
  expect(field("Base URL").value).toBe("https://proxy.lan/api");
  expect(button("Test").disabled).toBe(true);
  await userEvent.selectOptions(backendSelect(), "OpenRouter");
  expect(button("Test").disabled).toBe(false);

  await userEvent.type(field("API key"), "sk-test");
  expect(button("Test").disabled).toBe(true);
});

test("with nothing configured, Test is disabled without the Save-before-testing hint", async () => {
  mockCommands({ describer_settings: () => unconfigured });
  render(CaptionsSection);
  await screen.findByText("No describer is configured — captions are off.");

  expect(button("Test").disabled).toBe(true);
  expect(button("Test").hasAttribute("title")).toBe(false);
});

test("Test renders the probe's lines", async () => {
  mockCommands({
    describer_settings: () => openRouter,
    test_describer: () => probe,
    save_describer: () => openRouter,
  });
  const { container } = render(CaptionsSection);
  await loaded("google/gemini-2.5-flash");

  await userEvent.click(button("Test"));

  await waitFor(() => expect(resultLines(container)).toHaveLength(3));
  expect(resultLines(container)).toEqual(
    testLines(probe).map(({ text }) => ({ className: "good", text })),
  );
  expect(screen.getByText("a notice the probe collected")).toBeTruthy();

  await userEvent.click(button("Save"));
  await screen.findByText("Saved.");
  expect(container.querySelector(".captions-results")).toBeNull();
});

test("Test marks the named problems bad and the reachable backend good", async () => {
  const failing: DescriberProbeOutcome = {
    model: "gemini-flash",
    model_listed: false,
    vision: null,
    key: "rejected",
  };
  mockCommands({ describer_settings: () => openRouter, test_describer: () => failing });
  const { container } = render(CaptionsSection);
  await loaded("google/gemini-2.5-flash");

  await userEvent.click(button("Test"));

  await waitFor(() => expect(resultLines(container)).toHaveLength(3));
  expect(resultLines(container)).toEqual([
    { className: "good", text: "Backend reachable." },
    { className: "bad", text: "Model gemini-flash is not listed — check the name." },
    { className: "bad", text: "Key rejected — OpenRouter answered 401. Save a new key." },
  ]);
});

test("a failed Test renders the alert", async () => {
  const message =
    "describer test against http://localhost:11434: probe: request to " +
    "http://localhost:11434/v1/models: Connection refused";
  mockCommands({
    describer_settings: () => ollama,
    test_describer: () => rejectCommand(message),
  });
  const { container } = render(CaptionsSection);
  await loaded("llava");

  await userEvent.click(button("Test"));

  const alert = await screen.findByRole("alert");
  expect(alert.textContent).toBe(message);
  expect(container.querySelector(".captions-results")).toBeNull();
});

test("Remove key is shown for keychain and file, hidden for env and none", async () => {
  const cases: [KeySource, boolean][] = [
    ["keychain", true],
    ["file", true],
    ["env", false],
    ["none", false],
  ];
  for (const [source, shown] of cases) {
    mockCommands({ describer_settings: () => withKeySource(source) });
    const { unmount } = render(CaptionsSection);
    await screen.findByText(keyStatusLine(source));
    expect(screen.queryByRole("button", { name: "Remove key" }) !== null).toBe(shown);
    unmount();
  }
});

test("Remove key is hidden when the form's backend is not OpenRouter", async () => {
  mockCommands({ describer_settings: () => openRouter });
  render(CaptionsSection);
  await loaded("google/gemini-2.5-flash");
  expect(button("Remove key")).toBeTruthy();

  await userEvent.selectOptions(backendSelect(), "Ollama");

  expect(screen.queryByRole("button", { name: "Remove key" })).toBeNull();
});

test("Remove key replaces the status, clears stale results and Saved., and calls onchanged", async () => {
  let changes = 0;
  mockCommands({
    describer_settings: () => openRouter,
    save_describer: () => openRouter,
    test_describer: () => probe,
    clear_describer_key: () => withKeySource("none"),
  });
  const { container } = render(CaptionsSection, { onchanged: () => (changes += 1) });
  await loaded("google/gemini-2.5-flash");
  await userEvent.click(button("Save"));
  await screen.findByText("Saved.");
  await userEvent.click(button("Test"));
  await screen.findByText("Key accepted.");

  await userEvent.click(button("Remove key"));

  expect(await screen.findByText(keyStatusLine("none"))).toBeTruthy();
  expect(screen.queryByRole("button", { name: "Remove key" })).toBeNull();
  expect(field("API key").placeholder).toBe("sk-or-…");
  expect(container.querySelector(".captions-results")).toBeNull();
  expect(screen.queryByText("Saved.")).toBeNull();
  expect(changes).toBe(2);
});

test("a failed Remove key shows the alert and keeps the status", async () => {
  mockCommands({
    describer_settings: () => openRouter,
    clear_describer_key: () => rejectCommand("Keychain: item is locked"),
  });
  render(CaptionsSection);
  await loaded("google/gemini-2.5-flash");

  await userEvent.click(button("Remove key"));

  expect((await screen.findByRole("alert")).textContent).toBe("Keychain: item is locked");
  expect(screen.getByText(keyStatusLine("keychain"))).toBeTruthy();
});

test("a Save in flight disables Save, Test and Remove key", async () => {
  const pending = deferred<unknown>();
  mockCommands({ describer_settings: () => openRouter, save_describer: () => pending.promise });
  render(CaptionsSection);
  await loaded("google/gemini-2.5-flash");

  await userEvent.click(button("Save"));

  await waitFor(() =>
    expect(ACTIONS.map((name) => button(name).disabled)).toEqual([true, true, true]),
  );
  pending.settle(openRouter);
  await waitFor(() => expect(button("Save").disabled).toBe(false));
});

test("a Test in flight disables Save, Test and Remove key", async () => {
  const pending = deferred<unknown>();
  mockCommands({ describer_settings: () => openRouter, test_describer: () => pending.promise });
  render(CaptionsSection);
  await loaded("google/gemini-2.5-flash");

  await userEvent.click(button("Test"));

  await waitFor(() =>
    expect(ACTIONS.map((name) => button(name).disabled)).toEqual([true, true, true]),
  );
  pending.settle(probe);
  await waitFor(() => expect(button("Save").disabled).toBe(false));
});

test("a key stored for a local backend does not show as OpenRouter's", async () => {
  // Save drops a stored key across a backend switch
  // (`describer_config::carried_key`), so the form must not offer to keep it.
  mockCommands({
    describer_settings: () => ({
      keychain_supported: true,
      describer: {
        backend: "ollama",
        base_url: "http://localhost:11434",
        model: "llava",
        key_source: "file",
      },
    }),
  });
  render(CaptionsSection);
  await loaded("llava");

  await userEvent.selectOptions(backendSelect(), "OpenRouter");

  expect(field("API key").placeholder).toBe("sk-or-…");
  expect(screen.getByText("No key. Captions cannot run until one is saved.")).toBeTruthy();
  expect(screen.queryByRole("button", { name: "Remove key" })).toBeNull();
});

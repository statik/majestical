import { clearMocks } from "@tauri-apps/api/mocks";
import { render, screen, waitFor } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, test } from "vitest";
import type {
  DescriberProbeOutcome,
  DescriberSettingsOutcome,
  KeySource,
} from "./api-captions";
import { keyStatusLine, testLines, UNCONFIGURED_LINE } from "./captions-status";
import describerProbe from "./fixtures/describer_probe.json";
import describerSettings from "./fixtures/describer_settings.json";
import { mockCommands, rejectCommand } from "./test-support";
import CaptionsSection from "./CaptionsSection.svelte";

afterEach(clearMocks);

/** `describer_settings_unconfigured.json`'s shape, inlined to keep this
 *  suite under the import cap; `fixtures.captions.test.ts` pins the wire. */
const unconfigured: DescriberSettingsOutcome = { describer: null, keychain_supported: true };
const openRouter = describerSettings as DescriberSettingsOutcome;
const probe = describerProbe as DescriberProbeOutcome;

const ollama: DescriberSettingsOutcome = {
  describer: {
    backend: "ollama",
    base_url: "http://localhost:11434",
    model: "llava",
    key_source: "none",
  },
  keychain_supported: true,
};

function withKeySource(source: KeySource): DescriberSettingsOutcome {
  const view = openRouter.describer;
  if (view === null) throw new Error("describer_settings.json must be configured");
  return { ...openRouter, describer: { ...view, key_source: source } };
}

/** Every `save_describer` call's `req`, in order. */
function recordSaves(answer: () => unknown): { reqs: Record<string, unknown>[] } {
  const reqs: Record<string, unknown>[] = [];
  mockCommands({
    describer_settings: () => openRouter,
    save_describer: (args) => {
      reqs.push((args as { req: Record<string, unknown> }).req);
      return answer();
    },
  });
  return { reqs };
}

function button(name: string): HTMLButtonElement {
  return screen.getByRole("button", { name }) as HTMLButtonElement;
}

function field(name: string): HTMLInputElement {
  return screen.getByLabelText(name) as HTMLInputElement;
}

async function loaded(model: string): Promise<void> {
  await waitFor(() => expect(field("Model").value).toBe(model));
}

test("an unconfigured catalog preselects Ollama and disables Test", async () => {
  mockCommands({ describer_settings: () => unconfigured });
  render(CaptionsSection);

  expect(await screen.findByText(UNCONFIGURED_LINE)).toBeTruthy();
  expect((screen.getByLabelText("Backend") as HTMLSelectElement).value).toBe("ollama");
  expect(field("Base URL").value).toBe("http://localhost:11434");
  expect(field("Model").placeholder).toBe("The model's name as the backend lists it");
  expect(button("Save").disabled).toBe(true);
  expect(button("Test").disabled).toBe(true);
});

test("a configured OpenRouter describer shows the keychain status and an empty key field", async () => {
  mockCommands({ describer_settings: () => openRouter });
  render(CaptionsSection);

  await loaded("google/gemini-2.5-flash");
  expect((screen.getByLabelText("Backend") as HTMLSelectElement).value).toBe("open-router");
  expect(field("Base URL").value).toBe("https://openrouter.ai/api");
  expect(field("API key").value).toBe("");
  expect(field("API key").type).toBe("password");
  expect(field("API key").placeholder).toBe("Leave empty to keep the stored key");
  expect(screen.getByText(keyStatusLine("keychain"))).toBeTruthy();
  expect(screen.getByText("a notice the settings read collected")).toBeTruthy();
  expect(screen.queryByText(UNCONFIGURED_LINE)).toBeNull();
});

test("the key row is absent for a local backend", async () => {
  mockCommands({ describer_settings: () => ollama });
  render(CaptionsSection);

  await loaded("llava");
  expect(screen.queryByLabelText("API key")).toBeNull();
  expect(screen.queryByText(keyStatusLine("none"))).toBeNull();
});

test("switching backend replaces a default URL but keeps an edited one", async () => {
  mockCommands({ describer_settings: () => unconfigured });
  render(CaptionsSection);
  await screen.findByText(UNCONFIGURED_LINE);
  const backend = screen.getByLabelText("Backend");

  await userEvent.selectOptions(backend, "LM Studio");
  expect(field("Base URL").value).toBe("http://localhost:1234");

  await userEvent.clear(field("Base URL"));
  await userEvent.type(field("Base URL"), "http://studio.lan:1234");
  await userEvent.selectOptions(backend, "OpenRouter");
  expect(field("Base URL").value).toBe("http://studio.lan:1234");
});

test("Save sends no api_key when the field is empty", async () => {
  const { reqs } = recordSaves(() => openRouter);
  render(CaptionsSection);
  await loaded("google/gemini-2.5-flash");

  await userEvent.click(button("Save"));

  await waitFor(() => expect(reqs).toHaveLength(1));
  expect(reqs[0]).toEqual({
    backend: "open-router",
    model: "google/gemini-2.5-flash",
    base_url: "https://openrouter.ai/api",
  });
  expect("api_key" in (reqs[0] ?? {})).toBe(false);
});

test("Save sends the typed key, then empties the field and says Saved.", async () => {
  const { reqs } = recordSaves(() => openRouter);
  render(CaptionsSection);
  await loaded("google/gemini-2.5-flash");

  await userEvent.type(field("API key"), "sk-test");
  await userEvent.click(button("Save"));

  expect(await screen.findByText("Saved.")).toBeTruthy();
  expect(reqs[0]?.["api_key"]).toBe("sk-test");
  expect(field("API key").value).toBe("");
  expect(document.body.textContent).not.toContain("sk-test");

  await userEvent.type(field("Model"), "-lite");
  expect(screen.queryByText("Saved.")).toBeNull();
});

test("a failed Save keeps the typed key and shows the alert", async () => {
  const message = "Could not store the key in the macOS Keychain — nothing was saved.";
  recordSaves(() => rejectCommand(message));
  render(CaptionsSection);
  await loaded("google/gemini-2.5-flash");

  await userEvent.type(field("API key"), "sk-test");
  await userEvent.click(button("Save"));

  const alert = await screen.findByRole("alert");
  expect(alert.textContent).toBe(message);
  expect(field("API key").value).toBe("sk-test");
  expect(screen.queryByText("Saved.")).toBeNull();
});

test("Save calls onchanged", async () => {
  let changes = 0;
  recordSaves(() => openRouter);
  render(CaptionsSection, { onchanged: () => (changes += 1) });
  await loaded("google/gemini-2.5-flash");

  await userEvent.click(button("Save"));

  await waitFor(() => expect(changes).toBe(1));
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

test("Remove key replaces the status with the answer and calls onchanged", async () => {
  let changes = 0;
  mockCommands({
    describer_settings: () => openRouter,
    clear_describer_key: () => withKeySource("none"),
  });
  render(CaptionsSection, { onchanged: () => (changes += 1) });
  await loaded("google/gemini-2.5-flash");

  await userEvent.click(button("Remove key"));

  expect(await screen.findByText(keyStatusLine("none"))).toBeTruthy();
  expect(screen.queryByRole("button", { name: "Remove key" })).toBeNull();
  expect(field("API key").placeholder).toBe("sk-or-…");
  expect(changes).toBe(1);
});

test("Test is disabled until the form matches what is saved", async () => {
  mockCommands({ describer_settings: () => openRouter });
  render(CaptionsSection);
  await loaded("google/gemini-2.5-flash");
  expect(button("Test").disabled).toBe(false);

  await userEvent.type(field("Model"), "-lite");
  expect(button("Test").disabled).toBe(true);
  expect(button("Test").title).toBe("Save before testing");

  await userEvent.clear(field("Model"));
  await userEvent.type(field("Model"), "google/gemini-2.5-flash");
  expect(button("Test").disabled).toBe(false);

  await userEvent.type(field("API key"), "sk-test");
  expect(button("Test").disabled).toBe(true);
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

  await waitFor(() => expect(container.querySelectorAll(".captions-results li")).toHaveLength(3));
  const rendered = [...container.querySelectorAll(".captions-results li")].map((line) => ({
    good: line.classList.contains("good") && !line.classList.contains("bad"),
    text: line.textContent,
  }));
  expect(rendered).toEqual(testLines(probe));
  expect(screen.getByText("a notice the probe collected")).toBeTruthy();

  await userEvent.click(button("Save"));
  await screen.findByText("Saved.");
  expect(container.querySelector(".captions-results")).toBeNull();
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

// The Captions section's form and Save. Test, Remove key and the
// in-flight guard are in `CaptionsSection.actions.test.ts`.
import { clearMocks } from "@tauri-apps/api/mocks";
import { render, screen, waitFor } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, test, vi } from "vitest";
import { keyStatusLine, UNCONFIGURED_LINE } from "./captions-status";
import {
  backendSelect,
  button,
  field,
  loaded,
  ollama,
  openRouter,
  recordSaves,
  unconfigured,
} from "./captions-test-support";
import { mockCommands, rejectCommand } from "./test-support";
import CaptionsSection from "./CaptionsSection.svelte";

afterEach(() => {
  clearMocks();
  vi.restoreAllMocks();
});

const CONSOLE_METHODS = ["log", "info", "warn", "error", "debug"] as const;

/** Every argument any console method received, as text. */
function spyConsole(): () => string[] {
  const spies = CONSOLE_METHODS.map((method) =>
    vi.spyOn(console, method).mockImplementation(() => {}),
  );
  return () =>
    spies.flatMap((spy) =>
      spy.mock.calls.flat().map((arg) => (typeof arg === "string" ? arg : JSON.stringify(arg))),
    );
}

test("an unconfigured catalog preselects Ollama and disables Test", async () => {
  mockCommands({ describer_settings: () => unconfigured });
  render(CaptionsSection);

  expect(await screen.findByText(UNCONFIGURED_LINE)).toBeTruthy();
  expect(backendSelect().value).toBe("ollama");
  expect(field("Base URL").value).toBe("http://localhost:11434");
  expect(field("Model").placeholder).toBe("The model's name as the backend lists it");
  expect(button("Save").disabled).toBe(true);
  expect(button("Test").disabled).toBe(true);
});

test("the section reads the mockup's heading and subtitle", async () => {
  mockCommands({ describer_settings: () => unconfigured });
  const { container } = render(CaptionsSection);

  expect(await screen.findByRole("heading", { name: "Captions" })).toBeTruthy();
  expect(container.querySelector(".settings-section-sub")?.textContent).toBe(
    "The service that writes captions and suggests tags. Ollama and LM Studio run on this " +
      "Mac; OpenRouter is a hosted API.",
  );
});

test("a configured OpenRouter describer shows the keychain status and an empty key field", async () => {
  mockCommands({ describer_settings: () => openRouter });
  render(CaptionsSection);

  await loaded("google/gemini-2.5-flash");
  expect(backendSelect().value).toBe("open-router");
  expect(field("Base URL").value).toBe("https://openrouter.ai/api");
  expect(field("API key").value).toBe("");
  expect(field("API key").type).toBe("password");
  expect(field("API key").placeholder).toBe("Leave empty to keep the stored key");
  expect(screen.getByText(keyStatusLine("keychain"))).toBeTruthy();
  expect(screen.getByText("a notice the settings read collected")).toBeTruthy();
  expect(screen.queryByText(UNCONFIGURED_LINE)).toBeNull();
});

test("a failed load shows the alert", async () => {
  mockCommands({ describer_settings: () => rejectCommand("describer.toml: bad TOML") });
  render(CaptionsSection);

  expect((await screen.findByRole("alert")).textContent).toBe("describer.toml: bad TOML");
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

  await userEvent.selectOptions(backendSelect(), "LM Studio");
  expect(field("Base URL").value).toBe("http://localhost:1234");

  await userEvent.clear(field("Base URL"));
  await userEvent.type(field("Base URL"), "http://studio.lan:1234");
  await userEvent.selectOptions(backendSelect(), "OpenRouter");
  expect(field("Base URL").value).toBe("http://studio.lan:1234");
});

test("leaving OpenRouter drops a typed key, so a saved local backend can still be tested", async () => {
  mockCommands({ describer_settings: () => ollama });
  render(CaptionsSection);
  await loaded("llava");

  await userEvent.selectOptions(backendSelect(), "OpenRouter");
  await userEvent.type(field("API key"), "sk-test");
  await userEvent.selectOptions(backendSelect(), "Ollama");

  expect(button("Test").disabled).toBe(false);
  expect(button("Test").hasAttribute("title")).toBe(false);
  await userEvent.selectOptions(backendSelect(), "OpenRouter");
  expect(field("API key").value).toBe("");
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

test("Save sends no api_key for a local backend, even after one was typed", async () => {
  const { reqs } = recordSaves(() => ollama);
  render(CaptionsSection);
  await loaded("google/gemini-2.5-flash");

  await userEvent.type(field("API key"), "sk-test");
  await userEvent.selectOptions(backendSelect(), "Ollama");
  await userEvent.click(button("Save"));

  await waitFor(() => expect(reqs).toHaveLength(1));
  expect(reqs[0]?.["backend"]).toBe("ollama");
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

test("a failed Save keeps the typed key and shows the alert, and never renders or logs it", async () => {
  const consoleText = spyConsole();
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
  expect(document.body.innerHTML).not.toContain("sk-test");
  expect(document.body.textContent).not.toContain("sk-test");
  expect(consoleText().filter((text) => text.includes("sk-test"))).toEqual([]);
});

test("a failed Save renders the failure's notices with the alert", async () => {
  recordSaves(() => rejectCommand("save refused", ["notice: the refusal collected this"]));
  render(CaptionsSection);
  await loaded("google/gemini-2.5-flash");

  await userEvent.click(button("Save"));

  expect((await screen.findByRole("alert")).textContent).toBe("save refused");
  expect(screen.getByText("notice: the refusal collected this")).toBeTruthy();
});

test("Save calls onchanged", async () => {
  let changes = 0;
  recordSaves(() => openRouter);
  render(CaptionsSection, { onchanged: () => (changes += 1) });
  await loaded("google/gemini-2.5-flash");

  await userEvent.click(button("Save"));

  await waitFor(() => expect(changes).toBe(1));
});

import { clearMocks } from "@tauri-apps/api/mocks";
import { render, screen, waitFor, within } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, test } from "vitest";
import describerSettings from "./fixtures/describer_settings.json";
import describerSettingsUnconfigured from "./fixtures/describer_settings_unconfigured.json";
import doctorOutcome from "./fixtures/doctor_outcome.json";
import schedulerState from "./fixtures/scheduler_state.json";
import { mockCommands, rejectCommand } from "./test-support";
import SettingsView from "./SettingsView.svelte";

afterEach(clearMocks);

// Every test here mounts the whole Settings surface, which also mounts
// `CaptionsSection` and `AlwaysOnSection` — their own suites
// (`CaptionsSection.test.ts`, `AlwaysOnSection.test.ts`) pin their
// behavior; these commands are mocked in every test below only so mounting
// them does not throw "unexpected command" and drown an unrelated
// assertion in a rejected promise.
const siblings = {
  describer_settings: () => describerSettingsUnconfigured,
  scheduler_state: () => schedulerState,
  "plugin:autostart|is_enabled": () => false,
};

test("renders one row per check from doctor_report, in the outcome's order", async () => {
  mockCommands({ doctor_report: () => doctorOutcome, ...siblings });
  const { container } = render(SettingsView);

  const rows = await screen.findAllByRole("listitem");
  expect(rows).toHaveLength(3);
  // The outcome's own order (ffmpeg, catalog, models) — the surface must
  // not sort these itself.
  expect(
    [...container.querySelectorAll(".settings-check-name")].map(
      (name) => name.textContent,
    ),
  ).toEqual(["ffmpeg", "catalog", "models"]);

  const first = within(rows[0] as HTMLElement);
  expect(first.getByText("ok")).toBeTruthy();
  expect(first.getByText("ffmpeg")).toBeTruthy();
  expect(
    first.getByText("ffmpeg 7.1 at /opt/homebrew/bin/ffmpeg"),
  ).toBeTruthy();

  const second = within(rows[1] as HTMLElement);
  expect(second.getByText("warn")).toBeTruthy();
  expect(second.getByText("catalog")).toBeTruthy();

  const third = within(rows[2] as HTMLElement);
  expect(third.getByText("fail")).toBeTruthy();
  expect(third.getByText("models")).toBeTruthy();
});

test("the Fail row shows its remedy and the Ok row shows none", async () => {
  mockCommands({ doctor_report: () => doctorOutcome, ...siblings });
  render(SettingsView);

  const rows = await screen.findAllByRole("listitem");
  const okRow = within(rows[0] as HTMLElement);
  const failRow = within(rows[2] as HTMLElement);

  expect(
    failRow.getByText("run `maj model fetch --only clip`", { exact: false }),
  ).toBeTruthy();
  expect(okRow.queryByText(/remedy/u)).toBeNull();
});

test("the outcome's notices render above the rows", async () => {
  mockCommands({ doctor_report: () => doctorOutcome, ...siblings });
  render(SettingsView);

  expect(
    await screen.findByText("a notice the doctor sweep collected"),
  ).toBeTruthy();
});

test('"Run checks again" re-invokes doctor_report', async () => {
  let calls = 0;
  mockCommands({
    doctor_report: () => {
      calls += 1;
      return doctorOutcome;
    },
    ...siblings,
  });
  render(SettingsView);

  await screen.findAllByRole("listitem");
  expect(calls).toBe(1);

  await (
    await screen.findByRole("button", { name: "Run checks again" })
  ).click();

  await waitFor(() => expect(calls).toBe(2));
});

test("a rejected command renders the error through Notices, not a blank panel", async () => {
  const message = "no catalog selected yet — initialize or choose one first";
  const notice = "notice: the failing call still collected this";
  mockCommands({ doctor_report: () => rejectCommand(message, [notice]), ...siblings });
  render(SettingsView);

  const alert = await screen.findByRole("alert");
  expect(alert.textContent).toBe(message);
  expect(await screen.findByText(notice)).toBeTruthy();
  expect(screen.queryAllByRole("listitem")).toEqual([]);
});

test("a Captions save re-runs doctor_report so the describer row follows it", async () => {
  let calls = 0;
  mockCommands({
    doctor_report: () => {
      calls += 1;
      return doctorOutcome;
    },
    ...siblings,
    save_describer: () => describerSettings,
  });
  render(SettingsView);

  await screen.findAllByRole("listitem");
  expect(calls).toBe(1);

  await userEvent.type(screen.getByLabelText("Model"), "llava");
  await userEvent.click(screen.getByRole("button", { name: "Save" }));

  await waitFor(() => expect(calls).toBe(2));
});

test("the sections stand in order: Health, Captions, Always-on", async () => {
  mockCommands({ doctor_report: () => doctorOutcome, ...siblings });
  render(SettingsView);

  await screen.findAllByRole("listitem");
  expect(
    screen.getAllByRole("heading", { level: 3 }).map((heading) => heading.textContent),
  ).toEqual(["Health", "Captions", "Always-on"]);
});

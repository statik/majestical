import { clearMocks } from "@tauri-apps/api/mocks";
import { render, screen, waitFor, within } from "@testing-library/svelte";
import { afterEach, expect, test } from "vitest";
import doctorOutcome from "./fixtures/doctor_outcome.json";
import { mockCommands, rejectCommand } from "./test-support";
import SettingsView from "./SettingsView.svelte";

afterEach(clearMocks);

test("renders one row per check from doctor_report, in the outcome's order", async () => {
  let calls = 0;
  mockCommands({
    doctor_report: () => {
      calls += 1;
      return doctorOutcome;
    },
  });
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
  expect(first.getByText("Ok")).toBeTruthy();
  expect(first.getByText("ffmpeg")).toBeTruthy();
  expect(
    first.getByText("ffmpeg 7.1 at /opt/homebrew/bin/ffmpeg"),
  ).toBeTruthy();

  const second = within(rows[1] as HTMLElement);
  expect(second.getByText("Warn")).toBeTruthy();
  expect(second.getByText("catalog")).toBeTruthy();

  const third = within(rows[2] as HTMLElement);
  expect(third.getByText("Fail")).toBeTruthy();
  expect(third.getByText("models")).toBeTruthy();

  expect(container.querySelectorAll(".settings-check")).toHaveLength(3);
  expect(calls).toBe(1);
});

test("the Fail row shows its remedy and the Ok row shows none", async () => {
  mockCommands({ doctor_report: () => doctorOutcome });
  render(SettingsView);

  const rows = await screen.findAllByRole("listitem");
  const okRow = within(rows[0] as HTMLElement);
  const failRow = within(rows[2] as HTMLElement);

  expect(
    failRow.getByText("run `maj model fetch --only clip`", { exact: false }),
  ).toBeTruthy();
  expect(okRow.queryByText(/remedy/u)).toBeNull();
});

test("notices render", async () => {
  mockCommands({ doctor_report: () => doctorOutcome });
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
  mockCommands({ doctor_report: () => rejectCommand(message, [notice]) });
  render(SettingsView);

  const alert = await screen.findByRole("alert");
  expect(alert.textContent).toBe(message);
  expect(await screen.findByText(notice)).toBeTruthy();
  expect(screen.queryAllByRole("listitem")).toEqual([]);
});

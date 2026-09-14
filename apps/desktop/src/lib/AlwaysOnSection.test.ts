import { clearMocks } from "@tauri-apps/api/mocks";
import { render, screen, waitFor } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, test } from "vitest";
import schedulerState from "./fixtures/scheduler_state.json";
import schedulerStateHeld from "./fixtures/scheduler_state_held.json";
import type { SchedulerStateOutcome } from "./api";
import { mockCommands, rejectCommand } from "./test-support";
import AlwaysOnSection from "./AlwaysOnSection.svelte";

afterEach(clearMocks);

// `scheduler_state.json` is throttle "auto"; `scheduler_state_held.json` is
// throttle "paused" — two different fixtures so a test asserting "paused is
// checked" cannot pass by coincidence against a component that always
// checks "auto".
const auto = schedulerState as SchedulerStateOutcome;
const held = schedulerStateHeld as SchedulerStateOutcome;

/** No `@testing-library/jest-dom` in this project — every other suite reads
 *  boolean DOM state off the element itself (see `UpdateBanner.test.ts`'s
 *  `hasAttribute("disabled")`), so this does the same for `checked`. */
function checked(el: Element): boolean {
  return (el as HTMLInputElement).checked;
}

test("the throttle radio checked from scheduler_state, not assumed", async () => {
  mockCommands({
    scheduler_state: () => auto,
    "plugin:autostart|is_enabled": () => false,
  });
  render(AlwaysOnSection);

  const autoRadio = await screen.findByRole("radio", {
    name: "Auto",
    checked: true,
  });
  expect(checked(autoRadio)).toBe(true);
  expect(checked(screen.getByRole("radio", { name: "Paused" }))).toBe(false);
});

test("a different scheduler_state checks a different radio", async () => {
  mockCommands({
    scheduler_state: () => held,
    "plugin:autostart|is_enabled": () => false,
  });
  render(AlwaysOnSection);

  const pausedRadio = await screen.findByRole("radio", {
    name: "Paused",
    checked: true,
  });
  expect(checked(pausedRadio)).toBe(true);
  expect(checked(screen.getByRole("radio", { name: "Auto" }))).toBe(false);
});

test("clicking a radio invokes set_throttle and the group reflects the response", async () => {
  const calls: unknown[] = [];
  mockCommands({
    scheduler_state: () => auto,
    set_throttle: (args) => {
      calls.push(args);
      return held;
    },
    "plugin:autostart|is_enabled": () => false,
  });
  render(AlwaysOnSection);

  await screen.findByRole("radio", { name: "Auto", checked: true });
  await userEvent.click(screen.getByRole("radio", { name: "Paused" }));

  await screen.findByRole("radio", { name: "Paused", checked: true });
  expect(calls).toEqual([{ throttle: "paused" }]);
});

test("the throttle group works even when scheduler_state.available is false", async () => {
  mockCommands({
    scheduler_state: () => ({ ...auto, available: false }),
    "plugin:autostart|is_enabled": () => false,
  });
  render(AlwaysOnSection);

  await screen.findByRole("radio", { name: "Auto", checked: true });
  // Each label is looked up through `getByRole`, which excludes hidden
  // elements by default — finding all four is itself the "nothing hides"
  // assertion, not just a precondition for the `disabled` check below.
  for (const label of ["Auto", "Paused", "Low", "Full"]) {
    const radio = screen.getByRole("radio", { name: label });
    expect((radio as HTMLInputElement).disabled).toBe(false);
  }
});

test('"Start at login" reflects a true autostart state', async () => {
  mockCommands({
    scheduler_state: () => auto,
    "plugin:autostart|is_enabled": () => true,
  });
  render(AlwaysOnSection);

  const checkbox = await screen.findByRole("checkbox", {
    name: "Start at login",
    checked: true,
  });
  expect(checked(checkbox)).toBe(true);
});

test('"Start at login" reflects a false autostart state', async () => {
  mockCommands({
    scheduler_state: () => auto,
    "plugin:autostart|is_enabled": () => false,
  });
  render(AlwaysOnSection);

  const checkbox = await screen.findByRole("checkbox", {
    name: "Start at login",
    checked: false,
  });
  expect(checked(checkbox)).toBe(false);
});

test("checking the box enables autostart via the plugin", async () => {
  const calls: string[] = [];
  mockCommands({
    scheduler_state: () => auto,
    "plugin:autostart|is_enabled": () => false,
    "plugin:autostart|enable": () => {
      calls.push("enable");
      return null;
    },
  });
  render(AlwaysOnSection);

  await userEvent.click(
    await screen.findByRole("checkbox", { name: "Start at login" }),
  );

  await waitFor(() => expect(calls).toEqual(["enable"]));
});

test("unchecking the box disables autostart via the plugin", async () => {
  const calls: string[] = [];
  mockCommands({
    scheduler_state: () => auto,
    "plugin:autostart|is_enabled": () => true,
    "plugin:autostart|disable": () => {
      calls.push("disable");
      return null;
    },
  });
  render(AlwaysOnSection);

  const checkbox = await screen.findByRole("checkbox", {
    name: "Start at login",
    checked: true,
  });
  await userEvent.click(checkbox);

  await waitFor(() => expect(calls).toEqual(["disable"]));
});

test("a rejected enable renders the error and reverts the checkbox", async () => {
  // The autostart plugin's own `Error` actually serializes as a plain
  // string on the wire (`impl Serialize for Error` in
  // `tauri-plugin-autostart`), not the app's `CommandError` object —
  // `errorMessage` falls back to `String(error)` for exactly that shape, so
  // it reads a bare string the same as it reads `{message}`. `rejectCommand`
  // is reused here anyway, for the same "not a command this test forgot to
  // mock" guarantee every other rejected-command test in this app relies on.
  mockCommands({
    scheduler_state: () => auto,
    "plugin:autostart|is_enabled": () => false,
    "plugin:autostart|enable": () => rejectCommand("permission denied"),
  });
  render(AlwaysOnSection);

  const checkbox = await screen.findByRole("checkbox", {
    name: "Start at login",
  });
  await userEvent.click(checkbox);

  const alert = await screen.findByRole("alert");
  expect(alert.textContent).toBe("permission denied");
  await waitFor(() =>
    expect(
      checked(screen.getByRole("checkbox", { name: "Start at login" })),
    ).toBe(false),
  );
});

// Pinned against the `role="status"` element specifically, not a loose
// `findByText` — a loose text query also matches the throttle radio labeled
// "Paused", so it cannot tell "the status line says Paused" apart from "a
// radio labeled Paused exists" (a mutant that deletes the status `<p>` or
// reworks its text survives a `findByText` assertion for exactly that
// reason).
const statusCases: {
  label: string;
  state: SchedulerStateOutcome;
  expected: string;
}[] = [
  {
    label: "run_full with a plural pending count",
    state: { ...auto, decision: { mode: "run_full" }, pending_items: 42 },
    expected: "Indexing — 42 items pending",
  },
  {
    label: "run_full with a singular pending count",
    state: { ...auto, decision: { mode: "run_full" }, pending_items: 1 },
    expected: "Indexing — 1 item pending",
  },
  {
    label: "run_low",
    state: { ...auto, decision: { mode: "run_low" }, pending_items: 3 },
    expected: "Indexing slowly — 3 items pending",
  },
  {
    label: "hold/paused",
    state: { ...held, decision: { mode: "hold", hold_reason: "paused" } },
    expected: "Paused",
  },
  {
    label: "hold/low_power_mode",
    state: {
      ...auto,
      decision: { mode: "hold", hold_reason: "low_power_mode" },
    },
    expected: "Paused (Low Power Mode)",
  },
  {
    label: "hold/no_pending_work",
    state: {
      ...auto,
      decision: { mode: "hold", hold_reason: "no_pending_work" },
      pending_items: 0,
    },
    expected: "Idle",
  },
  {
    label: "no decision yet",
    state: { ...auto, decision: null },
    expected: "Starting…",
  },
];

test.each(statusCases)(
  "the status line reads $expected for $label",
  async ({ state, expected }) => {
    mockCommands({
      scheduler_state: () => state,
      "plugin:autostart|is_enabled": () => false,
    });
    render(AlwaysOnSection);

    const status = await screen.findByRole("status");
    await waitFor(() => expect(status.textContent).toBe(expected));
  },
);

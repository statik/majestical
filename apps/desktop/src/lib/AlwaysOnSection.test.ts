import { clearMocks } from "@tauri-apps/api/mocks";
import { render, screen, waitFor } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, test } from "vitest";
import schedulerState from "./fixtures/scheduler_state.json";
import schedulerStateHeld from "./fixtures/scheduler_state_held.json";
import type { SchedulerStateOutcome } from "./api-alwayson";
import { statusLine } from "./scheduler-status";
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

test("clicking a radio invokes set_throttle, and the group reflects the RESPONSE, not the click", async () => {
  // Answers a click on "Low" with the `held` fixture (throttle "paused"),
  // so a component that (wrongly) checks whichever radio was clicked
  // instead of applying `set_throttle`'s returned state cannot pass this
  // by checking "Low" — only "Paused" being checked afterward proves the
  // response was applied.
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
  await userEvent.click(screen.getByRole("radio", { name: "Low" }));

  await screen.findByRole("radio", { name: "Paused", checked: true });
  expect(checked(screen.getByRole("radio", { name: "Low" }))).toBe(false);
  expect(calls).toEqual([{ throttle: "low" }]);
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

// One test pinning the wiring — that the section renders `statusLine`'s
// result through the `role="status"` element — not the full table of
// decision/hold-reason cases, which belongs to `statusLine` itself and is
// pinned directly against the function in `scheduler-status.test.ts`.
test("the status line renders statusLine's result through role=status", async () => {
  mockCommands({
    scheduler_state: () => auto,
    "plugin:autostart|is_enabled": () => false,
  });
  render(AlwaysOnSection);

  const status = await screen.findByRole("status");
  await waitFor(() =>
    expect(status.textContent).toBe(statusLine(auto)),
  );
});

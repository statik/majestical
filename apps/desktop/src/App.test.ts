import { render, screen } from "@testing-library/svelte";
import { expect, test } from "vitest";
import App from "./App.svelte";

test("shell renders", () => {
  render(App);
  expect(screen.getByRole("main")).toBeTruthy();
});

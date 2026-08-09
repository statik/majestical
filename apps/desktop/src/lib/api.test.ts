import { expect, test } from "vitest";
import { errorNotices } from "./api";

test("an object carrying valid string notices returns them", () => {
  expect(errorNotices({ message: "boom", notices: ["warned"] })).toEqual([
    "warned",
  ]);
});

test("an object with no notices field returns an empty array", () => {
  expect(errorNotices({ message: "boom" })).toEqual([]);
});

test("a non-array notices field returns an empty array", () => {
  expect(errorNotices({ message: "boom", notices: "warned" })).toEqual([]);
});

test("an array mixing strings with other types returns an empty array", () => {
  expect(errorNotices({ message: "boom", notices: ["warned", 1] })).toEqual(
    [],
  );
});

test("a non-object value returns an empty array", () => {
  expect(errorNotices("boom")).toEqual([]);
  expect(errorNotices(null)).toEqual([]);
});
